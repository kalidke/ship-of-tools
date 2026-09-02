#![cfg(windows)]
//! ADR 0041 step 6 U3: the FE attach-only client's Windows-only RUNTIME —
//! wires `fe_client`'s pure state machines (the six FE rulings) to a real
//! `PipeClient`, a real supervisor lane, and the drawer's own
//! `vt100_ctt::Parser`. Windows-only because everything it connects to
//! (the capsule, the supervisor, `pipe_win`, `challenge`) is Windows-only
//! — the client code itself is otherwise the SAME kind of thin I/O
//! wrapper `term::LocalTerminal` already is over its own PTY: a
//! background reader thread forwards bytes/frames, the caller drains
//! them non-blockingly via `pump()`.
//!
//! **The flag.** Behind `drawer.attach_only` (an FE settings key, off by
//! default — read in `rust/frontend/src/settings.rs`; this crate has no
//! settings mechanism of its own and does not read it). When off,
//! nothing in the frontend calls anything in this module at all — the
//! Terminal drawer keeps spawning `term::LocalTerminal` exactly as
//! today. See `docs/adr/0041-fe-local-capsules-windows.md`'s "Step 6
//! units" U3 line and its as-built note below.
//!
//! **No DSR responder here, by design.** The capsule's own ConPTY DSR
//! responder (step 4) already answers every device-status query before a
//! byte of it ever reaches this client (Terminal state: "The ConPTY DSR
//! responder runs from producer spawn with zero clients attached").
//! `term::LocalTerminal::respond_to_queries` is untouched — it stays the
//! off-flag path's own responder — but this module never ports a copy of
//! it, which is what "Step 6 units" calls "deletion of its DSR
//! responder": absent from the attach-only path by construction, not a
//! runtime toggle.
//!
//! # Architecture
//!
//! One background WORKER thread (spawned by [`FeAttachClient::attach`])
//! owns the entire reconnect-classified episode loop (ruling (d)) and
//! every blocking pipe call; [`FeAttachClient`] itself never blocks.
//! Foreground → worker is [`WorkerMsg::Input`]/`Resize`/`Quit`/`Shutdown`;
//! worker → foreground is [`ClientEvent`], drained by
//! [`FeAttachClient::pump`]. While attached, a second, EPISODE-SCOPED
//! reader thread decodes the attach connection's incoming bytes and
//! forwards frames back into the SAME worker channel (mirroring
//! `term::LocalTerminal`'s own reader-thread/mpsc shape, and
//! `tests/e2e_pipe.rs`'s `RealFrames` harness) — folded into one channel
//! so the worker services commands and unsolicited output with a single
//! `recv_timeout` loop rather than a hand-rolled select.
//!
//! # Codex review round (the ONE review round for this PR) — what changed
//!
//! The first landing's runtime wiring violated all six rulings in
//! concrete, reproducible ways; every fix below is cited at its own site
//! by finding number. Summary, so the shape of the redesign reads as one
//! story rather than fourteen unrelated patches:
//! - Quit (finding 1, 2): the cutoff is the ADR's own pinned 90s
//!   bound-graph figure (`fe_client::QUIT_CUTOFF`); after `record_closed`
//!   the worker polls `query` until `record_verified`; a `Quit` message
//!   arriving during reconnect backoff is LATCHED, not dropped, and
//!   applied the moment the supervisor lane reconnects (`end_run` needs
//!   only that lane, never the attach lane).
//! - Take/input (finding 3, 4, 5, 6): `take_ok` sends only `resize`; the
//!   queue flushes only after `resize_ok`; `take_refused{not_attached}`
//!   ends the episode PRESERVING the take transaction; driving-mode
//!   input while one is outstanding is queued (reusing the take queue),
//!   never dropped; a reconnect resends the retained `(voyage, key,
//!   epoch, bytes)` tuple under the SAME key once re-taken; a stale
//!   refusal re-takes before minting a new key.
//! - Backpressure (finding 7): `queued_bytes` is a SINGLE `Arc<AtomicUsize>`
//!   shared between the episode reader (increments, blocks the pipe read
//!   when full) and `FeAttachClient::pump` (decrements on consumption,
//!   caps bytes drained per call).
//! - Health window (finding 8): the timer only advances when the voyage
//!   pipe is ALSO absent, checked via `on_supervisor_absent_or_unresponsive`;
//!   access-denied on either pipe is terminal immediately
//!   (`ReconnectState::classify_access_denied`, now wired).
//! - Attach notice (finding 9): the capsule's own identity comes from a
//!   THROWAWAY voyage mgmt-lane challenge (`capsule_identity_via_mgmt`),
//!   never the supervisor's own `status_ok` (which reports the
//!   SUPERVISOR process, not the leg).
//! - `fe_down` (finding 10): markers land in a small foreground
//!   `VecDeque` `pump` itself appends to, never a second, racing drain of
//!   the same channel; the baseline is captured by the caller at FE
//!   process start (`gpu.rs`'s `State::new`), not at first drawer open.
//! - Visible outcomes (finding 11) and flag-off diagnostics (finding 12)
//!   are the frontend's own fixes (`gpu.rs`); reader-thread spawn
//!   failure (finding 13) is a visible terminal error here, never a
//!   silent "attached".

use crate::challenge::{self, ChallengeOutcome, ChallengedProcess};
use crate::exchange::{SupervisorLaneExchange, VoyageMgmtExchange, SUPERVISOR_LANE_BUILD_ID};
use crate::fe_client::{
    self, FeDownBaseline, InputWireOutcome, OutstandingSlot, QuitDispatcher,
    ReconnectDecision, ReconnectState, Role, TakeAction, TakeTransaction,
};
use crate::pipe_win::{self, PipeClient};
use crate::pointer::{self, PointerState};
use crate::supervisor::state_dir_hash;
use crate::wire::{
    self, AttachClient, AttachServer, DecodedFrame, ResizeRefusedReason, SupervisorOp,
    SupervisorPhase, SupervisorReply, SupervisorRequest, TakeRefusedReason,
};
use std::collections::VecDeque;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// `hello`'s own reply budget (ADR 0041 Lifecycle "Every op has one
/// budget: connect 2 s, request write 2 s..." — hello is a fixed,
/// single-round-trip request, so it shares the 2 s figure rather than
/// the slower `status` budget below).
const HELLO_BUDGET: Duration = Duration::from_secs(2);
/// "Every client's first act, after the identity check above, is a
/// `status` with a 5 s budget; a lane that accepts but does not answer
/// within it is treated exactly as an absent lane."
const STATUS_BUDGET: Duration = Duration::from_secs(5);
/// Ordinary lane-operation write budget (connect/write halves of the
/// Lifecycle "one budget" triple; the read half is `STATUS_BUDGET` or,
/// for `command`/`query`, the same 5 s figure).
const WRITE_BUDGET: Duration = Duration::from_secs(2);
/// How often the worker polls the supervisor lane for `status` during
/// steady state — a still-answering authority's phase transition
/// (ruling (d)) must become visible promptly, but polling faster than
/// this buys nothing beyond load.
const LIVENESS_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// How often the worker polls `query` while a quit is `Verifying`
/// (Codex review round, finding 1: "after record_closed, query the
/// operation until record_verified"). `query` is a fast stateless read
/// (Lifecycle: "the FE never blocks on an O(history) walk") even though
/// what it reports on can take a while server-side, so polling this
/// much faster than the liveness cadence costs little and keeps the
/// "ending session…" window responsive.
const QUIT_QUERY_INTERVAL: Duration = Duration::from_secs(1);
/// The worker's own message-loop tick — bounds how promptly a command
/// (input/resize/quit) is serviced and how often the pure tick-driven
/// timers (quit cutoff, checkpoint-in-flight retry, backoff) advance.
const WORKER_TICK: Duration = Duration::from_millis(100);
/// Ruling (d): "The reader's unbounded channel becomes BYTE-ACCOUNTED
/// and bounded at 4 MiB — bytes, not items... When it is full the FE
/// STOPS READING THE PIPE." (Codex review round, finding 7: the first
/// landing's counter was local to the reader thread and released
/// immediately, never actually shared with the consumer — see
/// [`FeAttachClient::pump`]'s own doc for the real, shared half.)
const READER_QUEUE_CAP_BYTES: usize = 4 * 1024 * 1024;
/// How many bytes of `Output` one `pump()` call drains before returning
/// — Codex review round, finding 7: "cap bytes drained per pump while
/// requesting another redraw if more remain." `pump()` returning `true`
/// already makes every existing caller request a redraw (see
/// `gpu.rs::pump_attach_term`), so capping here and relying on that same
/// "changed -> redraw -> pump again" cycle needs no new plumbing — a
/// continuous-output flood drains in bounded slices across several
/// frames instead of stalling one. Comfortably below the 4 MiB reader
/// cap so a single pump() call can never itself observe the reader
/// having stalled.
const PUMP_DRAIN_CAP_BYTES: usize = 1024 * 1024;
/// Local, FE-side scrollback depth for the restored screen — a UI
/// parameter, not protocol-defined (the capsule itself keeps none; see
/// ADR 0041 "Terminal state"). Matches `term::LocalTerminal`'s own value
/// for parity between the two drawer backends.
const SCROLLBACK_ROWS: usize = 5000;

