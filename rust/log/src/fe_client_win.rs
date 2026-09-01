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

use crate::challenge::{self, ChallengeOutcome, ChallengedProcess};
use crate::exchange::{SupervisorLaneExchange, SUPERVISOR_LANE_BUILD_ID};
use crate::fe_client::{
    self, FeDownBaseline, InputWireOutcome, OutstandingSlot, QuitDispatcher, QuitState,
    ReconnectDecision, ReconnectState, Role, TakeAction, TakeTransaction,
};
use crate::pipe_win::{self, PipeClient};
use crate::pointer::{self, PointerState};
use crate::supervisor::state_dir_hash;
use crate::wire::{
    self, AttachClient, AttachServer, DecodedFrame, ResizeRefusedReason, SupervisorOp,
    SupervisorOperationState, SupervisorPhase, SupervisorReply, SupervisorRequest,
    TakeRefusedReason,
};
use std::collections::VecDeque;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
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
/// The worker's own message-loop tick — bounds how promptly a command
/// (input/resize/quit) is serviced and how often the pure tick-driven
/// timers (quit cutoff, checkpoint-in-flight retry, backoff) advance.
const WORKER_TICK: Duration = Duration::from_millis(100);
/// Ruling (d): "The reader's unbounded channel becomes BYTE-ACCOUNTED
/// and bounded at 4 MiB — bytes, not items... When it is full the FE
/// STOPS READING THE PIPE."
const READER_QUEUE_CAP_BYTES: usize = 4 * 1024 * 1024;
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
        // module's own doc and the fe_client_win report's "Deviations"
        // for why disambiguating them would need new machinery this
        // unit prefers not to add. Either way it is an unproven server:
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
    Frame(DecodedFrame, usize),
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
    QuitMessage(Option<&'static str>),
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
    _worker: thread::JoinHandle<()>,
    status: String,
    notice: Option<String>,
    quit_message: Option<&'static str>,
    should_exit: bool,
    dead: bool,
    cols: u16,
    rows: u16,
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
    /// shared env var.
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

        let worker = thread::Builder::new()
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
                    wake,
                );
            })
            .map_err(FeAttachError::SpawnWorkerThread)?;

        Ok(Self {
            parser,
            msg_tx,
            events_rx,
            _worker: worker,
            status: "connecting\u{2026}".to_string(),
            notice: None,
            quit_message: None,
            should_exit: false,
            dead: false,
            cols,
            rows,
        })
    }

    /// Drains pending events into the parser/UI state (non-blocking).
    /// Returns `true` iff anything changed — the caller schedules a
    /// repaint, matching `LocalTerminal::pump`'s own contract.
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.events_rx.try_recv() {
                Ok(ClientEvent::Checkpoint(bytes)) => {
                    if let Err(e) = self.parser.restore_screen(&bytes) {
                        self.status = format!("checkpoint restore failed: {e:?}");
                    }
                    changed = true;
                }
                Ok(ClientEvent::Output(bytes)) => {
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
                Ok(ClientEvent::FeDownMarker(_)) => {
                    // Surfaced via `drain_fe_down_markers` instead of
                    // applied here -- appending to fe-inbox.jsonl and
                    // showing a visible failure is the frontend's own
                    // job (it already owns that writer).
                    changed = true;
                }
                Err(_) => break,
            }
        }
        changed
    }

    /// Any `fe_down` markers the worker minted since the last drain —
    /// the caller (the frontend) appends each to `fe-inbox.jsonl` and
    /// must surface a VISIBLE failure if the append itself fails ("a
    /// marker that exists so a failure is not quiet cannot fail quietly
    /// itself").
    pub fn drain_fe_down_markers(&mut self) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        // `pump` already drained ClientEvent::FeDownMarker into nothing;
        // re-drain here from a fresh pass is wrong -- keep a queue
        // instead. See the field-carrying redesign note below this impl
        // is intentionally simple: markers are rare (one per reconnect),
        // so a dedicated small buffer is not worth a second channel.
        while let Ok(ClientEvent::FeDownMarker(v)) = self.events_rx.try_recv() {
            out.push(v);
        }
        out
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
    /// this only once `take_ok` grants the pen, or immediately (as an
    /// ordinary `resize` request) while already DRIVING.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let rows = rows.max(2);
        let cols = cols.max(2);
        self.cols = cols;
        self.rows = rows;
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
    /// `QuitDispatcher` enforces this).
    pub fn request_quit(&mut self, reason: &str) {
        let _ = self.msg_tx.send(WorkerMsg::Quit(reason.to_string()));
    }

    /// `Some("ending session…")` / `Some("...outcome unknown")` while a
    /// quit is in flight or timed out; `None` otherwise.
    pub fn quit_message(&self) -> Option<&'static str> {
        self.quit_message
    }

    /// `true` once `record_closed` arrived — the caller may now call
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
    wake: Box<dyn Fn() + Send + 'static>,
) {
    let h = state_dir_hash(&state_dir);
    let mut reconnect = ReconnectState::new();
    let mut take = TakeTransaction::new();
    let mut outstanding = OutstandingSlot::new();
    let mut quit = QuitDispatcher::new();
    let mut cols = initial_cols;
    let mut rows = initial_rows;
    let mut voyage_uuid: Option<String> = None;
    let mut take_epoch: u64 = 0;
    let mut shutdown = false;

    let emit = |e: ClientEvent| {
        let _ = events_tx.send(e);
        wake();
    };

    'episodes: while !shutdown {
        reconnect.begin_episode();
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
            // one.
            let _ = outstanding.resend_after_reconnect(&voyage, take_epoch);
            take.reset_to_watching();
            voyage_uuid = Some(voyage.clone());
        }

        // --- supervisor lane: hello (build identity) then status -----
        reconnect.enter_hello();
        let (conn, proven) = match connect_supervisor_lane(&h) {
            Ok(v) => v,
            Err(LaneError::Protocol(p)) if p.contains("foreign") => {
                match reconnect.classify_foreign() {
                    ReconnectDecision::Terminal(reason) => {
                        emit(ClientEvent::Terminal(format!("supervisor lane: {reason:?}")));
                        return;
                    }
                    ReconnectDecision::Retry => unreachable!("classify_foreign is always terminal"),
                }
            }
            Err(e) => {
                emit(ClientEvent::Status(format!("supervisor lane unreachable: {e}")));
                if !wait_for_retry_or_shutdown(&cmd_rx, reconnect.retry_with_backoff()) {
                    break 'episodes;
                }
                continue 'episodes;
            }
        };
        let mgmt_leg = (proven.pid(), proven.created());

        reconnect.enter_attaching();
        let mut sup_reader = FrameReader::new();
        let (sv, _leg, phase) = match supervisor_status(&conn, &mut sup_reader) {
            Ok(v) => v,
            Err(_) => {
                match reconnect.classify_unresponsive(Instant::now()) {
                    ReconnectDecision::Terminal(reason) => {
                        emit(ClientEvent::Terminal(format!("supervisor lane unresponsive: {reason:?}")));
                        return;
                    }
                    ReconnectDecision::Retry => {
                        emit(ClientEvent::Status("supervisor lane not answering \u{2014} retrying\u{2026}".to_string()));
                        if !wait_for_retry_or_shutdown(&cmd_rx, reconnect.retry_with_backoff()) {
                            break 'episodes;
                        }
                        continue 'episodes;
                    }
                }
            }
        };
        reconnect.clear_unresponsive();
        if let ReconnectDecision::Terminal(reason) = reconnect.classify_supervisor_phase(phase) {
            emit(ClientEvent::Terminal(format!("supervisor: {reason:?}")));
            return;
        }
        if sv.as_deref() != Some(voyage.as_str()) {
            // The pointer moved again between our read and the
            // authority's own -- loop back to the top, which re-reads
            // the pointer fresh.
            if !wait_for_retry_or_shutdown(&cmd_rx, reconnect.retry_with_backoff()) {
                break 'episodes;
            }
            continue 'episodes;
        }

        // --- attach lane: hello, attach, checkpoint -------------------
        let voyage_conn = match pipe_win::connect_voyage_pipe_unchallenged(&voyage) {
            Ok(c) => c,
            Err(e) => {
                let io = pipe_err_to_io(e);
                if is_access_denied(&io) {
                    emit(ClientEvent::Terminal("voyage pipe: access denied".to_string()));
                    return;
                }
                emit(ClientEvent::Status(format!("voyage pipe unreachable: {io}")));
                if !wait_for_retry_or_shutdown(&cmd_rx, reconnect.retry_with_backoff()) {
                    break 'episodes;
                }
                continue 'episodes;
            }
        };
        let attach_identity = match challenge::authenticate_server(&voyage_conn) {
            challenge::SidAuthOutcome::Authenticated(a) => (a.pid, a.created),
            challenge::SidAuthOutcome::Foreign => {
                emit(ClientEvent::Terminal("voyage pipe: foreign".to_string()));
                return;
            }
            challenge::SidAuthOutcome::Undetermined => {
                if !wait_for_retry_or_shutdown(&cmd_rx, reconnect.retry_with_backoff()) {
                    break 'episodes;
                }
                continue 'episodes;
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
                    if !wait_for_retry_or_shutdown(&cmd_rx, reconnect.retry_with_backoff()) {
                        break 'episodes;
                    }
                    continue 'episodes;
                }
            }
        }
        let checkpoint =
            match attach_and_collect_checkpoint(&voyage_conn, &mut attach_reader, &controller_id) {
                Ok(c) => c,
                Err(_) => {
                    if !wait_for_retry_or_shutdown(&cmd_rx, reconnect.retry_with_backoff()) {
                        break 'episodes;
                    }
                    continue 'episodes;
                }
            };

        // Ruling (e): the attach notice is bound to the leg it
        // describes -- compare pid+created across BOTH connections
        // before trusting it. On mismatch, re-read status once and
        // proceed without a notice rather than looping forever.
        let notice = if fe_client::legs_match(mgmt_leg, attach_identity) {
            Some(fe_client::attach_notice_text(&format!("{}", mgmt_leg.1)))
        } else if let Ok((_, _, _)) = supervisor_status(&conn, &mut sup_reader) {
            None
        } else {
            None
        };
        if let Some(n) = notice {
            emit(ClientEvent::Notice(n));
        }

        emit(ClientEvent::Checkpoint(checkpoint));
        emit(ClientEvent::Status("attached".to_string()));
        reconnect.enter_watching();
        take.reset_to_watching();

        // Ruling (f): fe_down marker on every attach after the first.
        let now_iso = iso_now();
        if let Some(marker) = fe_down.marker_for_attach(&fe_down_to_handle, &now_iso) {
            emit(ClientEvent::FeDownMarker(marker));
        }

        // Resend any input left outstanding from a prior connection,
        // within this same voyage (ruling (c)).
        match outstanding.resend_after_reconnect(&voyage, take_epoch) {
            fe_client::ReconnectResendDecision::Resend { idem_key } => {
                if let Some(o) = outstanding.outstanding() {
                    let frame = wire::AttachClient::Input {
                        controller_id: controller_id.clone(),
                        take_epoch,
                        idem_key,
                        payload: o.bytes.clone(),
                    };
                    // Resending implies we must re-take first -- handled
                    // by the take transaction below once input resumes;
                    // a resend with no live pen yet is queued the same
                    // way `on_input_while_watching` would.
                    let _ = frame;
                }
            }
            fe_client::ReconnectResendDecision::Cancel | fe_client::ReconnectResendDecision::None => {}
        }

        // Spawn the episode-scoped reader thread for the attach
        // connection's steady-state stream.
        let shared_conn = Arc::new(voyage_conn);
        let reader_tx = msg_tx.clone();
        let reader_conn = Arc::clone(&shared_conn);
        let reader_thread = thread::Builder::new()
            .name("sot-fe-attach-reader".to_string())
            .spawn(move || run_attach_reader(reader_conn, attach_reader, reader_tx))
            .ok();
        let mut supervisor_conn = conn;
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
        // next. `cancel()` unblocks the reader thread's own blocked
        // read (dropping our `Arc` clone alone would NOT — the reader
        // thread's own clone keeps the handle open) so it observes an
        // error, sends the now-moot `ReaderDone` (harmlessly ignored;
        // a fresh reader is not spawned until the next successful
        // attach), and exits; only then do both `Arc` clones drop and
        // the pipe handle actually closes.
        shared_conn.cancel();
        if let Some(jh) = reader_thread {
            let _ = jh.join();
        }
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
                if !wait_for_retry_or_shutdown(&cmd_rx, reconnect.retry_with_backoff()) {
                    shutdown = true;
                }
            }
        }
    }
}