// -----------------------------------------------------------------------
// Small bounded I/O helpers over PipeClient, shared by every lane this
// module speaks (mirrors the pattern `challenge::challenge` itself uses:
// `crate::deadline::run_with_deadline` racing the blocking call against a
// `cancel()`-issuing watchdog).
// -----------------------------------------------------------------------

#[derive(Debug)]
enum LaneError {
    Io(std::io::Error),
    Timeout,
    Eof,
    Wire(wire::WireError),
    Protocol(&'static str),
}

impl std::fmt::Display for LaneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LaneError::Io(e) => write!(f, "io: {e}"),
            LaneError::Timeout => write!(f, "timed out"),
            LaneError::Eof => write!(f, "connection closed"),
            LaneError::Wire(e) => write!(f, "wire: {e}"),
            LaneError::Protocol(s) => write!(f, "protocol: {s}"),
        }
    }
}

fn is_access_denied(e: &std::io::Error) -> bool {
    // ERROR_ACCESS_DENIED == 5.
    e.raw_os_error() == Some(5) || e.kind() == ErrorKind::PermissionDenied
}

fn write_bounded(conn: &PipeClient, bytes: &[u8], deadline: Instant) -> Result<(), LaneError> {
    match crate::deadline::run_with_deadline(deadline, || conn.cancel(), || conn.write_all(bytes)) {
        Some(Ok(())) => Ok(()),
        Some(Err(e)) => Err(LaneError::Io(pipe_err_to_io(e))),
        None => Err(LaneError::Timeout),
    }
}

fn pipe_err_to_io(e: pipe_win::PipeError) -> std::io::Error {
    match e {
        pipe_win::PipeError::Io { source, .. } => source,
        other => std::io::Error::other(other.to_string()),
    }
}

/// A connection plus its own `FrameSplitter` and a small pending queue,
/// so a bounded read never silently drops a SECOND frame that happened
/// to decode from the same underlying `read()` — unlike
/// `exchange::VoyageMgmtExchange`/`SupervisorLaneExchange` (whose
/// one-shot identity exchange treats a bundled second frame as
/// corruption, correctly, since THEIR protocol is exactly one round
/// trip), the mgmt/supervisor lane and the attach lane both keep being
/// used afterward, so a bundled extra frame here is ordinary traffic
/// that must be preserved for the caller's NEXT read.
struct FrameReader {
    splitter: wire::FrameSplitter,
    pending: VecDeque<DecodedFrame>,
}

impl FrameReader {
    fn new() -> Self {
        Self { splitter: wire::FrameSplitter::new(), pending: VecDeque::new() }
    }

    fn next_frame(&mut self, conn: &PipeClient, deadline: Instant) -> Result<DecodedFrame, LaneError> {
        if let Some(f) = self.pending.pop_front() {
            return Ok(f);
        }
        loop {
            let mut buf = [0u8; 8192];
            let n = match crate::deadline::run_with_deadline(deadline, || conn.cancel(), || conn.read(&mut buf)) {
                Some(Ok(n)) => n,
                Some(Err(e)) => return Err(LaneError::Io(pipe_err_to_io(e))),
                None => return Err(LaneError::Timeout),
            };
            if n == 0 {
                return Err(LaneError::Eof);
            }
            let (frames, err) = self.splitter.feed(&buf[..n]);
            self.pending.extend(frames);
            if let Some(e) = err {
                return Err(LaneError::Wire(e));
            }
            if let Some(f) = self.pending.pop_front() {
                return Ok(f);
            }
        }
    }
}

// -----------------------------------------------------------------------
// The supervisor lane: connect + hello (build identity) + status.
// -----------------------------------------------------------------------

/// Connect the supervisor lane and run the full same-connection
/// challenge with this crate's own build identity — the production
/// analog of `supervisor::connect_and_challenge_for_test` (test-support
/// only), reusing the SAME primitives (`connect_supervisor_pipe_
/// unchallenged`, `challenge::challenge`, `SupervisorLaneExchange`)
/// rather than depending on that test-gated helper.
fn connect_supervisor_lane(h: &str) -> Result<(PipeClient, ChallengedProcess), LaneError> {
    let conn = pipe_win::connect_supervisor_pipe_unchallenged(h).map_err(|e| LaneError::Io(pipe_err_to_io(e)))?;
    let mut exchange = SupervisorLaneExchange::new(SUPERVISOR_LANE_BUILD_ID);
    let deadline = Instant::now() + HELLO_BUDGET;
    match challenge::challenge(&conn, &mut exchange, deadline) {
        ChallengeOutcome::Proven(process) => Ok((conn, process)),
        // The shared challenge machinery folds "SID mismatch" and "a
        // well-formed WRONG reply" (which includes a genuine
        // `hello_refused{version_skew}` from an otherwise legitimate,
        // same-account peer) into the SAME `Foreign` outcome — see this
        // module's own doc and the report's "Deviations" for why
        // disambiguating them would need new machinery this unit
        // prefers not to add. Either way it is an unproven server:
        // never retried as if it might still be legitimate.
        ChallengeOutcome::Foreign => Err(LaneError::Protocol("supervisor hello: foreign")),
        ChallengeOutcome::Undetermined => Err(LaneError::Protocol("supervisor hello: undetermined")),
    }
}

fn supervisor_status(
    conn: &PipeClient,
    reader: &mut FrameReader,
) -> Result<(Option<String>, Option<u64>, SupervisorPhase), LaneError> {
    let bytes = wire::encode_supervisor_request(&SupervisorRequest::Status)
        .expect("Status has no fields; encoding cannot fail");
    write_bounded(conn, &bytes, Instant::now() + WRITE_BUDGET)?;
    match reader.next_frame(conn, Instant::now() + STATUS_BUDGET)? {
        DecodedFrame::SupervisorReply(SupervisorReply::StatusOk { voyage, leg, phase, .. }) => {
            Ok((voyage, leg, phase))
        }
        _ => Err(LaneError::Protocol("expected status_ok")),
    }
}

/// Ruling (d), Codex review round finding 8: called whenever the
/// supervisor lane looks absent OR unresponsive THIS round. Applies the
/// AND condition directly — the health-window timer only advances when
/// the voyage pipe is ALSO absent, checked here via a throwaway connect
/// probe (successful connect is evidence enough of presence; no need to
/// run the full challenge just to answer "does anything answer this
/// name"). A live voyage pipe means the capsule survives headless
/// (exactly the scenario ADR 0041 P3 is built to tolerate), so this
/// clears the clock and asks the caller to retry shortly rather than
/// attaching blind this round — the caller's own backoff (250 ms
/// doubling to 4 s) makes that a brief, bounded gap, not a stall.
fn on_supervisor_absent_or_unresponsive(
    reconnect: &mut ReconnectState,
    voyage: &str,
    now: Instant,
) -> ReconnectDecision {
    match pipe_win::connect_voyage_pipe_unchallenged(voyage) {
        Ok(_probe) => {
            reconnect.clear_unresponsive();
            ReconnectDecision::Retry
        }
        Err(e) => {
            if is_access_denied(&pipe_err_to_io(e)) {
                reconnect.classify_access_denied()
            } else {
                reconnect.classify_unresponsive(now)
            }
        }
    }
}

/// Ruling (e), Codex review round finding 9: the CAPSULE'S OWN identity,
/// proven via a THROWAWAY connection to the voyage pipe's mgmt sub-lane
/// (`probe`/`status`/`shutdown` — the step-5 lane, distinct from the
/// attach lane) and the full same-connection challenge
/// (`VoyageMgmtExchange`, already built for exactly this: "the voyage
/// mgmt lane's own `IdentityExchange`"). The merged U2 supervisor lane's
/// own `status_ok.pid`/`.created` report the SUPERVISOR process itself
/// (`supervisor.rs`'s own doc: "`pid`/`created` are this process's own
/// identity"), never the leg, so that reply can never stand in for this.
fn capsule_identity_via_mgmt(voyage: &str) -> Result<ChallengedProcess, LaneError> {
    let conn = pipe_win::connect_voyage_pipe_unchallenged(voyage).map_err(|e| LaneError::Io(pipe_err_to_io(e)))?;
    let mut exchange = VoyageMgmtExchange::default();
    let deadline = Instant::now() + STATUS_BUDGET;
    match challenge::challenge(&conn, &mut exchange, deadline) {
        ChallengeOutcome::Proven(process) => Ok(process),
        ChallengeOutcome::Foreign => Err(LaneError::Protocol("voyage mgmt: foreign")),
        ChallengeOutcome::Undetermined => Err(LaneError::Protocol("voyage mgmt: undetermined")),
    }
}

// -----------------------------------------------------------------------
// The attach lane: hello (proto only) + attach + checkpoint reassembly.
// -----------------------------------------------------------------------