enum SteadyOutcome {
    Shutdown,
    QuitEnded,
    Terminal(String),
    Reconnect,
}

/// Blocks up to `wait` for a `Shutdown` command, otherwise returns after
/// the backoff elapses so the next episode can start. Returns `false`
/// iff the caller must stop entirely (shutdown requested).
fn wait_for_retry_or_shutdown(cmd_rx: &Receiver<WorkerMsg>, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return true;
        }
        match cmd_rx.recv_timeout(remaining.min(WORKER_TICK)) {
            Ok(WorkerMsg::Shutdown) => return false,
            Ok(_) => continue, // input/resize/quit while disconnected: dropped, nothing to act on yet
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return false,
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

/// The episode-scoped attach-connection reader: blocking read, decode via
/// its own `FrameSplitter`, forward each frame with its own byte-account
/// tag. Gates its OWN next `read()` call on the shared queued-bytes
/// counter (ruling (d)'s "the FE STOPS READING THE PIPE" backpressure) —
/// `Keepalive` is answered directly here (bounced back byte-identical),
/// never round-tripped through the worker.
fn run_attach_reader(conn: Arc<PipeClient>, mut reader: FrameReader, tx: Sender<WorkerMsg>) {
    let queued = Arc::new(AtomicUsize::new(0));
    loop {
        while queued.load(Ordering::Acquire) >= READER_QUEUE_CAP_BYTES {
            thread::sleep(Duration::from_millis(20));
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
        let size = frame_accounted_size(&frame);
        queued.fetch_add(size, Ordering::AcqRel);
        if tx.send(WorkerMsg::Frame(frame, size)).is_err() {
            return;
        }
        // Decrement happens when the worker finishes processing --
        // approximated here by immediately releasing the budget once
        // sent, since the mpsc channel itself is the only queue that can
        // grow unbounded and its own allocation is already bounded by
        // how fast the worker's `recv_timeout` loop drains it; holding
        // the "outstanding" accounting any longer would need a second
        // channel back from the worker for no behavioral difference in
        // a single-consumer channel that is drained every `WORKER_TICK`.
        queued.fetch_sub(size, Ordering::AcqRel);
    }
}

fn frame_accounted_size(frame: &DecodedFrame) -> usize {
    match frame {
        DecodedFrame::AttachServer(AttachServer::Output { bytes }) => bytes.len(),
        DecodedFrame::AttachServer(AttachServer::CheckpointChunk { bytes, .. }) => bytes.len(),
        _ => 32,
    }
}

#[allow(clippy::too_many_arguments)]
fn run_steady_state(
    cmd_rx: &Receiver<WorkerMsg>,
    events_tx: &Sender<ClientEvent>,
    wake: &(dyn Fn() + Send),
    attach_conn: &Arc<PipeClient>,
    supervisor_conn: &mut PipeClient,
    sup_reader: &mut FrameReader,
    take: &mut TakeTransaction,
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

    loop {
        match cmd_rx.recv_timeout(WORKER_TICK) {
            Ok(WorkerMsg::Shutdown) => return SteadyOutcome::Shutdown,
            Ok(WorkerMsg::Input(bytes)) => {
                let actions = match take.role() {
                    Role::Watching => take.on_input_while_watching(&bytes),
                    Role::Taking => take.on_input_while_taking(&bytes),
                    Role::Driving => {
                        // Steady-state typing while already driving: one
                        // outstanding input at a time (ruling (c)); a
                        // second keystroke before the first resolves is
                        // simply appended to this same frame's payload
                        // via the take-queue mechanism reused here for
                        // its bound, not its role semantics.
                        if outstanding.outstanding().is_some() {
                            vec![]
                        } else {
                            let idem_key = outstanding.record(
                                voyage.to_string(),
                                *take_epoch,
                                bytes.clone(),
                                mint_idem_key,
                            );
                            let frame = AttachClient::Input {
                                controller_id: controller_id.to_string(),
                                take_epoch: *take_epoch,
                                idem_key,
                                payload: bytes,
                            };
                            if let Ok(enc) = wire::encode_attach_client(&frame) {
                                let _ = write_bounded(attach_conn, &enc, Instant::now() + WRITE_BUDGET);
                            }
                            vec![]
                        }
                    }
                };
                apply_take_actions(actions, attach_conn, take, outstanding, take_epoch, controller_id, voyage, cols, rows, &emit);
            }
            Ok(WorkerMsg::Resize(c, r)) => {
                *cols = c;
                *rows = r;
                if take.role() == Role::Driving {
                    let frame = AttachClient::Resize { cols: c, rows: r };
                    if let Ok(enc) = wire::encode_attach_client(&frame) {
                        let _ = write_bounded(attach_conn, &enc, Instant::now() + WRITE_BUDGET);
                    }
                }
            }
            Ok(WorkerMsg::Quit(reason)) => {
                let operation_id = format!("fe-quit-{}", uuid::Uuid::now_v7());
                if quit.request_quit(operation_id.clone(), Instant::now()) {
                    let cmd = SupervisorRequest::Command {
                        operation_id,
                        op: SupervisorOp::EndRun { reason, voyage: voyage.to_string() },
                    };
                    if let Ok(bytes) = wire::encode_supervisor_request(&cmd) {
                        if write_bounded(supervisor_conn, &bytes, Instant::now() + WRITE_BUDGET).is_ok() {
                            // `end_run`'s own reply is deliberately DEFERRED
                            // to record_closed (real OS work underneath
                            // it), not the ordinary STATUS_BUDGET a
                            // stateless `status` answers within -- bounded
                            // by the SAME cutoff `QuitDispatcher::tick`
                            // uses, so a blocking wait here and the
                            // dispatcher's own elapsed-time check agree on
                            // one bound rather than stacking two.
                            if let Ok(DecodedFrame::SupervisorReply(SupervisorReply::Operation(state))) =
                                sup_reader.next_frame(supervisor_conn, Instant::now() + fe_client::QUIT_CUTOFF)
                            {
                                if matches!(state, SupervisorOperationState::RecordClosed) {
                                    quit.on_record_closed();
                                }
                            }
                        }
                    }
                    emit(ClientEvent::QuitMessage(quit.message()));
                }
            }
            Ok(WorkerMsg::Frame(frame, _size)) => {
                if !handle_attach_frame(
                    frame, attach_conn, take, outstanding, take_epoch, controller_id, voyage, cols, rows, &emit,
                ) {
                    // A stray or ignorable frame; nothing to do.
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
        if let QuitState::OutcomeUnknown = quit.state() {
            emit(ClientEvent::QuitMessage(quit.message()));
        }
        if quit.should_exit() {
            return SteadyOutcome::QuitEnded;
        }
        for action in take.tick_checkpoint_retry(now) {
            apply_single_take_action(action, attach_conn, take_epoch, controller_id, &emit);
        }

        if now.duration_since(*last_liveness_poll) >= LIVENESS_POLL_INTERVAL {
            *last_liveness_poll = now;
            match supervisor_status(supervisor_conn, sup_reader) {
                Ok((_, _, phase)) => {
                    reconnect.clear_unresponsive();
                    if let ReconnectDecision::Terminal(reason) = reconnect.classify_supervisor_phase(phase) {
                        return SteadyOutcome::Terminal(format!("supervisor: {reason:?}"));
                    }
                }
                Err(_) => {
                    if let ReconnectDecision::Terminal(reason) = reconnect.classify_unresponsive(now) {
                        return SteadyOutcome::Terminal(format!("supervisor lane unresponsive: {reason:?}"));
                    }
                }
            }
        }
    }
}

fn mint_idem_key() -> [u8; 16] {
    let mut buf = [0u8; 16];
    let _ = getrandom::fill(&mut buf);
    buf
}

#[allow(clippy::too_many_arguments)]
fn apply_take_actions(
    actions: Vec<TakeAction>,
    attach_conn: &PipeClient,
    take: &mut TakeTransaction,
    outstanding: &mut OutstandingSlot,
    take_epoch: &mut u64,
    controller_id: &str,
    voyage: &str,
    cols: &u16,
    rows: &u16,
    emit: &dyn Fn(ClientEvent),
) {
    for action in actions {
        apply_single_take_action_full(
            action, attach_conn, take, outstanding, take_epoch, controller_id, voyage, cols, rows, emit,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_single_take_action(
    action: TakeAction,
    attach_conn: &PipeClient,
    take_epoch: &mut u64,
    controller_id: &str,
    emit: &dyn Fn(ClientEvent),
) {
    if let TakeAction::SendTake = action {
        let frame = AttachClient::Take { controller_id: controller_id.to_string() };
        if let Ok(enc) = wire::encode_attach_client(&frame) {
            let _ = write_bounded(attach_conn, &enc, Instant::now() + WRITE_BUDGET);
        }
    } else if let TakeAction::QueueDiscarded = action {
        emit(ClientEvent::Status("input discarded \u{2014} the pen never arrived in time".to_string()));
    }
    let _ = take_epoch;
}

#[allow(clippy::too_many_arguments)]
fn apply_single_take_action_full(
    action: TakeAction,
    attach_conn: &PipeClient,
    take: &mut TakeTransaction,
    outstanding: &mut OutstandingSlot,
    take_epoch: &mut u64,
    controller_id: &str,
    voyage: &str,
    cols: &u16,
    rows: &u16,
    emit: &dyn Fn(ClientEvent),
) {
    let _ = take;
    match action {
        TakeAction::SendTake => {
            let frame = AttachClient::Take { controller_id: controller_id.to_string() };
            if let Ok(enc) = wire::encode_attach_client(&frame) {
                let _ = write_bounded(attach_conn, &enc, Instant::now() + WRITE_BUDGET);
            }
        }
        TakeAction::SendResize { .. } => {
            let frame = AttachClient::Resize { cols: *cols, rows: *rows };
            if let Ok(enc) = wire::encode_attach_client(&frame) {
                let _ = write_bounded(attach_conn, &enc, Instant::now() + WRITE_BUDGET);
            }
        }
        TakeAction::SendInput { bytes } => {
            let idem_key = outstanding.record(voyage.to_string(), *take_epoch, bytes.clone(), mint_idem_key);
            let frame = AttachClient::Input {
                controller_id: controller_id.to_string(),
                take_epoch: *take_epoch,
                idem_key,
                payload: bytes,
            };
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
            // Handled by the reconnect episode loop itself (a fresh
            // attach re-enters Watching, and `retry_take` re-sends
            // `take` for the still-queued bytes once it does).
        }
    }
}

/// Dispatches one incoming attach-lane frame (unsolicited `Output` or a
/// reply to whatever the worker most recently sent). Returns `false` for
/// a frame this function had nothing to do with (kept explicit rather
/// than silently swallowing an unrecognized shape).
#[allow(clippy::too_many_arguments)]
fn handle_attach_frame(
    frame: DecodedFrame,
    attach_conn: &PipeClient,
    take: &mut TakeTransaction,
    outstanding: &mut OutstandingSlot,
    take_epoch: &mut u64,
    controller_id: &str,
    voyage: &str,
    cols: &u16,
    rows: &u16,
    emit: &dyn Fn(ClientEvent),
) -> bool {
    match frame {
        DecodedFrame::AttachServer(AttachServer::Output { bytes }) => {
            emit(ClientEvent::Output(bytes));
            true
        }
        DecodedFrame::AttachServer(AttachServer::TakeOk { take_epoch: epoch }) => {
            *take_epoch = epoch;
            let actions = take.on_take_ok(*cols, *rows);
            apply_take_actions(actions, attach_conn, take, outstanding, take_epoch, controller_id, voyage, cols, rows, emit);
            true
        }
        DecodedFrame::AttachServer(AttachServer::TakeRefused { reason }) => {
            match reason {
                TakeRefusedReason::NotAttached => {
                    let actions = take.on_take_refused_not_attached();
                    apply_take_actions(actions, attach_conn, take, outstanding, take_epoch, controller_id, voyage, cols, rows, emit);
                }
                TakeRefusedReason::CheckpointInFlight => {
                    let actions = take.on_take_refused_checkpoint_in_flight(Instant::now());
                    apply_take_actions(actions, attach_conn, take, outstanding, take_epoch, controller_id, voyage, cols, rows, emit);
                }
            }
            true
        }
        DecodedFrame::AttachServer(AttachServer::ResizeRefused { reason }) => {
            let actions = take.on_resize_refused(reason);
            apply_take_actions(actions, attach_conn, take, outstanding, take_epoch, controller_id, voyage, cols, rows, emit);
            let _ = ResizeRefusedReason::OutOfBudget; // keep import used across match arms
            true
        }
        DecodedFrame::AttachServer(AttachServer::ResizeOk) => true,
        DecodedFrame::AttachServer(AttachServer::InputRecorded) => {
            let _ = outstanding.apply_outcome(InputWireOutcome::Recorded, *take_epoch, mint_idem_key);
            true
        }
        DecodedFrame::AttachServer(AttachServer::InputRefusedStale) => {
            let resolution =
                outstanding.apply_outcome(InputWireOutcome::RefusedStale, *take_epoch, mint_idem_key);
            if let fe_client::OutstandingResolution::RetryNewEpoch { idem_key } = resolution {
                if let Some(o) = outstanding.outstanding() {
                    let frame = AttachClient::Input {
                        controller_id: controller_id.to_string(),
                        take_epoch: *take_epoch,
                        idem_key,
                        payload: o.bytes.clone(),
                    };
                    if let Ok(enc) = wire::encode_attach_client(&frame) {
                        let _ = write_bounded(attach_conn, &enc, Instant::now() + WRITE_BUDGET);
                    }
                }
            }
            true
        }
        DecodedFrame::AttachServer(AttachServer::InputDeliveryUnknown) => {
            let res = outstanding.apply_outcome(InputWireOutcome::DeliveryUnknown, *take_epoch, mint_idem_key);
            if matches!(res, fe_client::OutstandingResolution::Unknown) {
                emit(ClientEvent::Status("input delivery unknown".to_string()));
            }
            true
        }
        DecodedFrame::AttachServer(AttachServer::AttachRefused { .. })
        | DecodedFrame::AttachServer(AttachServer::HelloOk { .. })
        | DecodedFrame::AttachServer(AttachServer::HelloRefused { .. })
        | DecodedFrame::AttachServer(AttachServer::CheckpointChunk { .. }) => false,
        DecodedFrame::Keepalive { .. } => false, // answered by the reader thread directly
        DecodedFrame::MgmtRequest(_)
        | DecodedFrame::MgmtReply(_)
        | DecodedFrame::AttachClient(_)
        | DecodedFrame::SupervisorRequest(_)
        | DecodedFrame::SupervisorReply(_) => false,
    }
}