fn attach_lane_hello(conn: &PipeClient, reader: &mut FrameReader) -> Result<(), LaneError> {
    let bytes = wire::encode_attach_client(&AttachClient::Hello { proto: wire::ATTACH_PROTO_V1 })
        .expect("fixed hello shape");
    write_bounded(conn, &bytes, Instant::now() + WRITE_BUDGET)?;
    match reader.next_frame(conn, Instant::now() + HELLO_BUDGET)? {
        DecodedFrame::AttachServer(AttachServer::HelloOk { .. }) => Ok(()),
        DecodedFrame::AttachServer(AttachServer::HelloRefused { .. }) => {
            Err(LaneError::Protocol("attach hello: version_skew"))
        }
        _ => Err(LaneError::Protocol("expected attach hello_ok")),
    }
}

/// Sends `attach{controller_id}` (always arrives as a WATCHER — ADR
/// 0037's who-may-type) and reassembles the checkpoint transfer, bounded
/// at [`wire::MAX_CHECKPOINT_LEN`] the same way `tests/e2e_pipe.rs`'s own
/// `RealFrames::collect_checkpoint` proves the property.
fn attach_and_collect_checkpoint(
    conn: &PipeClient,
    reader: &mut FrameReader,
    controller_id: &str,
) -> Result<Vec<u8>, LaneError> {
    let bytes = wire::encode_attach_client(&AttachClient::Attach { controller_id: controller_id.to_string() })
        .map_err(LaneError::Wire)?;
    write_bounded(conn, &bytes, Instant::now() + WRITE_BUDGET)?;
    let mut out = Vec::new();
    loop {
        match reader.next_frame(conn, Instant::now() + STATUS_BUDGET)? {
            DecodedFrame::AttachServer(AttachServer::CheckpointChunk { last, bytes }) => {
                out.extend_from_slice(&bytes);
                if out.len() > wire::MAX_CHECKPOINT_LEN {
                    return Err(LaneError::Protocol("checkpoint exceeded MAX_CHECKPOINT_LEN"));
                }
                if last {
                    return Ok(out);
                }
            }
            DecodedFrame::AttachServer(AttachServer::AttachRefused { .. }) => {
                // GroundTimeout / SubscriberCap: transient, not named in
                // the ADR's terminal list -- the reconnect episode
                // simply retries (see `run_episode`'s caller).
                return Err(LaneError::Protocol("attach_refused"));
            }
            DecodedFrame::AttachServer(AttachServer::Output { .. }) => {
                return Err(LaneError::Protocol("live output arrived before checkpoint completed"));
            }
            _ => return Err(LaneError::Protocol("unexpected frame during checkpoint transfer")),
        }
    }
}

// -----------------------------------------------------------------------
// Worker <-> foreground messages
// -----------------------------------------------------------------------

enum WorkerMsg {
    Input(Vec<u8>),
    Resize(u16, u16),
    Quit(String),
    Shutdown,
    Frame(DecodedFrame),
    ReaderDone,
}

/// What the worker reports to the foreground. `pump()` applies each of
/// these to the parser/UI state.
enum ClientEvent {
    Checkpoint(Vec<u8>),
    Output(Vec<u8>),
    Notice(String),
    Status(String),
    Terminal(String),
    QuitMessage(Option<String>),
    ShouldExit,
    FeDownMarker(serde_json::Value),
}

// -----------------------------------------------------------------------
// Public surface
// -----------------------------------------------------------------------

/// Constructor/attach-time failures — everything AFTER a successful
/// [`FeAttachClient::attach`] is reported through [`FeAttachClient::pump`]
/// (status text / terminal notice), matching the ADR's "an actionable
/// error offering retry and reset" rather than a plain `Result` deep
/// inside a long-running reconnect loop. Resolving `state_dir` itself is
/// the CALLER's job (see `attach`'s own doc), so there is no
/// "no state dir" variant here — that failure is the caller's to name.
#[derive(Debug)]
pub enum FeAttachError {
    SpawnWorkerThread(std::io::Error),
}

impl std::fmt::Display for FeAttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FeAttachError::SpawnWorkerThread(e) => write!(f, "spawn fe-client worker thread: {e}"),
        }
    }
}

/// The attach-only drawer backend — the same consumer-facing shape as
/// `term::LocalTerminal` (`pump`/`screen`/`send_input`/`resize`/
/// `is_dead`), plus the quit-dispatcher and fe_down surfaces
/// `LocalTerminal` has no analog for.
pub struct FeAttachClient {
    parser: vt100_ctt::Parser,
    msg_tx: Sender<WorkerMsg>,
    events_rx: Receiver<ClientEvent>,
    /// Codex review round, deletion candidate: the join handle used to be
    /// held for no reason a `JoinHandle`'s own `Drop` does not already
    /// give for free (dropping it neither joins nor detaches — Rust
    /// threads run detached from their handle either way). Not stored.
    /// Ruling (d)'s reader/worker teardown is unaffected: the WORKER
    /// thread's own loop exits on `Shutdown` or `Disconnected`
    /// regardless of whether anything outlives it holding the handle.
    status: String,
    notice: Option<String>,
    quit_message: Option<String>,
    should_exit: bool,
    dead: bool,
    /// Codex review round, finding 7: the SHARED half of the byte-account
    /// (the episode reader thread, spawned inside the worker, holds the
    /// other `Arc` clone and increments this on every `Output`/
    /// `CheckpointChunk` byte it reads, blocking further reads while at
    /// cap). `pump` decrements it as it actually consumes `Output` bytes
    /// — the ONLY place this counter is ever decremented, which is what
    /// makes the accounting real (the first landing incremented and
    /// immediately decremented in the SAME reader-thread call, which
    /// Codex review round correctly called a no-op).
    queued_bytes: Arc<AtomicUsize>,
    /// Codex review round, finding 10: markers `pump` receives land here
    /// (never re-drained from the same channel a second time, which
    /// silently ate whatever non-marker event happened to be next in
    /// line). `drain_fe_down_markers` drains ONLY this queue.
    pending_fe_down_markers: VecDeque<serde_json::Value>,
}

impl FeAttachClient {
    /// Reads `drawer.voyage` under `state_dir` and starts the background
    /// worker; the worker itself performs the connect/hello/status/
    /// attach/checkpoint sequence and every reconnect thereafter — this
    /// constructor never blocks on the network, matching
    /// `LocalTerminal::spawn`'s own "returns once the reader thread is
    /// running" contract. `state_dir` is the CALLER's resolved value
    /// (`state_dir::sot_state_dir()` for the real frontend; an isolated
    /// tempdir for `tests/fe_client_win.rs`) — this constructor takes it
    /// rather than resolving it itself, the same way `sot-capsule
    /// supervise <state_dir>` takes it as an explicit argument rather
    /// than an internal env-var lookup, so a real client and a test can
    /// point at different trees in the same process without racing a
    /// shared env var. `fe_down_last_evidence` is likewise the CALLER's
    /// own read of `fe-inbox.jsonl`, taken at FE PROCESS START (Codex
    /// review round, finding 10) — this constructor never reads that
    /// file itself, so a drawer opened long after startup still reports
    /// the SAME baseline the process began with.
    pub fn attach(
        state_dir: PathBuf,
        cols: u16,
        rows: u16,
        controller_id: String,
        fe_down_to_handle: String,
        fe_down_last_evidence: Option<String>,
        wake: Box<dyn Fn() + Send + 'static>,
    ) -> Result<Self, FeAttachError> {
        let rows = rows.max(2);
        let cols = cols.max(2);
        let parser = vt100_ctt::Parser::new(rows, cols, SCROLLBACK_ROWS);

        let (msg_tx, msg_rx) = mpsc::channel::<WorkerMsg>();
        let (events_tx, events_rx) = mpsc::channel::<ClientEvent>();
        let worker_msg_tx = msg_tx.clone();
        let fe_down = FeDownBaseline::capture(fe_down_last_evidence);
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        let worker_queued_bytes = Arc::clone(&queued_bytes);

        thread::Builder::new()
            .name("sot-fe-attach-worker".to_string())
            .spawn(move || {
                run_worker(
                    state_dir,
                    controller_id,
                    fe_down_to_handle,
                    fe_down,
                    cols,
                    rows,
                    msg_rx,
                    worker_msg_tx,
                    events_tx,
                    worker_queued_bytes,
                    wake,
                );
            })
            .map_err(FeAttachError::SpawnWorkerThread)?;

        Ok(Self {
            parser,
            msg_tx,
            events_rx,
            status: "connecting\u{2026}".to_string(),
            notice: None,
            quit_message: None,
            should_exit: false,
            dead: false,
            queued_bytes,
            pending_fe_down_markers: VecDeque::new(),
        })
    }

    /// Drains pending events into the parser/UI state (non-blocking).
    /// Returns `true` iff anything changed — the caller schedules a
    /// repaint, matching `LocalTerminal::pump`'s own contract. Caps
    /// `Output` bytes drained per call at [`PUMP_DRAIN_CAP_BYTES`] (Codex
    /// review round, finding 7): a continuous-output flood is drained in
    /// bounded slices across several frames rather than stalling
    /// rendering for one unbounded call — the caller already requests
    /// another redraw whenever this returns `true`, which is what brings
    /// `pump` back for the rest.
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        let mut drained_output_bytes = 0usize;
        loop {
            if drained_output_bytes >= PUMP_DRAIN_CAP_BYTES {
                break;
            }
            match self.events_rx.try_recv() {
                Ok(ClientEvent::Checkpoint(bytes)) => {
                    if let Err(e) = self.parser.restore_screen(&bytes) {
                        self.status = format!("checkpoint restore failed: {e:?}");
                    }
                    changed = true;
                }
                Ok(ClientEvent::Output(bytes)) => {
                    drained_output_bytes += bytes.len();
                    // The ONLY decrement of the shared byte-account — see
                    // this struct's own `queued_bytes` doc.
                    self.queued_bytes.fetch_sub(bytes.len(), Ordering::AcqRel);
                    self.parser.process(&bytes);
                    changed = true;
                }
                Ok(ClientEvent::Notice(text)) => {
                    self.notice = Some(text);
                    changed = true;
                }
                Ok(ClientEvent::Status(text)) => {
                    self.status = text;
                    changed = true;
                }
                Ok(ClientEvent::Terminal(text)) => {
                    self.status = text;
                    self.dead = true;
                    changed = true;
                }
                Ok(ClientEvent::QuitMessage(msg)) => {
                    self.quit_message = msg;
                    changed = true;
                }
                Ok(ClientEvent::ShouldExit) => {
                    self.should_exit = true;
                    changed = true;
                }
                Ok(ClientEvent::FeDownMarker(v)) => {
                    // Codex review round, finding 10: land it in the
                    // dedicated queue rather than discarding the payload
                    // — `drain_fe_down_markers` reads ONLY this queue,
                    // never the channel again, so no other event can be
                    // swallowed alongside it.
                    self.pending_fe_down_markers.push_back(v);
                    changed = true;
                }
                Err(_) => break,
            }
        }
        changed
    }

    /// Any `fe_down` markers `pump` received since the last drain — the
    /// caller (the frontend) appends each to `fe-inbox.jsonl` and must
    /// surface a VISIBLE failure if the append itself fails ("a marker
    /// that exists so a failure is not quiet cannot fail quietly
    /// itself"). Call AFTER `pump`, which is what actually populates the
    /// queue this drains.
    pub fn drain_fe_down_markers(&mut self) -> Vec<serde_json::Value> {
        self.pending_fe_down_markers.drain(..).collect()
    }

    pub fn screen(&self) -> &vt100_ctt::Screen {
        self.parser.screen()
    }

    pub fn screen_mut(&mut self) -> &mut vt100_ctt::Screen {
        self.parser.screen_mut()
    }

    pub fn mouse_tracking_on(&self) -> bool {
        !matches!(self.parser.screen().mouse_protocol_mode(), vt100_ctt::MouseProtocolMode::None)
    }

    /// Forwards keystroke bytes to the worker, which drives the
    /// take-on-first-input transaction (ruling (b)).
    pub fn send_input(&mut self, bytes: &[u8]) {
        let _ = self.msg_tx.send(WorkerMsg::Input(bytes.to_vec()));
    }

    /// Records the desired viewport. A WATCHER cannot correct the
    /// geometry until it holds the pen (ruling (b)) — the worker applies
    /// this only once `take_ok` grants the pen (via its OWN `resize`,
    /// awaited alone — see `fe_client::TakeTransaction::on_take_ok`), or
    /// immediately (as an ordinary `resize` request) while already
    /// DRIVING.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let rows = rows.max(2);
        let cols = cols.max(2);
        let _ = self.msg_tx.send(WorkerMsg::Resize(cols, rows));
    }

    /// `true` once the reconnect episode has reached a TERMINAL
    /// classification (ruling (d)) — the drawer shows the terminal
    /// notice rather than a blank pane.
    pub fn is_dead(&mut self) -> bool {
        self.dead
    }

    /// Ruling (a): the ONE quit dispatcher. Idempotent — a second call
    /// while already ending does nothing (the worker's own
    /// `QuitDispatcher` enforces this). Never lost across a reconnect in
    /// flight (Codex review round, finding 2) — the worker LATCHES this
    /// message rather than dropping it if a reconnect backoff is
    /// currently in progress.
    pub fn request_quit(&mut self, reason: &str) {
        let _ = self.msg_tx.send(WorkerMsg::Quit(reason.to_string()));
    }

    /// `Some("ending session…")` / `Some("verifying…")` / `Some("...
    /// outcome unknown")` / `Some("...failed: ...")` /
    /// `Some("...refused: ...")` while a quit is in flight, verifying,
    /// or reached a terminal outcome; `None` otherwise.
    pub fn quit_message(&self) -> Option<&str> {
        self.quit_message.as_deref()
    }

    /// `true` once `record_verified` arrived (ADR Lifecycle: "the
    /// COMMAND reply arrives at record_closed, and record_verified
    /// follows through query") — the caller may now call
    /// `event_loop.exit()`.
    pub fn should_exit(&self) -> bool {
        self.should_exit
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub fn status_line(&self) -> &str {
        &self.status
    }
}

impl Drop for FeAttachClient {
    fn drop(&mut self) {
        let _ = self.msg_tx.send(WorkerMsg::Shutdown);
    }
}

// -----------------------------------------------------------------------
// The worker thread
// -----------------------------------------------------------------------

/// What triggered the take-on-first-input `take` currently in flight (or
/// about to be), so the post-`take_ok`/`resize_ok` flush knows what to
/// send once it is safe to. `Ordinary` covers the common case (a real
/// keystroke); the other two exist ONLY to correctly discharge ruling
/// (c)'s exactly-once contract (Codex review round, finding 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TakeIntent {
    /// Flush whatever `TakeTransaction::take_queued` returns, minting a
    /// FRESH idem key.
    Ordinary,
    /// Resend the retained `OutstandingInput` under the SAME key, once
    /// the fresh epoch is known (ruling (c): "resends THE SAME KEY").
    ReconnectResend,
    /// `input_refused_stale`'s own retry: mint a NEW key under the fresh
    /// epoch now that it is known (ruling (c): "re-sent under the new
    /// epoch with a NEW key").
    StaleRetry,
}

#[allow(clippy::too_many_arguments)]
fn run_worker(
    state_dir: PathBuf,
    controller_id: String,
    fe_down_to_handle: String,
    mut fe_down: FeDownBaseline,
    initial_cols: u16,
    initial_rows: u16,
    cmd_rx: Receiver<WorkerMsg>,
    msg_tx: Sender<WorkerMsg>,
    events_tx: Sender<ClientEvent>,
    queued_bytes: Arc<AtomicUsize>,
    wake: Box<dyn Fn() + Send + 'static>,
) {
    let h = state_dir_hash(&state_dir);
    let mut reconnect = ReconnectState::new();
    let mut take = TakeTransaction::new();
    let mut outstanding = OutstandingSlot::new();
    let mut quit = QuitDispatcher::new();
    let mut take_intent = TakeIntent::Ordinary;
    let mut cols = initial_cols;
    let mut rows = initial_rows;
    let mut voyage_uuid: Option<String> = None;
    let mut take_epoch: u64 = 0;
    let mut shutdown = false;
    // Ruling (b), Codex review round finding 4: set when
    // `take_refused{not_attached}` fires, so the NEXT episode's own
    // arrival at a fresh checkpoint knows to `retry_take()` (preserving
    // role+queue) instead of `reset_to_watching()`.
    let mut preserve_take_on_reconnect = false;
    // Ruling (a), Codex review round finding 2: a `Quit` requested
    // while no supervisor connection is currently open (mid-backoff, or
    // before the first one ever connects) is LATCHED here rather than
    // dropped — applied the instant a fresh supervisor connection
    // exists, since `end_run` needs only that lane, never the attach
    // lane.
    let mut latched_quit_reason: Option<String> = None;

    let emit = |e: ClientEvent| {
        let _ = events_tx.send(e);
        wake();
    };

    'episodes: while !shutdown {
        emit(ClientEvent::Status("connecting\u{2026}".to_string()));

        // Re-read and re-validate the pointer at the START of every
        // episode (ruling (d)) — never against a cached UUID.
        let voyage = match pointer::validate(&state_dir) {
            PointerState::Valid(id) => id,
            PointerState::NotFound | PointerState::Corrupt | PointerState::OtherIo(_) => {
                emit(ClientEvent::Terminal(
                    "drawer.voyage is absent or corrupt \u{2014} retry or reset".to_string(),
                ));
                return;
            }
        };
        if voyage_uuid.as_deref() != Some(voyage.as_str()) {
            // A reset landed underneath us: any outstanding input from
            // the OLD voyage is canceled, never replayed into the new
            // one -- and reported, never silently (finding 6).
            if let fe_client::ReconnectResendDecision::Cancel { canceled } =
                outstanding.resend_after_reconnect(&voyage, take_epoch)
            {
                emit(ClientEvent::Status(format!(
                    "input canceled \u{2014} the voyage changed ({} byte(s) lost)",
                    canceled.bytes.len()
                )));
            }
            take.reset_to_watching();
            preserve_take_on_reconnect = false;
            take_intent = TakeIntent::Ordinary;
            voyage_uuid = Some(voyage.clone());
        }

        // --- supervisor lane: hello (build identity) then status -----
        let supervisor_connected = match connect_supervisor_lane(&h) {
            Ok((conn, _proven)) => {
                let mut sup_reader = FrameReader::new();
                match supervisor_status(&conn, &mut sup_reader) {
                    Ok((sv, _leg, phase)) => {
                        if let ReconnectDecision::Terminal(reason) = reconnect.classify_supervisor_phase(phase) {
                            emit(ClientEvent::Terminal(format!("supervisor: {reason:?}")));
                            return;
                        }
                        if sv.as_deref() == Some(voyage.as_str()) || sv.is_none() {
                            Some((conn, sup_reader))
                        } else {
                            // The pointer moved again between our read
                            // and the authority's own -- treated as
                            // "not yet usable this round"; the top of
                            // the NEXT episode re-reads the pointer
                            // fresh and reconciles.
                            None
                        }
                    }
                    Err(_) => None,
                }
            }
            Err(LaneError::Protocol(p)) if p.contains("foreign") => {
                match reconnect.classify_foreign() {
                    ReconnectDecision::Terminal(reason) => {
                        emit(ClientEvent::Terminal(format!("supervisor lane: {reason:?}")));
                        return;
                    }
                    ReconnectDecision::Retry => unreachable!("classify_foreign is always terminal"),
                }
            }
            Err(LaneError::Io(e)) if is_access_denied(&e) => {
                emit(ClientEvent::Terminal("supervisor lane: access denied".to_string()));
                return;
            }
            Err(_) => None,
        };

        // Ruling (d), Codex review round finding 8: only when the
        // supervisor lane is ALSO absent/unresponsive this round does the
        // health window even get consulted -- a reachable voyage pipe
        // (the capsule surviving headless) clears it unconditionally.
        if supervisor_connected.is_none() {
            match on_supervisor_absent_or_unresponsive(&mut reconnect, &voyage, Instant::now()) {
                ReconnectDecision::Terminal(reason) => {
                    emit(ClientEvent::Terminal(format!("supervisor lane unreachable: {reason:?}")));
                    return;
                }
                ReconnectDecision::Retry => {
                    emit(ClientEvent::Status("supervisor lane not answering \u{2014} retrying\u{2026}".to_string()));
                    match wait_for_retry_or_shutdown(&cmd_rx, reconnect.retry_with_backoff(), &mut latched_quit_reason) {
                        WaitOutcome::Shutdown => break 'episodes,
                        WaitOutcome::Continue => continue 'episodes,
                    }
                }
            }
        }
        let (supervisor_conn, mut sup_reader) = supervisor_connected.expect("checked Some above");

        // A latched quit only needs the supervisor lane -- apply it now,
        // rather than waiting for a full attach that a quit makes moot.
        if let Some(reason) = latched_quit_reason.take() {
            run_quit(&supervisor_conn, &mut sup_reader, &voyage, reason, &mut quit, &mut outstanding, &emit);
            if quit.should_exit() {
                emit(ClientEvent::ShouldExit);
                return;
            }
        }

        // --- attach lane: SID auth, hello, attach, checkpoint ---------
        let voyage_conn = match pipe_win::connect_voyage_pipe_unchallenged(&voyage) {
            Ok(c) => c,
            Err(e) => {
                let io = pipe_err_to_io(e);
                if is_access_denied(&io) {
                    emit(ClientEvent::Terminal("voyage pipe: access denied".to_string()));
                    return;
                }
                emit(ClientEvent::Status(format!("voyage pipe unreachable: {io}")));
                match wait_for_retry_or_shutdown(&cmd_rx, reconnect.retry_with_backoff(), &mut latched_quit_reason) {
                    WaitOutcome::Shutdown => break 'episodes,
                    WaitOutcome::Continue => continue 'episodes,
                }
            }
        };
        let attach_identity = match challenge::authenticate_server(&voyage_conn) {
            challenge::SidAuthOutcome::Authenticated(a) => (a.pid, a.created),
            challenge::SidAuthOutcome::Foreign => {
                emit(ClientEvent::Terminal("voyage pipe: foreign".to_string()));
                return;
            }
            challenge::SidAuthOutcome::Undetermined => {
                match wait_for_retry_or_shutdown(&cmd_rx, reconnect.retry_with_backoff(), &mut latched_quit_reason) {
                    WaitOutcome::Shutdown => break 'episodes,
                    WaitOutcome::Continue => continue 'episodes,
                }
            }
        };

        let mut attach_reader = FrameReader::new();
        if let Err(e) = attach_lane_hello(&voyage_conn, &mut attach_reader) {
            match e {
                LaneError::Protocol(p) if p.contains("version_skew") => {
                    emit(ClientEvent::Terminal("attach hello: version_skew".to_string()));
                    return;
                }
                _ => {
                    match wait_for_retry_or_shutdown(&cmd_rx, reconnect.retry_with_backoff(), &mut latched_quit_reason) {
                        WaitOutcome::Shutdown => break 'episodes,
                        WaitOutcome::Continue => continue 'episodes,
                    }
                }
            }
        }
        let checkpoint =
            match attach_and_collect_checkpoint(&voyage_conn, &mut attach_reader, &controller_id) {
                Ok(c) => c,
                Err(_) => {
                    match wait_for_retry_or_shutdown(&cmd_rx, reconnect.retry_with_backoff(), &mut latched_quit_reason) {
                        WaitOutcome::Shutdown => break 'episodes,
                        WaitOutcome::Continue => continue 'episodes,
                    }
                }
            };

        // Ruling (e), Codex review round finding 9: the attach notice
        // compares the CAPSULE's own identity (a throwaway voyage
        // mgmt-lane challenge) against the attach connection's own
        // SID-proven identity -- never the supervisor's. On mismatch,
        // re-read the mgmt identity once and proceed without a notice
        // rather than looping forever.
        let mgmt_identity = capsule_identity_via_mgmt(&voyage)
            .ok()
            .map(|p| (p.pid(), p.created()))
            .or_else(|| capsule_identity_via_mgmt(&voyage).ok().map(|p| (p.pid(), p.created())));
        if let Some(mgmt_leg) = mgmt_identity {
            if fe_client::legs_match(mgmt_leg, attach_identity) {
                emit(ClientEvent::Notice(fe_client::attach_notice_text(&format!("{}", mgmt_leg.1))));
            }
        }

        emit(ClientEvent::Checkpoint(checkpoint));
        emit(ClientEvent::Status("attached".to_string()));
        reconnect.attached();

        // Ruling (b), Codex review round finding 4: a `not_attached`
        // reattach preserves the take transaction instead of resetting
        // it -- re-issue `take` for the SAME still-queued bytes now that
        // a fresh checkpoint has landed.
        if preserve_take_on_reconnect {
            preserve_take_on_reconnect = false;
            for action in take.retry_take() {
                apply_single_take_action(action, &voyage_conn, &controller_id, &emit);
            }
        } else {
            take.reset_to_watching();
        }

        // Ruling (f): fe_down marker on every attach after the first.
        let now_iso = iso_now();
        if let Some(marker) = fe_down.marker_for_attach(&fe_down_to_handle, &now_iso) {
            emit(ClientEvent::FeDownMarker(marker));
        }

        // Ruling (c), Codex review round finding 6: resume any input
        // left outstanding from a prior connection, within this same
        // voyage -- kick off the SAME take-on-first-input transaction
        // that a real keystroke would, so `resize` then the retained
        // frame flow through the identical lockstep-respecting path.
        match outstanding.resend_after_reconnect(&voyage, take_epoch) {
            fe_client::ReconnectResendDecision::Resend { .. } => {
                take_intent = TakeIntent::ReconnectResend;
                if take.role() == Role::Watching {
                    let actions = take.on_input_while_watching(&[]);
                    for action in actions {
                        apply_single_take_action(action, &voyage_conn, &controller_id, &emit);
                    }
                }
                // Else: role is already Taking from the preserved
                // not_attached retry above -- the same take_ok serves
                // both purposes.
            }
            fe_client::ReconnectResendDecision::Cancel { canceled } => {
                emit(ClientEvent::Status(format!(
                    "input canceled \u{2014} the voyage changed ({} byte(s) lost)",
                    canceled.bytes.len()
                )));
            }
            fe_client::ReconnectResendDecision::None => {}
        }

        // Spawn the episode-scoped reader thread for the attach
        // connection's steady-state stream.
        let shared_conn = Arc::new(voyage_conn);
        let reader_tx = msg_tx.clone();
        let reader_conn = Arc::clone(&shared_conn);
        let episode_stop = Arc::new(AtomicBool::new(false));
        let reader_stop = Arc::clone(&episode_stop);
        let reader_queued_bytes = Arc::clone(&queued_bytes);
        let reader_thread = match thread::Builder::new()
            .name("sot-fe-attach-reader".to_string())
            .spawn(move || run_attach_reader(reader_conn, attach_reader, reader_tx, reader_queued_bytes, reader_stop))
        {
            Ok(jh) => jh,
            Err(e) => {
                // Codex review round, finding 13: a reader that could
                // not even be spawned must never look "attached" -- no
                // thread exists to ever deliver TakeOk, output, or input
                // acknowledgements.
                emit(ClientEvent::Terminal(format!("failed to start the attach reader thread: {e}")));
                return;
            }
        };
        let mut supervisor_conn = supervisor_conn;
        let mut last_liveness_poll = Instant::now();

        // --- steady state ------------------------------------------
        let episode_result = run_steady_state(
            &cmd_rx,
            &events_tx,
            &wake,
            &shared_conn,
            &mut supervisor_conn,
            &mut sup_reader,
            &mut take,
            &mut take_intent,
            &mut outstanding,
            &mut quit,
            &mut reconnect,
            &mut cols,
            &mut rows,
            &mut take_epoch,
            &controller_id,
            &voyage,
            &mut last_liveness_poll,
        );

        // Tear down this episode's connections before deciding what's
        // next. The stop flag interrupts the reader's OWN backpressure
        // wait (a sleep loop, not a blocked read -- `cancel()` alone
        // cannot reach it); `cancel()` then unblocks a blocked read so
        // the thread observes an error, sends the now-moot `ReaderDone`
        // (harmlessly ignored; a fresh reader is not spawned until the
        // next successful attach), and exits; only then do both `Arc`
        // clones drop and the pipe handle actually closes.
        episode_stop.store(true, Ordering::Release);
        shared_conn.cancel();
        let _ = reader_thread.join();
        drop(shared_conn);
        drop(supervisor_conn);

        match episode_result {
            SteadyOutcome::Shutdown => {
                shutdown = true;
            }
            SteadyOutcome::QuitEnded => {
                emit(ClientEvent::ShouldExit);
                shutdown = true;
            }
            SteadyOutcome::Terminal(reason) => {
                emit(ClientEvent::Terminal(reason));
                return;
            }
            SteadyOutcome::Reconnect => {
                match wait_for_retry_or_shutdown(&cmd_rx, reconnect.retry_with_backoff(), &mut latched_quit_reason) {
                    WaitOutcome::Shutdown => shutdown = true,
                    WaitOutcome::Continue => {}
                }
            }
            SteadyOutcome::ReconnectPreserveTake => {
                preserve_take_on_reconnect = true;
                match wait_for_retry_or_shutdown(&cmd_rx, reconnect.retry_with_backoff(), &mut latched_quit_reason) {
                    WaitOutcome::Shutdown => shutdown = true,
                    WaitOutcome::Continue => {}
                }
            }
        }
    }
}

enum SteadyOutcome {
    Shutdown,
    QuitEnded,
    Terminal(String),
    /// An ordinary episode end -- the NEXT episode resets the take
    /// transaction to Watching.
    Reconnect,
    /// `take_refused{not_attached}` ended this episode -- the NEXT
    /// episode preserves the take transaction instead (ruling (b),
    /// Codex review round finding 4).
    ReconnectPreserveTake,
}

enum WaitOutcome {
    Continue,
    Shutdown,
}

/// Blocks up to `wait` for a `Shutdown` command, otherwise returns after
/// the backoff elapses so the next episode can start. A `Quit` arriving
/// during this wait is LATCHED into `*latched_quit_reason` rather than
/// dropped (Codex review round, finding 2) — the top of the next episode
/// applies it the moment a supervisor connection exists, since `end_run`
/// needs only that lane. `Input`/`Resize` arriving with no live
/// connection to send them on have nothing to act on yet and are
/// dropped (the take transaction and outstanding slot are not mutated
/// while disconnected, so a keystroke here would have nothing to attach
/// its intent to).
fn wait_for_retry_or_shutdown(
    cmd_rx: &Receiver<WorkerMsg>,
    wait: Duration,
    latched_quit_reason: &mut Option<String>,
) -> WaitOutcome {
    let deadline = Instant::now() + wait;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return WaitOutcome::Continue;
        }
        match cmd_rx.recv_timeout(remaining.min(WORKER_TICK)) {
            Ok(WorkerMsg::Shutdown) => return WaitOutcome::Shutdown,
            Ok(WorkerMsg::Quit(reason)) => {
                latched_quit_reason.get_or_insert(reason);
                continue;
            }
            Ok(_) => continue,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return WaitOutcome::Shutdown,
        }
    }
}

fn iso_now() -> String {
    // No chrono dependency in this crate; a plain RFC-3339-shaped UTC
    // stamp built from `SystemTime` is sufficient here since this string
    // is carried opaquely (fe_client::build_fe_down_marker never parses
    // it) and only ever compared/read by a human or a future durable
    // reader that already tolerates the daemon's own ISO-8601 strings.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let days = secs / 86400;
    let rem = secs % 86400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil-from-days (Howard Hinnant's algorithm) -- avoids a chrono
    // dependency for one timestamp string.
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m2 = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m2 <= 2 { y + 1 } else { y };
    format!("{y:04}-{m2:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Submits `end_run` on the (already-connected) supervisor lane and
/// applies its OWN reply (per Lifecycle, this arrives AT `record_closed`
/// — see `fe_client::QuitDispatcher::on_operation_state`'s own doc for
/// why that is not yet exit-worthy). Call sites keep polling `query` via
/// [`maybe_poll_quit_query`] afterward, every tick, until
/// `record_verified` or another terminal outcome (ruling (a), Codex
/// review round finding 1). Idempotent — a `quit` already in flight
/// (Ending/Verifying/terminal) makes this a no-op, so applying a
/// LATCHED quit at the top of a fresh episode can never double-fire.
fn run_quit(
    supervisor_conn: &PipeClient,
    sup_reader: &mut FrameReader,
    voyage: &str,
    reason: String,
    quit: &mut QuitDispatcher,
    outstanding: &mut OutstandingSlot,
    emit: &dyn Fn(ClientEvent),
) {
    let operation_id = format!("fe-quit-{}", uuid::Uuid::now_v7());
    if !quit.request_quit(operation_id.clone(), Instant::now()) {
        return; // already ending/verifying/terminal -- idempotent
    }
    // Ruling (c), Codex review round finding 6: an input outstanding
    // when quit is requested is reported, never dropped silently.
    if let Some(o) = outstanding.cancel_for_quit() {
        emit(ClientEvent::Status(format!(
            "input canceled by quit \u{2014} {} byte(s) not confirmed",
            o.bytes.len()
        )));
    }
    let cmd = SupervisorRequest::Command {
        operation_id,
        op: SupervisorOp::EndRun { reason, voyage: voyage.to_string() },
    };
    if let Ok(bytes) = wire::encode_supervisor_request(&cmd) {
        if write_bounded(supervisor_conn, &bytes, Instant::now() + WRITE_BUDGET).is_ok() {
            // The command's own reply is bounded by the SAME cutoff
            // `QuitDispatcher::tick` uses, so a blocking wait here and
            // the dispatcher's own elapsed-time check agree on one
            // bound rather than stacking two.
            if let Ok(DecodedFrame::SupervisorReply(SupervisorReply::Operation(state))) =
                sup_reader.next_frame(supervisor_conn, Instant::now() + fe_client::QUIT_CUTOFF)
            {
                quit.on_operation_state(state);
            }
        }
    }
    emit(ClientEvent::QuitMessage(quit.message()));
}

/// While `quit` is `Ending`/`Verifying`, polls `query{operation_id}`
/// every [`QUIT_QUERY_INTERVAL`] and applies the reply — the mechanism
/// that actually reaches `record_verified` after the command's own
/// `record_closed` reply (ruling (a), Codex review round finding 1:
/// "after record_closed, query the operation until record_verified").
/// A no-op whenever `quit` is not currently waiting on anything, or the
/// interval has not yet elapsed since the last poll.
fn maybe_poll_quit_query(
    supervisor_conn: &PipeClient,
    sup_reader: &mut FrameReader,
    quit: &mut QuitDispatcher,
    next_quit_query_at: &mut Instant,
    now: Instant,
    emit: &dyn Fn(ClientEvent),
) {
    let Some(operation_id) = quit.operation_id() else {
        return;
    };
    if now < *next_quit_query_at {
        return;
    }
    *next_quit_query_at = now + QUIT_QUERY_INTERVAL;
    let q = SupervisorRequest::Query { operation_id: operation_id.to_string() };
    if let Ok(bytes) = wire::encode_supervisor_request(&q) {
        if write_bounded(supervisor_conn, &bytes, Instant::now() + WRITE_BUDGET).is_ok() {
            if let Ok(DecodedFrame::SupervisorReply(SupervisorReply::Operation(state))) =
                sup_reader.next_frame(supervisor_conn, Instant::now() + STATUS_BUDGET)
            {
                quit.on_operation_state(state);
                emit(ClientEvent::QuitMessage(quit.message()));
            }
        }
    }
}

// -----------------------------------------------------------------------
// The episode-scoped attach-connection reader
// -----------------------------------------------------------------------

/// Blocking read, decode via its own `FrameSplitter`, forward each frame
/// to the worker. Gates its OWN next `read()` call on the SHARED
/// `queued_bytes` counter (Codex review round, finding 7: "the FE STOPS
/// READING THE PIPE" is now real — this counter is the SAME `Arc` clone
/// [`FeAttachClient::pump`] decrements, not a private, immediately-
/// released one). `stop` breaks the backpressure wait itself (a sleep
/// loop `cancel()` cannot reach); a normal teardown sets it just before
/// calling `cancel()`. `Keepalive` is answered directly here (bounced
/// back byte-identical), never round-tripped through the worker.
fn run_attach_reader(
    conn: Arc<PipeClient>,
    mut reader: FrameReader,
    tx: Sender<WorkerMsg>,
    queued_bytes: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
) {
    loop {
        while queued_bytes.load(Ordering::Acquire) >= READER_QUEUE_CAP_BYTES {
            if stop.load(Ordering::Acquire) {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        if stop.load(Ordering::Acquire) {
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(3600); // steady-state: no artificial read deadline; EOF/cancel end it
        let frame = match reader.next_frame(&conn, deadline) {
            Ok(f) => f,
            Err(_) => {
                let _ = tx.send(WorkerMsg::ReaderDone);
                return;
            }
        };
        if let DecodedFrame::Keepalive { nonce } = &frame {
            let _ = conn.write_all(&wire::encode_keepalive(*nonce));
            continue;
        }
        if let DecodedFrame::AttachServer(AttachServer::Output { bytes }) = &frame {
            // The ONLY increment of the shared byte-account — paired
            // with `FeAttachClient::pump`'s own decrement.
            queued_bytes.fetch_add(bytes.len(), Ordering::AcqRel);
        }
        if tx.send(WorkerMsg::Frame(frame)).is_err() {
            return;
        }
    }
}

// -----------------------------------------------------------------------
// Steady state
// -----------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn run_steady_state(
    cmd_rx: &Receiver<WorkerMsg>,
    events_tx: &Sender<ClientEvent>,
    wake: &(dyn Fn() + Send),
    attach_conn: &Arc<PipeClient>,
    supervisor_conn: &mut PipeClient,
    sup_reader: &mut FrameReader,
    take: &mut TakeTransaction,
    take_intent: &mut TakeIntent,
    outstanding: &mut OutstandingSlot,
    quit: &mut QuitDispatcher,
    reconnect: &mut ReconnectState,
    cols: &mut u16,
    rows: &mut u16,
    take_epoch: &mut u64,
    controller_id: &str,
    voyage: &str,
    last_liveness_poll: &mut Instant,
) -> SteadyOutcome {
    let emit = |e: ClientEvent| {
        let _ = events_tx.send(e);
        wake();
    };
    let mut next_quit_query_at = Instant::now();

    loop {
        match cmd_rx.recv_timeout(WORKER_TICK) {
            Ok(WorkerMsg::Shutdown) => return SteadyOutcome::Shutdown,
            Ok(WorkerMsg::Input(bytes)) => match take.role() {
                Role::Watching => {
                    for action in take.on_input_while_watching(&bytes) {
                        apply_single_take_action(action, attach_conn, controller_id, &emit);
                    }
                }
                Role::Taking | Role::Resizing => {
                    for action in take.on_input_while_pending(&bytes) {
                        apply_single_take_action(action, attach_conn, controller_id, &emit);
                    }
                }
                Role::Driving => {
                    if outstanding.outstanding().is_some() {
                        // Ruling (b), Codex review round finding 5: an
                        // input already outstanding queues the next one
                        // rather than dropping it.
                        for action in take.queue_while_driving(&bytes) {
                            apply_single_take_action(action, attach_conn, controller_id, &emit);
                        }
                    } else {
                        send_new_input(attach_conn, outstanding, *take_epoch, controller_id, voyage, bytes);
                    }
                }
            },
            Ok(WorkerMsg::Resize(c, r)) => {
                *cols = c;
                *rows = r;
                // An ad hoc resize while already DRIVING is sent
                // immediately (unrelated to the take transaction's own
                // resize, which is awaited alone before ANYTHING else
                // goes out — see ruling (b)'s lockstep fix). Not sent
                // while WATCHING/TAKING/RESIZING: a watcher cannot
                // correct the geometry until it holds the pen, and while
                // RESIZING a second resize would itself violate lockstep.
                if take.role() == Role::Driving {
                    let frame = AttachClient::Resize { cols: c, rows: r };
                    if let Ok(enc) = wire::encode_attach_client(&frame) {
                        let _ = write_bounded(attach_conn, &enc, Instant::now() + WRITE_BUDGET);
                    }
                }
            }
            Ok(WorkerMsg::Quit(reason)) => {
                run_quit(supervisor_conn, sup_reader, voyage, reason, quit, outstanding, &emit);
                next_quit_query_at = Instant::now() + QUIT_QUERY_INTERVAL;
            }
            Ok(WorkerMsg::Frame(frame)) => {
                match handle_attach_frame(
                    frame, attach_conn, take, take_intent, outstanding, take_epoch, controller_id, voyage, *cols,
                    *rows, &emit,
                ) {
                    FrameOutcome::ReattachRequested => return SteadyOutcome::ReconnectPreserveTake,
                    FrameOutcome::Handled | FrameOutcome::Ignored => {}
                }
            }
            Ok(WorkerMsg::ReaderDone) => {
                return SteadyOutcome::Reconnect;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return SteadyOutcome::Shutdown,
        }

        let now = Instant::now();
        quit.tick(now);
        maybe_poll_quit_query(supervisor_conn, sup_reader, quit, &mut next_quit_query_at, now, &emit);
        if quit.should_exit() {
            return SteadyOutcome::QuitEnded;
        }
        for action in take.tick_checkpoint_retry(now) {
            apply_single_take_action(action, attach_conn, controller_id, &emit);
        }

        if now.duration_since(*last_liveness_poll) >= LIVENESS_POLL_INTERVAL {
            *last_liveness_poll = now;
            match supervisor_status(supervisor_conn, sup_reader) {
                Ok((_, _, phase)) => {
                    // The supervisor answered -- unambiguously NOT
                    // absent/unresponsive; the voyage pipe question
                    // never even arises (ruling (d), finding 8).
                    reconnect.clear_unresponsive();
                    if let ReconnectDecision::Terminal(reason) = reconnect.classify_supervisor_phase(phase) {
                        return SteadyOutcome::Terminal(format!("supervisor: {reason:?}"));
                    }
                }
                Err(_) => {
                    // Codex review round, finding 8: the attach
                    // connection is DEMONSTRABLY alive right now (we are
                    // actively reading it in this very loop) — the
                    // voyage-absent half of the AND condition is false
                    // by construction here, so the health window must
                    // NEVER be consulted from this branch. "The capsule
                    // survives headless": keep going.
                    reconnect.clear_unresponsive();
                    emit(ClientEvent::Status(
                        "supervisor lane not answering \u{2014} the session is still live".to_string(),
                    ));
                }
            }
        }
    }
}

enum FrameOutcome {
    Handled,
    Ignored,
    /// `take_refused{not_attached}` — the caller ends this episode
    /// PRESERVING the take transaction (ruling (b), Codex review round
    /// finding 4).
    ReattachRequested,
}

fn mint_idem_key() -> [u8; 16] {
    let mut buf = [0u8; 16];
    let _ = getrandom::fill(&mut buf);
    buf
}

fn send_wire_input(attach_conn: &PipeClient, controller_id: &str, take_epoch: u64, idem_key: [u8; 16], payload: Vec<u8>) {
    let frame = AttachClient::Input { controller_id: controller_id.to_string(), take_epoch, idem_key, payload };
    if let Ok(enc) = wire::encode_attach_client(&frame) {
        let _ = write_bounded(attach_conn, &enc, Instant::now() + WRITE_BUDGET);
    }
}

/// Records a FRESH outstanding input (a new idem key) and sends it —
/// the ordinary path for both a first Driving-idle keystroke and a
/// flushed queue entry.
fn send_new_input(
    attach_conn: &PipeClient,
    outstanding: &mut OutstandingSlot,
    take_epoch: u64,
    controller_id: &str,
    voyage: &str,
    bytes: Vec<u8>,
) {
    let idem_key = outstanding.record(voyage.to_string(), take_epoch, bytes.clone(), mint_idem_key);
    send_wire_input(attach_conn, controller_id, take_epoch, idem_key, bytes);
}

/// Dispatches one `TakeAction`. `SendInput` no longer exists as a
/// variant (Codex review round, finding 3: flushing the queue is never
/// bundled with `take_ok`'s own actions) — every input send in this
/// module goes through [`send_new_input`]/[`send_wire_input`] instead,
/// called from the specific points ruling (b)/(c) pin (after
/// `resize_ok`, after an outstanding reply resolves while DRIVING).
fn apply_single_take_action(action: TakeAction, attach_conn: &PipeClient, controller_id: &str, emit: &dyn Fn(ClientEvent)) {
    match action {
        TakeAction::SendTake => {
            let frame = AttachClient::Take { controller_id: controller_id.to_string() };
            if let Ok(enc) = wire::encode_attach_client(&frame) {
                let _ = write_bounded(attach_conn, &enc, Instant::now() + WRITE_BUDGET);
            }
        }
        TakeAction::SendResize { cols, rows } => {
            let frame = AttachClient::Resize { cols, rows };
            if let Ok(enc) = wire::encode_attach_client(&frame) {
                let _ = write_bounded(attach_conn, &enc, Instant::now() + WRITE_BUDGET);
            }
        }
        TakeAction::QueueDiscarded => {
            emit(ClientEvent::Status("input discarded \u{2014} the pen never arrived in time".to_string()));
        }
        TakeAction::GeometryUnrepresentable => {
            emit(ClientEvent::Status("window size not representable by this session".to_string()));
        }
        TakeAction::PenLost => {
            emit(ClientEvent::Status("lost the pen".to_string()));
        }
        TakeAction::Reattach => {
            // Handled by the caller propagating `FrameOutcome::
            // ReattachRequested` up to `SteadyOutcome::
            // ReconnectPreserveTake` -- nothing to send here (the
            // server already does not recognize this connection as
            // attached).
        }
    }
}

/// After the pen is fully secured (`resize_ok`, or `resize_refused{out_
/// of_budget}` which still keeps it): send whatever is owed next, per
/// `take_intent` — the reconnect resend (SAME key), the stale retry
/// (NEW key under the now-current epoch), or the ordinary queued flush
/// (fresh key). Ruling (c), Codex review round finding 6.
fn flush_after_pen_secured(
    attach_conn: &PipeClient,
    take: &mut TakeTransaction,
    take_intent: &mut TakeIntent,
    outstanding: &mut OutstandingSlot,
    take_epoch: u64,
    controller_id: &str,
    voyage: &str,
) {
    match std::mem::replace(take_intent, TakeIntent::Ordinary) {
        TakeIntent::ReconnectResend => {
            if let Some(o) = outstanding.outstanding() {
                send_wire_input(attach_conn, controller_id, o.take_epoch, o.idem_key, o.bytes.clone());
            }
        }
        TakeIntent::StaleRetry => {
            let resolution = outstanding.apply_outcome(InputWireOutcome::RefusedStale, take_epoch, mint_idem_key);
            if let fe_client::OutstandingResolution::RetryNewEpoch { idem_key } = resolution {
                if let Some(o) = outstanding.outstanding() {
                    send_wire_input(attach_conn, controller_id, take_epoch, idem_key, o.bytes.clone());
                }
            }
        }
        TakeIntent::Ordinary => {
            if let Some(bytes) = take.take_queued() {
                send_new_input(attach_conn, outstanding, take_epoch, controller_id, voyage, bytes);
            }
        }
    }
}

/// After an outstanding input's reply resolves (`InputRecorded`/
/// `InputDeliveryUnknown`) while still DRIVING: flush whatever the take
/// transaction queued behind it (ruling (b), Codex review round finding
/// 5's own "dispatch queued bytes after the outstanding reply").
fn flush_next_driving_input(
    attach_conn: &PipeClient,
    take: &mut TakeTransaction,
    outstanding: &mut OutstandingSlot,
    take_epoch: u64,
    controller_id: &str,
    voyage: &str,
) {
    if take.role() != Role::Driving {
        return;
    }
    if let Some(bytes) = take.take_queued() {
        send_new_input(attach_conn, outstanding, take_epoch, controller_id, voyage, bytes);
    }
}

/// Dispatches one incoming attach-lane frame (unsolicited `Output` or a
/// reply to whatever the worker most recently sent).
#[allow(clippy::too_many_arguments)]
fn handle_attach_frame(
    frame: DecodedFrame,
    attach_conn: &PipeClient,
    take: &mut TakeTransaction,
    take_intent: &mut TakeIntent,
    outstanding: &mut OutstandingSlot,
    take_epoch: &mut u64,
    controller_id: &str,
    voyage: &str,
    cols: u16,
    rows: u16,
    emit: &dyn Fn(ClientEvent),
) -> FrameOutcome {
    match frame {
        DecodedFrame::AttachServer(AttachServer::Output { bytes }) => {
            emit(ClientEvent::Output(bytes));
            FrameOutcome::Handled
        }
        DecodedFrame::AttachServer(AttachServer::TakeOk { take_epoch: epoch }) => {
            *take_epoch = epoch;
            if *take_intent == TakeIntent::ReconnectResend {
                if let fe_client::ReconnectResendDecision::Cancel { canceled } =
                    outstanding.resend_after_reconnect(voyage, epoch)
                {
                    emit(ClientEvent::Status(format!(
                        "input canceled \u{2014} the voyage changed ({} byte(s) lost)",
                        canceled.bytes.len()
                    )));
                    *take_intent = TakeIntent::Ordinary;
                }
            }
            for action in take.on_take_ok(cols, rows) {
                apply_single_take_action(action, attach_conn, controller_id, emit);
            }
            FrameOutcome::Handled
        }
        DecodedFrame::AttachServer(AttachServer::TakeRefused { reason }) => {
            match reason {
                TakeRefusedReason::NotAttached => {
                    let actions = take.on_take_refused_not_attached();
                    let reattach = actions.contains(&TakeAction::Reattach);
                    for action in actions {
                        apply_single_take_action(action, attach_conn, controller_id, emit);
                    }
                    if reattach {
                        return FrameOutcome::ReattachRequested;
                    }
                }
                TakeRefusedReason::CheckpointInFlight => {
                    for action in take.on_take_refused_checkpoint_in_flight(Instant::now()) {
                        apply_single_take_action(action, attach_conn, controller_id, emit);
                    }
                }
            }
            FrameOutcome::Handled
        }
        DecodedFrame::AttachServer(AttachServer::ResizeOk) => {
            take.on_resize_ok();
            flush_after_pen_secured(attach_conn, take, take_intent, outstanding, *take_epoch, controller_id, voyage);
            FrameOutcome::Handled
        }
        DecodedFrame::AttachServer(AttachServer::ResizeRefused { reason }) => {
            let was_resizing = take.role() == Role::Resizing;
            for action in take.on_resize_refused(reason) {
                apply_single_take_action(action, attach_conn, controller_id, emit);
            }
            if was_resizing && reason == ResizeRefusedReason::OutOfBudget {
                // `on_resize_refused` already promoted RESIZING ->
                // DRIVING for this refusal -- the pen is still held, so
                // whatever was queued behind the take-ok flushes exactly
                // as it would after a real `resize_ok`.
                flush_after_pen_secured(attach_conn, take, take_intent, outstanding, *take_epoch, controller_id, voyage);
            }
            FrameOutcome::Handled
        }
        DecodedFrame::AttachServer(AttachServer::InputRecorded) => {
            let _ = outstanding.apply_outcome(InputWireOutcome::Recorded, *take_epoch, mint_idem_key);
            flush_next_driving_input(attach_conn, take, outstanding, *take_epoch, controller_id, voyage);
            FrameOutcome::Handled
        }
        DecodedFrame::AttachServer(AttachServer::InputRefusedStale) => {
            // Ruling (c), Codex review round finding 6: re-take FIRST;
            // the new key is minted once the fresh `take_ok` arrives
            // (see the `TakeOk` arm above and `flush_after_pen_secured`'s
            // own `StaleRetry` handling).
            *take_intent = TakeIntent::StaleRetry;
            for action in take.retake_while_driving() {
                apply_single_take_action(action, attach_conn, controller_id, emit);
            }
            FrameOutcome::Handled
        }
        DecodedFrame::AttachServer(AttachServer::InputDeliveryUnknown) => {
            let res = outstanding.apply_outcome(InputWireOutcome::DeliveryUnknown, *take_epoch, mint_idem_key);
            if matches!(res, fe_client::OutstandingResolution::Unknown) {
                emit(ClientEvent::Status("input delivery unknown".to_string()));
            }
            flush_next_driving_input(attach_conn, take, outstanding, *take_epoch, controller_id, voyage);
            FrameOutcome::Handled
        }
        DecodedFrame::AttachServer(AttachServer::AttachRefused { .. })
        | DecodedFrame::AttachServer(AttachServer::HelloOk { .. })
        | DecodedFrame::AttachServer(AttachServer::HelloRefused { .. })
        | DecodedFrame::AttachServer(AttachServer::CheckpointChunk { .. }) => FrameOutcome::Ignored,
        DecodedFrame::Keepalive { .. } => FrameOutcome::Ignored, // answered by the reader thread directly
        DecodedFrame::MgmtRequest(_)
        | DecodedFrame::MgmtReply(_)
        | DecodedFrame::AttachClient(_)
        | DecodedFrame::SupervisorRequest(_)
        | DecodedFrame::SupervisorReply(_) => FrameOutcome::Ignored,
    }
}
