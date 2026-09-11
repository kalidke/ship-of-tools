#![cfg(any(windows, target_os = "linux"))]
//! L1-unix LU3b (ADR 0043 decision 20): the FE attach-only client's
//! RUNTIME — wires `fe_client`'s pure state machines (the six FE
//! rulings) to a real [`crate::client::Endpoint`], a real supervisor
//! lane, and the drawer's own `vt100_ctt::Parser`. Generic over `E:
//! Endpoint` (renamed from `fe_client_win.rs`, which hard-coded
//! `pipe_win::PipeClient`): every type in this module that used to name
//! `PipeClient`/`ChallengedProcess` directly now names `E::Client`/
//! `E::Process`, and every call that used to go straight to
//! `pipe_win`/`challenge_win` now goes through `E`'s own associated
//! functions — the concrete platform is chosen exactly once, by
//! [`crate::client::PlatformEndpoint`], which is what the frontend
//! instantiates this module's public type with (see [`FeAttachClient`]'s
//! own doc for its default type parameter). The client code itself is
//! otherwise the SAME kind of thin I/O wrapper `term::LocalTerminal`
//! already is over its own PTY: a background reader thread forwards
//! bytes/frames, the caller drains them non-blockingly via `pump()`.
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
//! - Backpressure (finding 7): `queued_bytes` is a SINGLE [`QueuedBytes`]
//!   shared between the episode reader (increments, blocks the pipe read
//!   when full) and `FeAttachClient::pump` (decrements on consumption,
//!   caps bytes drained per call). switch-latency Phase 1: the reader's
//!   "blocks when full" used to be a 20ms sleep poll; it now parks on
//!   `QueuedBytes`'s own condvar, woken the instant `pump` drains enough
//!   to matter (or immediately at episode teardown) — see that type's
//!   own doc.
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

use crate::challenge::{ChallengeOutcome, PeerAuthOutcome};
use crate::client::{transport_error_to_io, Client, Endpoint, PeerProcess, PlatformEndpoint};
use crate::exchange::{SupervisorLaneExchange, VoyageMgmtExchange, SUPERVISOR_LANE_BUILD_ID};
use crate::fe_client::{
    self, FeDownBaseline, InputWireOutcome, OutstandingSlot, QuitDispatcher, QuitState,
    ReconnectDecision, ReconnectState, Role, TakeAction, TakeTransaction,
};
use crate::pointer::{self, PointerState};
use crate::state_dir::state_dir_hash;
use crate::wire::{
    self, AttachClient, AttachServer, DecodedFrame, ResizeRefusedReason, SupervisorOp,
    SupervisorPhase, SupervisorReply, SupervisorRequest, TakeRefusedReason,
};
use std::collections::VecDeque;
use std::io::ErrorKind;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
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
/// Absolute deadline for an ENTIRE checkpoint transfer, not merely each
/// frame within it (Codex round on #194, finding 3): `STATUS_BUDGET`
/// alone re-arms every loop iteration in
/// [`attach_and_collect_checkpoint`], and the wire format allows any
/// number of `checkpoint_chunk` frames -- including empty non-final ones
/// (see `wire::CHECKPOINT_CHUNKS_AT_MAX_PAYLOAD`'s own doc) -- so a
/// faulty or hostile capsule dripping one technically-legal frame every
/// ~5 s could hold this worker open forever. Sized at the greedy
/// encoder's own worst-case chunk count times the per-frame budget:
/// generous for the ordinary case (a real checkpoint that large is
/// itself the ADR 0041 worst case), and still a REAL bound on the
/// adversarial one. See [`checkpoint_frame_deadline`] for how it clamps
/// each per-frame deadline.
const CHECKPOINT_TRANSFER_BUDGET: Duration =
    Duration::from_secs(wire::CHECKPOINT_CHUNKS_AT_MAX_PAYLOAD as u64 * STATUS_BUDGET.as_secs());
/// Ordinary lane-operation write budget (connect/write halves of the
/// Lifecycle "one budget" triple; the read half is `STATUS_BUDGET` or,
/// for `command`/`query`, the same 5 s figure). `pub(crate)`: shared with
/// [`run_end_run_and_wait`]'s own callers (ADR 0042 L1a, Codex review
/// finding 4 — `supervisor_client::end_run` reuses this exact budget
/// rather than inventing its own).
pub(crate) const WRITE_BUDGET: Duration = Duration::from_secs(2);
/// How often the worker polls the supervisor lane for `status` during
/// steady state — a still-answering authority's phase transition
/// (ruling (d)) must become visible promptly, but polling faster than
/// this buys nothing beyond load.
const LIVENESS_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// The worker's own message-loop tick — bounds how promptly a command
/// (input/resize/quit) is serviced and how often the pure tick-driven
/// timers (quit cutoff, checkpoint-in-flight retry, backoff) advance.
const WORKER_TICK: Duration = Duration::from_millis(100);
/// Prefix of the status `pump()` sets when `ClientEvent::Checkpoint`'s
/// own `restore_screen` fails — shared with the `ClientEvent::Status`
/// handler right below it, which must not let a stale, already-queued
/// "attached" silently overwrite this (Codex round on #194, finding 1).
const CHECKPOINT_RESTORE_FAILED_PREFIX: &str = "checkpoint restore failed";
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
// Small bounded I/O helpers over any `Endpoint::Client`, shared by every
// lane this module speaks (mirrors the pattern each platform's own
// `challenge()` itself uses: `crate::deadline::run_with_deadline` racing
// the blocking call against a `cancel()`-issuing watchdog).
// -----------------------------------------------------------------------

/// `pub(crate)`: shared with [`run_end_run_and_wait`]'s own callers (ADR
/// 0042 L1a, Codex review finding 4 — one lane-I/O error vocabulary, not
/// two).
#[derive(Debug)]
pub(crate) enum LaneError {
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
    // ERROR_ACCESS_DENIED == 5 is a Windows GetLastError code -- on Linux
    // os error 5 is EIO, unrelated, so the raw-code check must not apply
    // there (L1-unix LU3b: this function is now reachable from a Linux
    // build too). `ErrorKind::PermissionDenied` (std's own EACCES/EPERM
    // mapping) alone is both necessary and sufficient on Linux.
    (cfg!(windows) && e.raw_os_error() == Some(5)) || e.kind() == ErrorKind::PermissionDenied
}

pub(crate) fn write_bounded<E: Endpoint>(conn: &E::Client, bytes: &[u8], deadline: Instant) -> Result<(), LaneError> {
    match crate::deadline::run_with_deadline(deadline, || conn.cancel(), || conn.write_all(bytes)) {
        Some(Ok(())) => Ok(()),
        Some(Err(e)) => Err(LaneError::Io(transport_error_to_io(e))),
        None => Err(LaneError::Timeout),
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
/// that must be preserved for the caller's NEXT read. `pub(crate)`:
/// shared with [`run_end_run_and_wait`]'s own callers (ADR 0042 L1a).
pub(crate) struct FrameReader {
    splitter: wire::FrameSplitter,
    pending: VecDeque<DecodedFrame>,
}

impl FrameReader {
    pub(crate) fn new() -> Self {
        Self { splitter: wire::FrameSplitter::new(), pending: VecDeque::new() }
    }

    pub(crate) fn next_frame<E: Endpoint>(&mut self, conn: &E::Client, deadline: Instant) -> Result<DecodedFrame, LaneError> {
        if let Some(f) = self.pending.pop_front() {
            return Ok(f);
        }
        loop {
            let mut buf = [0u8; 8192];
            let n = match crate::deadline::run_with_deadline(deadline, || conn.cancel(), || conn.read(&mut buf)) {
                Some(Ok(n)) => n,
                Some(Err(e)) => return Err(LaneError::Io(transport_error_to_io(e))),
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
/// only), reusing the SAME primitives (`E::connect_supervisor_unchallenged`,
/// `E::challenge`, `SupervisorLaneExchange`) rather than depending on
/// that test-gated helper.
fn connect_supervisor_lane<E: Endpoint>(h: &str) -> Result<(E::Client, E::Process), LaneError> {
    let conn = E::connect_supervisor_unchallenged(h).map_err(|e| LaneError::Io(transport_error_to_io(e)))?;
    let mut exchange = SupervisorLaneExchange::new(SUPERVISOR_LANE_BUILD_ID);
    let deadline = Instant::now() + HELLO_BUDGET;
    match E::challenge(&conn, &mut exchange, deadline) {
        ChallengeOutcome::Proven(process) => Ok((conn, process)),
        // The shared challenge machinery folds "SID mismatch" (Windows) /
        // "not same-uid" (Linux) and "a well-formed WRONG reply" into the
        // SAME `Foreign` outcome — see this module's own doc and the
        // report's "Deviations" for why disambiguating THOSE would need
        // new machinery this unit prefers not to add. `exchange` itself
        // (ADR 0045 decision 7) already picks the ONE `Foreign` cause
        // that specifically means "another lane protocol" out of that
        // set — a genuine `hello_refused{version_skew}` from an
        // otherwise legitimate, same-account peer — so a caller can tell
        // it apart from every other unproven-server cause, which all
        // stay "foreign". Either way it is an unproven server: never
        // retried as if it might still be legitimate.
        ChallengeOutcome::Foreign => {
            if exchange.is_version_skew() {
                Err(LaneError::Protocol("supervisor hello: version_skew"))
            } else {
                Err(LaneError::Protocol("supervisor hello: foreign"))
            }
        }
        ChallengeOutcome::Undetermined => Err(LaneError::Protocol("supervisor hello: undetermined")),
    }
}

fn supervisor_status<E: Endpoint>(
    conn: &E::Client,
    reader: &mut FrameReader,
) -> Result<(Option<String>, Option<u64>, SupervisorPhase), LaneError> {
    let bytes = wire::encode_supervisor_request(&SupervisorRequest::Status)
        .expect("Status has no fields; encoding cannot fail");
    write_bounded::<E>(conn, &bytes, Instant::now() + WRITE_BUDGET)?;
    match reader.next_frame::<E>(conn, Instant::now() + STATUS_BUDGET)? {
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
/// attaching blind this round — the caller's own backoff (fixed pre-
/// attach interval, doubling only after a first attach) makes that a
/// brief, bounded gap, not a stall.
///
/// ADR 0043 decision 28: `voyage` is now OPTIONAL — the pointer is no
/// longer read as authoritative before the supervisor's own word (see
/// `converge_on_ready`'s pointer check, which runs AFTER `Ready`), so an
/// episode reaching this path may not have one yet. A missing pointer
/// starts the health accounting exactly as an absent supervisor does
/// (there is nothing to probe with, so this cannot distinguish "the
/// capsule survives headless" from "nothing exists yet" — both retry
/// under the same clock); an answered `Status` on a later round still
/// clears the unresponsive count via `ReconnectState::attached` or
/// `clear_unresponsive`, whichever path reaches it.
fn on_supervisor_absent_or_unresponsive<E: Endpoint>(
    reconnect: &mut ReconnectState,
    voyage: Option<&str>,
    now: Instant,
) -> ReconnectDecision {
    let Some(voyage) = voyage else {
        return reconnect.classify_unresponsive(now);
    };
    match E::connect_voyage_unchallenged(voyage) {
        Ok(_probe) => {
            reconnect.clear_unresponsive();
            ReconnectDecision::Retry
        }
        Err(e) => {
            if is_access_denied(&transport_error_to_io(e)) {
                reconnect.classify_access_denied()
            } else {
                reconnect.classify_unresponsive(now)
            }
        }
    }
}

/// What [`converge_on_ready`] concluded. `Ready` carries the SAME
/// supervisor-lane connection it was given back (the caller keeps using
/// it, first for the "voyage changed" reconciliation, then for a latched
/// `Quit` in the steady-state loop — no second connect+hello) plus the
/// voyage id [`pointer::validate`] confirmed against the supervisor's own
/// report AND the already-connected voyage lane itself — Codex review
/// round finding 1: the voyage-lane connect is now made INSIDE this
/// loop, one attempt per `Ready` round, so its own absence returns to
/// `Status` polling on the SAME connection instead of an independent
/// loop no health accounting or terminal classification ever reaches.
enum ReadyOutcome<E: Endpoint> {
    Ready { conn: E::Client, sup_reader: FrameReader, voyage_id: String, voyage_conn: E::Client },
    /// The connection stopped answering a `Status` request mid-poll — the
    /// caller falls back to path (i), presence/health accounting, exactly
    /// as an outright connect/hello failure would.
    LaneDown,
    Terminal(String),
    ShouldExit,
    Shutdown,
}

/// Non-blocking drain of every command already queued on `cmd_rx`,
/// applied right before the Ready/attach transition (Codex review round
/// finding 7): [`wait_for_retry_or_shutdown`] is the only OTHER place a
/// `Quit`/`Shutdown` gets read off this channel, and a round that never
/// waits (the supervisor already answers `Ready` with a valid pointer
/// and the voyage pipe connects on the first try) never calls it — a
/// `Quit` sent at that exact moment would sit unread through the whole
/// attach hello + checkpoint transfer, which is exactly the "quit must
/// never wait on the checkpoint" invariant (ruling (a)) this closes.
/// `Quit` is latched exactly as `wait_for_retry_or_shutdown` does;
/// `Shutdown` (or a disconnected channel) is reported for the caller to
/// act on immediately; a queued `Input`/`Resize` has no live attach
/// connection yet to act on and is dropped, matching
/// `wait_for_retry_or_shutdown`'s own documented behavior.
fn drain_pending_control(cmd_rx: &Receiver<WorkerMsg>, latched_quit_reason: &mut Option<String>) -> Option<WaitOutcome> {
    loop {
        match cmd_rx.try_recv() {
            Ok(WorkerMsg::Shutdown) => return Some(WaitOutcome::Shutdown),
            Ok(WorkerMsg::Quit(reason)) => {
                latched_quit_reason.get_or_insert(reason);
            }
            Ok(_) => {}
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => return Some(WaitOutcome::Shutdown),
        }
    }
}

/// ADR 0043 decision 28: the attach client converges on the supervisor's
/// OWN word, never on the voyage pipe. Given an already-connected,
/// already-`hello`'d supervisor lane, polls `Status` on that SAME
/// connection every [`fe_client::RECONNECT_BACKOFF_INITIAL`] (a FIXED
/// interval — Codex review round finding 6: this loop is steady-state
/// polling of a lane that is actively ANSWERING, never a reconnect
/// attempt, so [`ReconnectState::retry_with_backoff`]'s doubling — which
/// stays reserved for genuine reconnect waits in [`run_worker`]'s own
/// outer episode loop — never applies here) until the report says
/// `Ready` with a voyage id AND `drawer.voyage` on disk validates
/// against it AND the voyage lane itself accepts a connection. Every
/// answered `Status`, whatever its phase, clears
/// [`ReconnectState::clear_unresponsive`] (finding 2) — an outage that
/// already resolved must not keep aging through however many "still
/// starting" rounds follow. Three things happen INSIDE this loop, all
/// because they need only the supervisor lane and (once known) the
/// voyage id — never a second connect+hello of the supervisor lane
/// itself:
///
/// - A latched `Quit` dispatches the instant a voyage id is known,
///   BEFORE the `Ready` gate (ruling (a) — path (ii)): a quit must never
///   wait on a supervisor that is still starting. `run_quit` itself
///   blocks for as long as the ending transaction takes, so the `Status`
///   this round already read is stale by the time it returns — the loop
///   re-polls fresh rather than act on it.
/// - The pointer check (decision 28's own text): absent or naming
///   another voyage while this is the FIRST round reporting `Ready` (the
///   pointer may not have propagated yet, even though
///   `discover_or_mint_voyage` publishes it before `Ready` — a benign
///   observation race, not a fault) is "not yet", one more poll;
///   unchanged across TWO consecutive `Ready` rounds (both read through
///   this SAME top-of-loop `supervisor_status` call and its own Terminal
///   classification — Codex review round finding 9: no separate,
///   unclassified "recheck" round) is INCONSISTENT — a typed status,
///   reported once per spell and re-polled forever, never `Terminal` for
///   this case alone. `Corrupt`/`OtherIo` are unrelated malformed-content
///   or real I/O failures and stay loud, immediate stops, exactly as
///   before this lane.
/// - Once the pointer validates, ONE voyage-lane connect attempt
///   (finding 1): success finalizes `Ready`; an absent/refused pipe is
///   "not yet" and returns to polling `Status` on the SAME connection
///   (so a supervisor loss or terminal phase between rounds is caught by
///   the very next iteration's own classification, not invisible to
///   this loop the way an independent retry loop would leave it);
///   access-denied stays a loud, immediate stop.
///
/// There is NO client-side cutoff for "starting" — the supervisor's own
/// lifecycle deadlines are what eventually surface as `Terminal` (or a
/// respawned leg the next `Status` reports); this function invents none
/// of its own.
fn converge_on_ready<E: Endpoint>(
    mut conn: E::Client,
    mut sup_reader: FrameReader,
    state_dir: &Path,
    h: &str,
    cmd_rx: &Receiver<WorkerMsg>,
    reconnect: &mut ReconnectState,
    latched_quit_reason: &mut Option<String>,
    quit: &mut QuitDispatcher,
    outstanding: &mut OutstandingSlot,
    emit: &dyn Fn(ClientEvent),
) -> ReadyOutcome<E> {
    // Emitted at most once per "still starting" spell — re-armed every
    // time a latched Quit or a resolved pointer makes the NEXT status
    // worth announcing again as a fresh wait.
    let mut emitted_starting = false;
    // Emitted at most once per "inconsistent" spell, same idea.
    let mut emitted_inconsistent = false;
    // How many CONSECUTIVE `Ready` rounds (this connection, no reconnect
    // in between) have read a pointer that does not yet name this id --
    // two in a row is what decision 28 calls "an unchanged Ready", the
    // inconsistent case.
    let mut ready_pointer_mismatches: u32 = 0;

    loop {
        let (sv, _leg, phase) = match supervisor_status::<E>(&conn, &mut sup_reader) {
            Ok(v) => v,
            Err(_) => return ReadyOutcome::LaneDown,
        };
        // Finding 2: an answered Status, whatever its phase, proves the
        // supervisor lane is not the thing that is unresponsive right
        // now -- clear the clock unconditionally, before any Terminal
        // classification or gating below.
        reconnect.clear_unresponsive();
        if let ReconnectDecision::Terminal(reason) = reconnect.classify_supervisor_phase(phase) {
            return ReadyOutcome::Terminal(format!("supervisor: {reason:?}"));
        }

        // Path (ii): a latched Quit needs only the supervisor lane and a
        // known voyage id -- dispatched here, BEFORE the Ready gate
        // below, so a quit never waits on a supervisor that is still
        // starting.
        if let Some(id) = sv.clone() {
            if let Some(reason) = latched_quit_reason.take() {
                run_quit::<E>(&mut conn, &mut sup_reader, h, &id, reason, quit, outstanding, emit);
                if quit.should_exit() {
                    return ReadyOutcome::ShouldExit;
                }
                emitted_starting = false;
                emitted_inconsistent = false;
                ready_pointer_mismatches = 0;
                continue;
            }
        }

        let Some(id) = (phase == SupervisorPhase::Ready).then_some(sv).flatten() else {
            // Not yet Ready, or Ready with no voyage id yet (should not
            // normally happen -- `discover_or_mint_voyage` publishes
            // before `Ready` -- handled the same as "starting" rather
            // than assumed).
            ready_pointer_mismatches = 0;
            emitted_inconsistent = false;
            if !emitted_starting {
                emit(ClientEvent::Status("supervisor starting \u{2014} waiting\u{2026}".to_string()));
                emitted_starting = true;
            }
            match wait_for_retry_or_shutdown(cmd_rx, fe_client::RECONNECT_BACKOFF_INITIAL, latched_quit_reason) {
                WaitOutcome::Shutdown => return ReadyOutcome::Shutdown,
                WaitOutcome::Continue => continue,
            }
        };

        match pointer::validate(state_dir) {
            PointerState::Valid(pid) if pid == id => {
                ready_pointer_mismatches = 0;
                emitted_inconsistent = false;
                // Finding 7: drain and latch any control command already
                // queued before committing to the attach transition below
                // — a Quit that arrived while this round's Status/pointer
                // checks ran must never be allowed to sail through
                // unread.
                if let Some(WaitOutcome::Shutdown) = drain_pending_control(cmd_rx, latched_quit_reason) {
                    return ReadyOutcome::Shutdown;
                }
                if let Some(reason) = latched_quit_reason.take() {
                    run_quit::<E>(&mut conn, &mut sup_reader, h, &id, reason, quit, outstanding, emit);
                    if quit.should_exit() {
                        return ReadyOutcome::ShouldExit;
                    }
                    emitted_starting = false;
                    continue;
                }
                // Finding 1: the voyage-lane connect is ONE attempt per
                // round, folded into this SAME loop -- "not yet" returns
                // to Status polling above rather than an independent,
                // unbounded retry loop that never sees a supervisor
                // Terminal phase or health accounting again.
                match E::connect_voyage_unchallenged(&id) {
                    Ok(voyage_conn) => {
                        return ReadyOutcome::Ready { conn, sup_reader, voyage_id: id, voyage_conn };
                    }
                    Err(e) => {
                        let io = transport_error_to_io(e);
                        if is_access_denied(&io) {
                            return ReadyOutcome::Terminal("voyage pipe: access denied".to_string());
                        }
                        emit(ClientEvent::Status(format!("voyage pipe not yet available: {io}")));
                        match wait_for_retry_or_shutdown(cmd_rx, fe_client::RECONNECT_BACKOFF_INITIAL, latched_quit_reason) {
                            WaitOutcome::Shutdown => return ReadyOutcome::Shutdown,
                            WaitOutcome::Continue => continue,
                        }
                    }
                }
            }
            PointerState::Corrupt | PointerState::OtherIo(_) => {
                return ReadyOutcome::Terminal("drawer.voyage is corrupt \u{2014} retry or reset".to_string());
            }
            absent_or_mismatched => {
                ready_pointer_mismatches += 1;
                if ready_pointer_mismatches >= 2 && !emitted_inconsistent {
                    let detail = match absent_or_mismatched {
                        PointerState::NotFound => "absent".to_string(),
                        PointerState::Valid(other) => format!("names another voyage ({other})"),
                        PointerState::Corrupt | PointerState::OtherIo(_) => unreachable!("handled above"),
                    };
                    emit(ClientEvent::Status(format!(
                        "supervisor reports Ready but drawer.voyage is {detail}"
                    )));
                    emitted_inconsistent = true;
                }
                match wait_for_retry_or_shutdown(cmd_rx, fe_client::RECONNECT_BACKOFF_INITIAL, latched_quit_reason) {
                    WaitOutcome::Shutdown => return ReadyOutcome::Shutdown,
                    WaitOutcome::Continue => continue,
                }
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
fn capsule_identity_via_mgmt<E: Endpoint>(voyage: &str) -> Result<E::Process, LaneError> {
    let conn = E::connect_voyage_unchallenged(voyage).map_err(|e| LaneError::Io(transport_error_to_io(e)))?;
    let mut exchange = VoyageMgmtExchange::default();
    let deadline = Instant::now() + STATUS_BUDGET;
    match E::challenge(&conn, &mut exchange, deadline) {
        ChallengeOutcome::Proven(process) => Ok(process),
        ChallengeOutcome::Foreign => Err(LaneError::Protocol("voyage mgmt: foreign")),
        ChallengeOutcome::Undetermined => Err(LaneError::Protocol("voyage mgmt: undetermined")),
    }
}

// -----------------------------------------------------------------------
// The attach lane: hello (proto only) + attach + checkpoint reassembly.
// -----------------------------------------------------------------------

/// The outcome of one `hello` round trip, distinguishing "negotiated,
/// proceed" from "refused, but retry the whole episode at a version this
/// client also speaks" from a hard failure (see
/// [`attach_lane_hello`]'s own doc).
enum HelloOutcome {
    Accepted,
    /// Refused with a `supported` version this client can also speak --
    /// today, only ever [`wire::ATTACH_PROTO_V1`] (an older capsule
    /// build, predating the scrollback ring). The caller should retry
    /// the WHOLE episode at this version: the capsule closes this
    /// connection right after refusing it (`ReplyThenClose`), so there
    /// is no connection left to retry the hello ON.
    RetryAt(u32),
}

/// Sends `hello{proto}` and interprets the reply. `proto` is the version
/// THIS attempt asks for; a caller wanting the retry-on-refusal behavior
/// loops the whole episode (a fresh connection) rather than recursing
/// here.
///
/// ADR 0041 "attach proto v2 bound to checkpoint v2" (Codex round on
/// #194): a `hello_ok` must echo back EXACTLY the version it negotiated,
/// never a different one -- silently trusting a mismatch would mean
/// assuming a checkpoint shape the capsule never actually promised.
fn attach_lane_hello<E: Endpoint>(
    conn: &E::Client,
    reader: &mut FrameReader,
    proto: u32,
) -> Result<HelloOutcome, LaneError> {
    let bytes =
        wire::encode_attach_client(&AttachClient::Hello { proto }).expect("fixed hello shape");
    write_bounded::<E>(conn, &bytes, Instant::now() + WRITE_BUDGET)?;
    match reader.next_frame::<E>(conn, Instant::now() + HELLO_BUDGET)? {
        DecodedFrame::AttachServer(AttachServer::HelloOk { proto: negotiated }) => {
            if negotiated != proto {
                return Err(LaneError::Protocol(
                    "attach hello_ok: accepted an unrequested proto version",
                ));
            }
            Ok(HelloOutcome::Accepted)
        }
        DecodedFrame::AttachServer(AttachServer::HelloRefused { supported }) => {
            if proto != wire::ATTACH_PROTO_V1 && supported == wire::ATTACH_PROTO_V1 {
                Ok(HelloOutcome::RetryAt(wire::ATTACH_PROTO_V1))
            } else {
                Err(LaneError::Protocol("attach hello: version_skew"))
            }
        }
        _ => Err(LaneError::Protocol("expected attach hello_ok")),
    }
}

/// Clamps a per-frame checkpoint-collection deadline to the aggregate
/// transfer deadline, so a run of per-frame budgets can never together
/// exceed it — pure and unit-tested on its own (the transfer's actual
/// I/O cannot be driven from a plain unit test) because it is the one
/// piece of [`attach_and_collect_checkpoint`]'s loop that fixes Codex
/// round on #194 finding 3.
fn checkpoint_frame_deadline(now: Instant, transfer_deadline: Instant) -> Instant {
    (now + STATUS_BUDGET).min(transfer_deadline)
}

/// Sends `attach{controller_id}` (always arrives as a WATCHER — ADR
/// 0037's who-may-type) and reassembles the checkpoint transfer, bounded
/// at [`wire::MAX_CHECKPOINT_LEN`] the same way `tests/e2e_pipe.rs`'s own
/// `RealFrames::collect_checkpoint` proves the property, and at
/// [`CHECKPOINT_TRANSFER_BUDGET`] in aggregate (see its own doc).
fn attach_and_collect_checkpoint<E: Endpoint>(
    conn: &E::Client,
    reader: &mut FrameReader,
    controller_id: &str,
) -> Result<Vec<u8>, LaneError> {
    let bytes = wire::encode_attach_client(&AttachClient::Attach { controller_id: controller_id.to_string() })
        .map_err(LaneError::Wire)?;
    write_bounded::<E>(conn, &bytes, Instant::now() + WRITE_BUDGET)?;
    let mut out = Vec::new();
    let transfer_deadline = Instant::now() + CHECKPOINT_TRANSFER_BUDGET;
    loop {
        let frame_deadline = checkpoint_frame_deadline(Instant::now(), transfer_deadline);
        match reader.next_frame::<E>(conn, frame_deadline)? {
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
///
/// L1-unix LU3b: generic over `E: Endpoint`, defaulted to
/// [`PlatformEndpoint`] — every real caller (the frontend) names this
/// type unparameterised (`FeAttachClient`) and gets whatever `Endpoint`
/// this build's own platform speaks; a test naming a different `E`
/// still compiles the same struct. No field actually stores an
/// `E::Client`/`E::Process` (the connection lives inside the worker
/// thread's own stack, moved into its closure at [`attach`](Self::attach)
/// time) — `E` is carried only as a marker so `attach` knows which
/// [`run_worker`] to spawn.
pub struct FeAttachClient<E: Endpoint = PlatformEndpoint> {
    _endpoint: PhantomData<E>,
    parser: vt100_ctt::Parser,
    /// The pane's current `(rows, cols)` — the CALLER's rect, tracked
    /// independently of whatever size a just-restored checkpoint carries.
    /// A capsule's checkpoint reflects the capsule's own PTY dimensions
    /// (its `build_run_command` default of 80x24 until the capsule is
    /// actually resized), which is not necessarily the pane's current
    /// rect: a WATCHER cannot correct the capsule's geometry until it
    /// holds the pen (`resize`'s own doc), so the first checkpoint after
    /// attach — and any checkpoint from a reattach whose pane rect moved
    /// while disconnected — can arrive at a size that disagrees with the
    /// rect the renderer actually paints into. `pump`'s `Checkpoint` arm
    /// reflows the restored screen to this size right after every
    /// restore, so the render never depends on the wire-level resize
    /// (gated on holding the pen) completing first — see that arm's own
    /// comment for why `Screen::set_size`'s deterministic pad/clip is
    /// enough on its own, no protocol change needed.
    pane_size: (u16, u16),
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
    /// LU6a: `true` once the `Checkpoint` arm of `pump` has applied a
    /// checkpoint to `parser` (set right after `restore_screen`,
    /// regardless of whether that restore itself succeeded — the
    /// checkpoint EVENT still landed either way, and there is only ever
    /// one per attach episode). The caller (`gpu.rs`'s session pane) uses
    /// this to know when it may stop holding the pane's previous content
    /// and paint this client's own screen instead — see
    /// `pane_screen_choice` there.
    checkpointed: bool,
    /// switch-latency Phase 1: `true` only when the MOST RECENT
    /// `Checkpoint` event's own `restore_screen` call actually succeeded
    /// — unlike `checkpointed` above (which stays `true` after a failed
    /// restore, by design), this reflects the latest attempt honestly so
    /// a caller's instrumentation can tell "attached, and rendering what
    /// it received" from "attached, but the restore itself failed" (see
    /// [`Self::restore_ok`]).
    restore_ok: bool,
    /// Codex review round, finding 7: the SHARED half of the byte-account
    /// (the episode reader thread, spawned inside the worker, holds the
    /// other `Arc` clone and increments this on every `Output` byte it
    /// reads, blocking further reads while at cap — `CheckpointChunk`
    /// bytes are never counted here: they are consumed by
    /// `attach_and_collect_checkpoint`, on the attach connection's SAME
    /// `FrameReader`, before this reader thread even exists). `pump`
    /// decrements it as it actually consumes `Output` bytes
    /// — the ONLY place this counter is ever decremented, which is what
    /// makes the accounting real (the first landing incremented and
    /// immediately decremented in the SAME reader-thread call, which
    /// Codex review round correctly called a no-op). See [`QueuedBytes`]
    /// for the notified wait this decrement wakes.
    queued_bytes: Arc<QueuedBytes>,
    /// Codex review round, finding 10: markers `pump` receives land here
    /// (never re-drained from the same channel a second time, which
    /// silently ate whatever non-marker event happened to be next in
    /// line). `drain_fe_down_markers` drains ONLY this queue.
    pending_fe_down_markers: VecDeque<serde_json::Value>,
    /// ADR 0042 amendment (2026-09-07): `true` for [`Self::attach_headless`]
    /// — a client with no viewport. Read only by [`Self::pump`]'s
    /// `Checkpoint` arm: a headless client adopts the checkpoint's own
    /// geometry as [`Self::pane_size`] instead of reflowing the restored
    /// screen TO `pane_size` (there is no real viewport size to reflow to).
    /// The take transaction's own headless behavior (never sending
    /// `Resize`) is a separate, independent flag on [`fe_client::
    /// TakeTransaction`] itself, chosen by [`run_worker`] at construction —
    /// this field never reaches that decision directly.
    headless: bool,
    /// Sum of the byte lengths of every `input` this client has seen
    /// `InputRecorded` for (ADR 0042 amendment: "success requires
    /// `InputRecorded` covering the WHOLE payload, not a bare ack
    /// counter" — a caller compares this against the length it sent,
    /// rather than trusting a single increment-only tick). Shared with the
    /// worker thread, which is the only writer.
    recorded_bytes: Arc<AtomicU64>,
    /// The wire's terminal answer to the MOST RECENT `input` this client
    /// sent (`Recorded` / `RefusedStale` / `DeliveryUnknown` — ADR 0041's
    /// own "three terminal answers," restated as [`InputOutcome`]). `None`
    /// until the first outcome arrives. Shared with the worker thread
    /// (the only writer); a `Mutex` rather than an atomic encoding because
    /// this is written and read at most a few times per client lifetime
    /// (never a hot path) and a 3-variant enum has no natural atomic
    /// representation worth inventing one for.
    last_input_outcome: Arc<Mutex<Option<InputOutcome>>>,
    /// ADR 0042 amendment: the worker thread's own handle, for
    /// [`Self::shutdown`]'s bounded join — the ONLY reason this is stored
    /// (a previous Codex review round correctly deleted it as dead weight
    /// when nothing ever joined it; `shutdown` is that first real caller).
    /// `Option` so `shutdown` can `.take()` it out of a `&mut self` without
    /// needing `self` by value merely to move a field.
    worker_handle: Option<thread::JoinHandle<()>>,
}

/// The wire's terminal answer to ONE `input` frame this client sent (ADR
/// 0041: "the wire defines three terminal answers"), exposed as an
/// observable a caller (the headless daemon client) can poll — "the
/// smallest honest observable," not a new [`ClientEvent`] (ADR 0042
/// amendment review: "no bare ack counter... success requires
/// `InputRecorded` covering the whole payload").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputOutcome {
    /// `input_recorded`: the record has it.
    Recorded,
    /// `input_refused_stale`: the epoch changed; THIS client's own worker
    /// re-takes automatically (ADR 0041 ruling (c)), but a headless caller
    /// treats the ORIGINAL send as failed and does not wait for that retry
    /// (ADR 0042 amendment: "the daemon NEVER retries an input on its
    /// own").
    RefusedStale,
    /// `input_delivery_unknown`: "never auto-retried... dropped and marked
    /// visibly unknown" — the record's own verdict is unknowable from here.
    DeliveryUnknown,
}

impl<E: Endpoint> FeAttachClient<E> {
    /// Reads `drawer.voyage` under `state_dir` and starts the background
    /// worker; the worker itself performs the connect/hello/status/
    /// attach/checkpoint sequence and every reconnect thereafter — this
    /// constructor never blocks on the network, matching
    /// `LocalTerminal::spawn`'s own "returns once the reader thread is
    /// running" contract. `state_dir` is the CALLER's resolved value
    /// (`state_dir::sot_state_dir()` for the real frontend; an isolated
    /// tempdir for `tests/fe_client.rs`) — this constructor takes it
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
    ) -> Result<Self, FeAttachError>
    where
        E::Client: 'static,
    {
        Self::attach_inner(
            state_dir,
            cols,
            rows,
            controller_id,
            fe_down_to_handle,
            fe_down_last_evidence,
            wake,
            false,
        )
    }

    /// ADR 0042 amendment (2026-09-07): a HEADLESS attach — the daemon's
    /// own `pty.input`/`pty.screen` client on a capsule row
    /// (`capsule_workspace::headless`), never the frontend. No viewport
    /// (`cols`/`rows` are a placeholder until the checkpoint lands — see
    /// [`Self::pump`]'s `Checkpoint` arm), no `fe_down` marker (this client
    /// makes exactly one attach in its short lifetime, and
    /// [`FeDownBaseline::marker_for_attach`]'s own "skipped on a first
    /// attach" rule means `fe_down_last_evidence: None` here is never
    /// observed to matter), no real wake (the caller pumps on its own
    /// clock, not an event loop). The take transaction this spawns is
    /// [`fe_client::TakeTransaction::new_headless`] — it sends no `Resize`,
    /// ever.
    pub fn attach_headless(
        state_dir: PathBuf,
        controller_id: String,
    ) -> Result<Self, FeAttachError>
    where
        E::Client: 'static,
    {
        // Placeholder viewport: wholesale-replaced by the first checkpoint
        // restore regardless (`pump`'s `Checkpoint` arm), and this client
        // adopts the checkpoint's OWN size into `pane_size` rather than
        // reflowing to this one (`headless: true` below) — the exact
        // number here is never rendered or reported.
        const PLACEHOLDER_SIZE: u16 = 24;
        Self::attach_inner(
            state_dir,
            PLACEHOLDER_SIZE,
            PLACEHOLDER_SIZE,
            controller_id.clone(),
            controller_id,
            None,
            Box::new(|| {}),
            true,
        )
    }

    fn attach_inner(
        state_dir: PathBuf,
        cols: u16,
        rows: u16,
        controller_id: String,
        fe_down_to_handle: String,
        fe_down_last_evidence: Option<String>,
        wake: Box<dyn Fn() + Send + 'static>,
        headless: bool,
    ) -> Result<Self, FeAttachError>
    where
        E::Client: 'static,
    {
        let rows = rows.max(2);
        let cols = cols.max(2);
        let parser = vt100_ctt::Parser::new(rows, cols, SCROLLBACK_ROWS);

        let (msg_tx, msg_rx) = mpsc::channel::<WorkerMsg>();
        let (events_tx, events_rx) = mpsc::channel::<ClientEvent>();
        let worker_msg_tx = msg_tx.clone();
        let fe_down = FeDownBaseline::capture(fe_down_last_evidence);
        let queued_bytes = Arc::new(QueuedBytes::new());
        let worker_queued_bytes = Arc::clone(&queued_bytes);
        let recorded_bytes = Arc::new(AtomicU64::new(0));
        let worker_recorded_bytes = Arc::clone(&recorded_bytes);
        let last_input_outcome = Arc::new(Mutex::new(None));
        let worker_last_input_outcome = Arc::clone(&last_input_outcome);

        let worker_handle = thread::Builder::new()
            .name("sot-fe-attach-worker".to_string())
            .spawn(move || {
                run_worker::<E>(
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
                    worker_recorded_bytes,
                    worker_last_input_outcome,
                    headless,
                    wake,
                );
            })
            .map_err(FeAttachError::SpawnWorkerThread)?;

        Ok(Self {
            _endpoint: PhantomData,
            parser,
            pane_size: (rows, cols),
            msg_tx,
            events_rx,
            status: "connecting\u{2026}".to_string(),
            notice: None,
            quit_message: None,
            should_exit: false,
            dead: false,
            checkpointed: false,
            restore_ok: false,
            queued_bytes,
            pending_fe_down_markers: VecDeque::new(),
            headless,
            recorded_bytes,
            last_input_outcome,
            worker_handle: Some(worker_handle),
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
                    // switch-latency Phase 1: a fresh checkpoint starts a
                    // new attach episode against a (possibly different)
                    // leg, so any notice left over from the PREVIOUS one
                    // is retracted here rather than left standing until a
                    // new `Notice` event replaces it. The attach notice
                    // is emitted only after this checkpoint (the
                    // mgmt-lane identity lookup it depends on runs
                    // afterward — see `run_worker`'s own comment at that
                    // reorder), so without this clear a reconnect from
                    // leg A to leg B would render B's freshly restored
                    // screen under A's stale notice text for as long as
                    // that lookup takes.
                    self.notice = None;
                    let restore_result = self.parser.restore_screen(&bytes);
                    // switch-latency Phase 1: recorded separately from
                    // `checkpointed` below (which is set unconditionally
                    // either way) so a caller can tell "did apply a
                    // checkpoint" from "did apply it SUCCESSFULLY" — see
                    // `restore_ok`'s own doc.
                    self.restore_ok = restore_result.is_ok();
                    if let Err(e) = restore_result {
                        self.status = format!("{CHECKPOINT_RESTORE_FAILED_PREFIX}: {e:?}");
                    } else if self.headless {
                        // ADR 0042 amendment: a headless client has no
                        // viewport to reflow TO — adopt the checkpoint's
                        // OWN dimensions as `pane_size` instead (never
                        // `set_size`, which would pad/clip it to whatever
                        // placeholder `attach_headless` was constructed
                        // with). This is also what keeps the take
                        // transaction's own `Resize` a true no-op honest:
                        // there is no local geometry disagreement to
                        // correct in the first place.
                        self.pane_size = self.parser.screen().size();
                    } else {
                        // `restore_screen` REPLACES the parser's screen
                        // wholesale with one sized to the checkpoint's own
                        // encoded dimensions (`Screen::restore`, vt100 crate)
                        // — not this parser's construction size, and not
                        // `pane_size`. Reflow it to the pane's current rect
                        // immediately, rather than waiting on the wire-level
                        // resize: that only reaches the capsule once this
                        // client holds the pen (`resize`'s own doc), which a
                        // fresh WATCHER never does before its first paint.
                        // `Screen::set_size` pads/clips deterministically
                        // (`vt100::grid::Grid::set_size`), so this is a pure
                        // local reflow with no protocol involvement — the
                        // actual capsule-side resize still happens exactly
                        // as before, via the take-on-first-input handshake.
                        let (rows, cols) = self.pane_size;
                        self.parser.screen_mut().set_size(rows, cols);
                    }
                    // LU6a: right after `restore_screen`, success or not —
                    // see `checkpointed`'s own doc for why a failed restore
                    // still counts (the checkpoint EVENT landed either way,
                    // and there is only ever one per episode).
                    self.checkpointed = true;
                    changed = true;
                }
                Ok(ClientEvent::Output(bytes)) => {
                    drained_output_bytes += bytes.len();
                    // The ONLY decrement of the shared byte-account — see
                    // this struct's own `queued_bytes` doc. Wakes the
                    // reader if it is parked in `wait_below_cap`.
                    self.queued_bytes.sub(bytes.len());
                    self.parser.process(&bytes);
                    changed = true;
                }
                Ok(ClientEvent::Notice(text)) => {
                    self.notice = Some(text);
                    changed = true;
                }
                Ok(ClientEvent::Status(text)) => {
                    // The worker queues `Status("attached")` unconditionally
                    // right behind the checkpoint bytes -- it does not
                    // itself know whether the LOCAL restore will succeed,
                    // that only happens above, foreground-side, in the
                    // SAME drain (Codex round on #194, finding 1). A
                    // client that could not render what it received is
                    // not honestly "attached"; do not let this stale
                    // success overwrite the failure that was just set.
                    let stale_attached_after_failed_restore = text == "attached"
                        && self.status.starts_with(CHECKPOINT_RESTORE_FAILED_PREFIX);
                    if !stale_attached_after_failed_restore {
                        self.status = text;
                    }
                    changed = true;
                }
                Ok(ClientEvent::Terminal(text)) => {
                    // ADR 0030 §8 "Where it is shown": mirror the reason to
                    // `tracing` here too, not only `self.status` (which a
                    // caller must poll) -- this is the ONE place `self.dead`
                    // ever becomes true, so it fires exactly once per
                    // episode, same as every other one-shot log line in
                    // this file.
                    tracing::warn!(reason = %text, "fe attach client: reached a terminal state");
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
    ///
    /// The LOCAL screen is reflowed right here, unconditionally — mirrors
    /// `term::LocalTerminal::resize`'s own "resize both the PTY and the
    /// vt100 parser so their grids stay in sync," except the capsule side
    /// of that pair is the wire resize above (protocol-gated on holding
    /// the pen, unlike a local PTY spawn). Updating `pane_size` here is
    /// also what makes a later reattach renegotiate: the next checkpoint
    /// this episode restores — whatever size the capsule's own PTY
    /// happens to carry — is reflowed to THIS size in `pump`'s
    /// `Checkpoint` arm, not to the size the pane happened to be at the
    /// original `attach` call.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let rows = rows.max(2);
        let cols = cols.max(2);
        self.pane_size = (rows, cols);
        self.parser.screen_mut().set_size(rows, cols);
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

    /// `true` once the FIRST `ClientEvent::Checkpoint` has been applied —
    /// restore ATTEMPTED, whether or not it succeeded (a failed restore
    /// still sets [`Self::status`] to the `CHECKPOINT_RESTORE_FAILED_PREFIX`
    /// text, which a caller polling this should also check). ADR 0042
    /// amendment: the headless client's own "pump until attached" loop
    /// condition — a watcher's checkpoint arrives exactly once per attach,
    /// so this never resets after the first `true`.
    pub fn is_checkpointed(&self) -> bool {
        self.checkpointed
    }

    /// `true` only when the MOST RECENT `Checkpoint` event's own
    /// `restore_screen` call actually succeeded — switch-latency Phase
    /// 1's own addition, distinguishing "attached and rendering it" from
    /// [`Self::is_checkpointed`]'s weaker "a checkpoint arrived, whether
    /// or not it could be applied" (that one stays `true` after a failed
    /// restore, by design — see its own doc). `false` before any
    /// `Checkpoint` event has landed at all.
    pub fn restore_ok(&self) -> bool {
        self.restore_ok
    }

    /// Sum of the byte lengths of every `input` `InputRecorded` for so
    /// far. ADR 0042 amendment's own observable: a caller compares this
    /// (before vs. after sending) against the length it sent, rather than
    /// trusting a bare increment-only ack counter.
    pub fn recorded_bytes(&self) -> u64 {
        self.recorded_bytes.load(Ordering::Acquire)
    }

    /// The wire's terminal answer to the most recent `input` this client
    /// sent — see [`InputOutcome`]'s own doc. A poisoned lock (a prior
    /// panic while holding it) reads as `None` rather than panicking here
    /// too: a caller polling this in a loop must never itself become the
    /// second panic.
    pub fn last_input_outcome(&self) -> Option<InputOutcome> {
        self.last_input_outcome.lock().ok().and_then(|g| *g)
    }

    /// Sends `Shutdown` (same as [`Drop`] does) and waits UP TO `wait` for
    /// the worker thread to actually exit, polling [`thread::JoinHandle::
    /// is_finished`] rather than an unbounded `join()` — ADR 0042
    /// amendment: "on EVERY exit path: drop the client, then observe the
    /// worker's closure." Returns `true` iff the worker exited within the
    /// bound; `false` logs a `warn` and leaves the handle for `Drop` to
    /// forget about (a `JoinHandle` that is never joined does not leak the
    /// thread — it simply runs to completion on its own, same as today).
    /// Callable at most meaningfully once — a second call after the first
    /// already took the handle returns `true` (nothing left to wait for).
    pub fn shutdown(&mut self, wait: Duration) -> bool {
        let _ = self.msg_tx.send(WorkerMsg::Shutdown);
        let Some(handle) = self.worker_handle.take() else {
            return true;
        };
        let deadline = Instant::now() + wait;
        while !handle.is_finished() {
            if Instant::now() >= deadline {
                eprintln!(
                    "fe_client_io: worker thread still alive {wait:?} after Shutdown was sent"
                );
                return false;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = handle.join();
        true
    }
}

impl<E: Endpoint> Drop for FeAttachClient<E> {
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
fn run_worker<E: Endpoint>(
    state_dir: PathBuf,
    controller_id: String,
    fe_down_to_handle: String,
    mut fe_down: FeDownBaseline,
    initial_cols: u16,
    initial_rows: u16,
    cmd_rx: Receiver<WorkerMsg>,
    msg_tx: Sender<WorkerMsg>,
    events_tx: Sender<ClientEvent>,
    queued_bytes: Arc<QueuedBytes>,
    recorded_bytes: Arc<AtomicU64>,
    last_input_outcome: Arc<Mutex<Option<InputOutcome>>>,
    headless: bool,
    wake: Box<dyn Fn() + Send + 'static>,
) where
    E::Client: 'static,
{
    let h = state_dir_hash(&state_dir);
    let mut reconnect = ReconnectState::new();
    let mut take = if headless { TakeTransaction::new_headless() } else { TakeTransaction::new() };
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
    // ADR 0041 "attach proto v2 bound to checkpoint v2" (Codex round on
    // #194): the version THIS episode's `hello` asks for. Starts at the
    // newest this build speaks; a `hello_refused` naming a version this
    // client ALSO speaks (only ever `ATTACH_PROTO_V1`, an older capsule)
    // downgrades this and retries the whole episode immediately, rather
    // than failing outright -- survives across `continue 'episodes` on
    // purpose, unlike the per-episode locals above it.
    let mut preferred_attach_proto = wire::ATTACH_PROTO_V2;
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

        // --- supervisor lane: hello (build identity) once, then converge
        // on Ready (ADR 0043 decision 28) -------------------------------
        let supervisor_ready = match connect_supervisor_lane::<E>(&h) {
            Ok((conn, _proven)) => {
                match converge_on_ready::<E>(
                    conn,
                    FrameReader::new(),
                    &state_dir,
                    &h,
                    &cmd_rx,
                    &mut reconnect,
                    &mut latched_quit_reason,
                    &mut quit,
                    &mut outstanding,
                    &emit,
                ) {
                    ReadyOutcome::Ready { conn, sup_reader, voyage_id, voyage_conn } => {
                        Some((conn, sup_reader, voyage_id, voyage_conn))
                    }
                    ReadyOutcome::Terminal(msg) => {
                        emit(ClientEvent::Terminal(msg));
                        return;
                    }
                    ReadyOutcome::ShouldExit => {
                        emit(ClientEvent::ShouldExit);
                        return;
                    }
                    ReadyOutcome::Shutdown => break 'episodes,
                    ReadyOutcome::LaneDown => None,
                }
            }
            Err(LaneError::Protocol(p)) if p.contains("version_skew") => {
                match reconnect.classify_hello_refused_version_skew() {
                    ReconnectDecision::Terminal(_) => {
                        emit(ClientEvent::Terminal(
                            "supervisor speaks another lane protocol \u{2014} end the row from a client of its own build, or kill only its supervise process and attach again".to_string(),
                        ));
                        return;
                    }
                    ReconnectDecision::Retry => unreachable!("classify_hello_refused_version_skew is always terminal"),
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
        let (supervisor_conn, mut sup_reader, voyage, voyage_conn) = match supervisor_ready {
            Some(v) => v,
            None => {
                // Finding 2: resolved FRESH here, not cached from the top
                // of the episode -- `converge_on_ready` may have polled
                // for a long while before reporting `LaneDown`/failing to
                // connect at all, during which `discover_or_mint_voyage`
                // could have published the pointer this health check
                // needs. `Corrupt`/`OtherIo` remain loud, immediate
                // stops -- malformed content or a real I/O failure, never
                // "not yet" -- exactly as before this lane; only
                // `NotFound` degrades to the missing-pointer case the
                // health check already treats like an absent supervisor.
                let health_pointer = match pointer::validate(&state_dir) {
                    PointerState::Valid(id) => Some(id),
                    PointerState::NotFound => None,
                    PointerState::Corrupt | PointerState::OtherIo(_) => {
                        emit(ClientEvent::Terminal(
                            "drawer.voyage is absent or corrupt \u{2014} retry or reset".to_string(),
                        ));
                        return;
                    }
                };
                match on_supervisor_absent_or_unresponsive::<E>(&mut reconnect, health_pointer.as_deref(), Instant::now()) {
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
        };

        if voyage_uuid.as_deref() != Some(voyage.as_str()) {
            // A reset landed underneath us: any outstanding input from
            // the OLD voyage is canceled, never replayed into the new
            // one -- and reported, never silently (finding 6). Keyed on
            // the id `converge_on_ready` confirmed against the
            // supervisor's own `Status` reply, not a locally cached
            // pointer read.
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

        // --- attach lane: the voyage pipe is already connected --
        // `converge_on_ready` made that ONE attempt itself (finding 1),
        // folded into its own Status-polling loop rather than an
        // independent retry here.
        let attach_identity = match E::authenticate_server(&voyage_conn) {
            PeerAuthOutcome::Authenticated(a) => (a.pid, a.created),
            PeerAuthOutcome::Foreign => {
                emit(ClientEvent::Terminal("voyage pipe: foreign".to_string()));
                return;
            }
            PeerAuthOutcome::Undetermined => {
                match wait_for_retry_or_shutdown(&cmd_rx, reconnect.retry_with_backoff(), &mut latched_quit_reason) {
                    WaitOutcome::Shutdown => break 'episodes,
                    WaitOutcome::Continue => continue 'episodes,
                }
            }
        };

        let mut attach_reader = FrameReader::new();
        match attach_lane_hello::<E>(&voyage_conn, &mut attach_reader, preferred_attach_proto) {
            Ok(HelloOutcome::Accepted) => {}
            Ok(HelloOutcome::RetryAt(fallback)) => {
                // The capsule does not speak `preferred_attach_proto` (an
                // older build, predating attach proto v2 / the
                // scrollback ring) but DOES speak a version this client
                // also understands. Retry the whole episode immediately
                // -- a fresh connection, since the refused one is already
                // closed server-side -- at that version rather than
                // failing outright: v1 still works, just without
                // history.
                preferred_attach_proto = fallback;
                continue 'episodes;
            }
            Err(e) => match e {
                LaneError::Protocol(p) if p.contains("version_skew") => {
                    emit(ClientEvent::Terminal(
                        "attach hello: version_skew".to_string(),
                    ));
                    return;
                }
                _ => {
                    match wait_for_retry_or_shutdown(
                        &cmd_rx,
                        reconnect.retry_with_backoff(),
                        &mut latched_quit_reason,
                    ) {
                        WaitOutcome::Shutdown => break 'episodes,
                        WaitOutcome::Continue => continue 'episodes,
                    }
                }
            },
        }
        let checkpoint =
            match attach_and_collect_checkpoint::<E>(&voyage_conn, &mut attach_reader, &controller_id) {
                Ok(c) => c,
                Err(_) => {
                    match wait_for_retry_or_shutdown(&cmd_rx, reconnect.retry_with_backoff(), &mut latched_quit_reason) {
                        WaitOutcome::Shutdown => break 'episodes,
                        WaitOutcome::Continue => continue 'episodes,
                    }
                }
            };

        // switch-latency Phase 1: the checkpoint (and the "attached"
        // status right behind it) now land BEFORE the mgmt-identity
        // round trip below, not after. That round trip is a THROWAWAY
        // connection + full challenge purely to word the attach notice —
        // best-effort commentary on an already-attached client, never a
        // precondition for showing one — and it used to sit ahead of the
        // checkpoint emit, delaying first paint by a whole extra
        // connection + challenge for no reason the client's own state
        // needed.
        emit(ClientEvent::Checkpoint(checkpoint));
        emit(ClientEvent::Status("attached".to_string()));
        reconnect.attached();

        // Ruling (e), Codex review round finding 9: the attach notice
        // compares the CAPSULE's own identity (a throwaway voyage
        // mgmt-lane challenge) against the attach connection's own
        // challenge-proven identity -- never the supervisor's. On
        // mismatch, re-read the mgmt identity once and proceed without a
        // notice
        // rather than looping forever.
        let mgmt_identity = capsule_identity_via_mgmt::<E>(&voyage)
            .ok()
            .map(|p| (p.pid(), p.created()))
            .or_else(|| capsule_identity_via_mgmt::<E>(&voyage).ok().map(|p| (p.pid(), p.created())));
        if let Some(mgmt_leg) = mgmt_identity {
            if fe_client::legs_match(mgmt_leg, attach_identity) {
                emit(ClientEvent::Notice(fe_client::attach_notice_text(&format!("{}", mgmt_leg.1))));
            }
        }

        // Ruling (b), Codex review round finding 4: a `not_attached`
        // reattach preserves the take transaction instead of resetting
        // it -- re-issue `take` for the SAME still-queued bytes now that
        // a fresh checkpoint has landed.
        if preserve_take_on_reconnect {
            preserve_take_on_reconnect = false;
            for action in take.retry_take() {
                apply_single_take_action::<E>(action, &voyage_conn, &controller_id, &emit);
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
                        apply_single_take_action::<E>(action, &voyage_conn, &controller_id, &emit);
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
            .spawn(move || run_attach_reader::<E>(reader_conn, attach_reader, reader_tx, reader_queued_bytes, reader_stop))
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
        let episode_result = run_steady_state::<E>(
            &cmd_rx,
            &events_tx,
            &wake,
            &h,
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
            &recorded_bytes,
            &last_input_outcome,
        );

        // Tear down this episode's connections before deciding what's
        // next. The stop flag interrupts the reader's OWN backpressure
        // wait (a `QueuedBytes` condvar wait, not a blocked read --
        // `cancel()` alone cannot reach it); `notify_stop` wakes it
        // promptly since the plain `store` above has nothing else to
        // make a parked `Condvar::wait` notice it. `cancel()` then
        // unblocks a blocked read so the thread observes an error, sends
        // the now-moot `ReaderDone` (harmlessly ignored; a fresh reader
        // is not spawned until the next successful attach), and exits;
        // only then do both `Arc` clones drop and the pipe handle
        // actually closes.
        episode_stop.store(true, Ordering::Release);
        queued_bytes.notify_stop();
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

/// Steady heartbeat cadence for `run_quit`'s own loop, chosen against a
/// CONFIRMED read of U2's own idle-eviction code (`supervisor.rs`'s
/// `service_lane`/`handle_lane_bytes`): a connection's idle clock
/// (`Conn::last_activity`, evicted past `LANE_IDLE_DEADLINE` = 5 s) is
/// reset in EXACTLY ONE place — `handle_lane_bytes`'s own
/// `conn.last_activity = now;`, which runs only when the SERVER
/// RECEIVES a `TransportEvent::Bytes` from the CLIENT. The SERVER
/// SENDING a reply (`TransportEvent::Sent`) never touches it — that
/// event only drives the unrelated `pending_close`/refusal-flush
/// bookkeeping. So a client that writes `end_run` and then only READS,
/// waiting for the deferred reply, is indistinguishable from an idle
/// connection to the SERVER, which can close it mid-teardown before the
/// reply it is computing ever goes out — confirmed by real-Windows
/// evidence on `285ad0d9`: `RecordClosed` arrived, `Verifying` began,
/// then nothing for 60 s (a 2 s-budget read-then-query design still left
/// up to ~2 s of silence between attempts, and every subsequent `query`
/// write was swallowed by `let _ = write_bounded(..)`, so the eviction
/// was never even detected). 1 s gives 5x margin under the 5 s deadline.
pub(crate) const QUIT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);

/// Submits `end_run` on the (already-connected) supervisor lane, THEN
/// LOOPS — entirely within this call — until `quit` reaches a terminal
/// state (`Ended`/`Failed`/`Refused`/`OutcomeUnknown`), sending
/// `query { operation_id }` every [`QUIT_HEARTBEAT_INTERVAL`] and never
/// blocking a read past that same interval (see the constant's own doc
/// for why: ONLY outbound bytes reset the supervisor lane's idle clock,
/// so a read that outlasts the heartbeat is exactly the silence that
/// gets this connection evicted mid-wait).
///
/// This is the THIRD pass (first real-Windows run diagnostics): the
/// second pass's read-then-query-on-timeout design still left gaps wide
/// enough to lose the connection under a slow enough teardown, and, once
/// lost, could not recover — every write after that point was silently
/// discarded. This pass never trusts the CURRENT connection to still be
/// good: any write error, `Eof`, `Io`, or read timeout on it
/// unconditionally RECONNECTS the supervisor lane via the caller-supplied
/// `reconnect` and continues querying the SAME durable `operation_id` —
/// safe because the authority's own operation journal (ADR 0041
/// Lifecycle: "`operation_id` is durable for MUTATING ops") is exactly
/// what a fresh `hello` and `query` can read back, so there is nothing
/// about the OLD connection worth preserving. No write in this loop is
/// ever swallowed (`let _ = write_bounded(..)` silently discarding a
/// failure is what let the second pass spin for 60 s against a
/// connection already gone) — every write's own result feeds the SAME
/// reconnect decision a read failure does. `tick` against `quit`'s own
/// `fe_client::QUIT_CUTOFF` (90 s, ADR 0041's own bound-graph figure)
/// each iteration is the ONLY terminal bound: a lane that cannot be
/// reconnected within it still yields `OutcomeUnknown`.
///
/// `reconnect` now reports success as a `bool` (Codex review round
/// finding 3): once ADR 0043 decision 27 made an absent endpoint fail
/// fast on both transports, a FAILED reconnect attempt returns almost
/// instantly rather than eating `CONNECT_BOUND` — which used to be this
/// loop's own accidental throttle. A `false` return sleeps
/// `QUIT_HEARTBEAT_INTERVAL` before the next tick/attempt so a
/// genuinely-gone supervisor paces its retries at roughly one per
/// second, comfortably completing many attempts within the 90 s cutoff,
/// instead of spinning as fast as the CPU allows and flooding stderr.
///
/// Idempotent — a `quit` already in flight (Ending/Verifying/terminal)
/// makes this a no-op, so applying a LATCHED quit at the top of a fresh
/// episode can never double-fire.
///
/// `on_transition` fires once right after the initial send, once per
/// operation-state reply, and once more at the terminal return —
/// [`run_quit`] (this module's own FE caller) passes a closure that
/// emits `QuitMessage` events at exactly those points, unchanged from
/// before this function was extracted; a caller with no UI (ADR 0042
/// L1a's `supervisor_client::end_run`, the daemon's own caller) passes a
/// no-op. Shared by both — ONE lane-operation loop (Codex review finding
/// 4), not two: `supervisor_client::end_run` no longer carries its own
/// state machine or invented budget.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_end_run_and_wait<E: Endpoint>(
    conn: &mut E::Client,
    reader: &mut FrameReader,
    mut reconnect: impl FnMut(&mut E::Client, &mut FrameReader) -> bool,
    quit: &mut QuitDispatcher,
    operation_id: String,
    reason: String,
    voyage: &str,
    mut on_transition: impl FnMut(&QuitDispatcher),
) {
    if !quit.request_quit(operation_id.clone(), Instant::now()) {
        return; // already ending/verifying/terminal -- idempotent
    }
    let cmd = SupervisorRequest::Command {
        operation_id: operation_id.clone(),
        op: SupervisorOp::EndRun { reason, voyage: voyage.to_string() },
    };
    match wire::encode_supervisor_request(&cmd) {
        Ok(bytes) => {
            if let Err(e) = write_bounded::<E>(conn, &bytes, Instant::now() + WRITE_BUDGET) {
                // Not swallowed (Codex review round): a failed write here
                // is exactly as recoverable as one later -- the loop
                // below reconnects and queries the SAME operation_id,
                // which the authority's own journal already has.
                eprintln!("lane end_run: write failed ({e}); reconnecting and querying");
                reconnect(conn, reader);
            }
        }
        Err(e) => eprintln!("lane end_run: encode failed ({e}); will query for its outcome anyway"),
    }
    on_transition(quit);

    let mut last_query_sent_at: Option<Instant> = None;
    loop {
        let now = Instant::now();
        quit.tick(now);
        let terminal = matches!(
            quit.state(),
            QuitState::Ended | QuitState::Failed { .. } | QuitState::Refused { .. } | QuitState::OutcomeUnknown
        );
        if terminal {
            on_transition(quit);
            return;
        }

        let heartbeat_due = last_query_sent_at
            .map(|t| now.duration_since(t) >= QUIT_HEARTBEAT_INTERVAL)
            .unwrap_or(true);
        if heartbeat_due {
            last_query_sent_at = Some(now);
            let q = SupervisorRequest::Query { operation_id: operation_id.clone() };
            match wire::encode_supervisor_request(&q) {
                Ok(bytes) => {
                    if let Err(e) = write_bounded::<E>(conn, &bytes, Instant::now() + WRITE_BUDGET) {
                        eprintln!("lane end_run: query write failed ({e}); reconnecting");
                        if !reconnect(conn, reader) {
                            // Finding 3: an absent endpoint now fails fast
                            // (decision 27) instead of eating
                            // `CONNECT_BOUND` -- without this sleep, a
                            // genuinely-gone supervisor spins this loop as
                            // fast as the CPU allows.
                            thread::sleep(QUIT_HEARTBEAT_INTERVAL);
                        }
                        continue;
                    }
                }
                Err(e) => eprintln!("lane end_run: query encode failed ({e}); will retry next heartbeat"),
            }
        }

        match reader.next_frame::<E>(conn, now + QUIT_HEARTBEAT_INTERVAL) {
            Ok(DecodedFrame::SupervisorReply(SupervisorReply::Operation(state))) => {
                eprintln!("lane end_run: operation state {state:?}");
                quit.on_operation_state(state);
                on_transition(quit);
            }
            Ok(other) => {
                eprintln!("lane end_run: unrelated frame while waiting: {other:?}");
            }
            Err(e) => {
                // Timeout, EOF, or a wire error alike: the ruling this
                // pass implements treats ALL of them as "this connection
                // can no longer be trusted" -- reconnect unconditionally
                // rather than try to distinguish a merely-slow reply
                // from a silently-evicted connection, which is exactly
                // the distinction the second pass got wrong.
                eprintln!("lane end_run: read failed ({e}); reconnecting");
                if !reconnect(conn, reader) {
                    // Finding 3, same reasoning as the write-failure arm
                    // above: pace retries instead of spinning.
                    thread::sleep(QUIT_HEARTBEAT_INTERVAL);
                }
            }
        }
    }
}

/// The FE's own wrapping around [`run_end_run_and_wait`]: cancels any
/// outstanding input (Ruling (c), Codex review round finding 6 — never
/// dropped silently) before the transaction starts, and emits
/// `QuitMessage` events at every transition. The quit path never touches
/// the ATTACH connection at all — the capsule's own pipe going away is
/// exactly what a successful `end_run` does, so depending on it (as the
/// steady-state loop that used to poll `query` did) can never be correct
/// here.
#[allow(clippy::too_many_arguments)]
fn run_quit<E: Endpoint>(
    supervisor_conn: &mut E::Client,
    sup_reader: &mut FrameReader,
    h: &str,
    voyage: &str,
    reason: String,
    quit: &mut QuitDispatcher,
    outstanding: &mut OutstandingSlot,
    emit: &dyn Fn(ClientEvent),
) {
    let operation_id = format!("fe-quit-{}", uuid::Uuid::now_v7());
    // Mirrors `run_end_run_and_wait`'s own `request_quit` gate: this is
    // the SAME "is state currently Idle" condition that function checks
    // moments later, read here (nothing else mutates `quit` in between)
    // so the outstanding-input cancel — a ONE-TIME side effect — fires
    // only on the call that actually starts the ending transaction.
    if matches!(quit.state(), QuitState::Idle) {
        if let Some(o) = outstanding.cancel_for_quit() {
            emit(ClientEvent::Status(format!(
                "input canceled by quit \u{2014} {} byte(s) not confirmed",
                o.bytes.len()
            )));
        }
    }
    run_end_run_and_wait::<E>(
        supervisor_conn,
        sup_reader,
        |conn, reader| reconnect_supervisor_lane_for_quit::<E>(conn, reader, h),
        quit,
        operation_id,
        reason,
        voyage,
        |quit| emit(ClientEvent::QuitMessage(quit.message())),
    );
}

/// Reconnects the supervisor lane in place, for [`run_quit`]'s own use.
/// Best-effort and silent-on-failure BY DESIGN (beyond the one stderr
/// line): the caller's own loop simply tries again next iteration,
/// bounded overall by `QuitDispatcher::tick`'s 90 s cutoff -- there is
/// no separate retry budget to manage here, unlike the reconnect EPISODE
/// loop the rest of this module drives for the attach lane.
fn reconnect_supervisor_lane_for_quit<E: Endpoint>(supervisor_conn: &mut E::Client, sup_reader: &mut FrameReader, h: &str) -> bool {
    match connect_supervisor_lane::<E>(h) {
        Ok((conn, _proven)) => {
            *supervisor_conn = conn;
            *sup_reader = FrameReader::new();
            eprintln!("fe-client quit: reconnected the supervisor lane");
            true
        }
        Err(e) => {
            eprintln!("fe-client quit: reconnect failed ({e}); will retry");
            false
        }
    }
}

// -----------------------------------------------------------------------
// Backpressure accounting shared between the episode reader and `pump`
// -----------------------------------------------------------------------

/// The byte-account behind Codex review round finding 7 ("the FE STOPS
/// READING THE PIPE"): the episode reader (`run_attach_reader`)
/// increments it on every `Output` byte it reads (`CheckpointChunk`
/// bytes are never counted — those are consumed earlier, by
/// `attach_and_collect_checkpoint` on the same connection, before this
/// reader exists) and blocks its own next `read()` while it is at or above
/// [`READER_QUEUE_CAP_BYTES`]; [`FeAttachClient::pump`] decrements it as
/// it actually consumes `Output` bytes — the ONLY place it is ever
/// decremented, which is what makes the accounting real (see
/// [`FeAttachClient::queued_bytes`]'s own doc).
///
/// switch-latency Phase 1: the reader used to enforce the block with a
/// `thread::sleep(20ms)` poll loop. The count itself stays a plain
/// `AtomicUsize` — `add`/`load` need no lock, and stay on the hot path
/// `pump` and the reader already run on every `Output` byte — but
/// [`Self::wait_below_cap`] parks on a `Condvar` instead of polling,
/// woken by [`Self::sub`] the instant draining brings the count back
/// under the cap, or by [`Self::notify_stop`] at episode teardown. The
/// `Mutex`/`Condvar` pair here is a pure doorbell, the same shape
/// `deadline::run_with_deadline` uses for its own watchdog: the waiter
/// re-checks the real condition (the atomic count, or `stop`) every time
/// it wakes, under the SAME lock a notifier must hold to signal, so a
/// notify that lands between the waiter's check and its park can never
/// be lost — see [`Self::wait_below_cap`] for the full argument.
struct QueuedBytes {
    count: AtomicUsize,
    gate: Mutex<()>,
    room: Condvar,
}

impl QueuedBytes {
    fn new() -> Self {
        Self { count: AtomicUsize::new(0), gate: Mutex::new(()), room: Condvar::new() }
    }

    /// The reader's own increment — paired with [`Self::sub`].
    fn add(&self, n: usize) {
        self.count.fetch_add(n, Ordering::AcqRel);
    }

    /// `pump`'s own decrement, the ONLY place this ever goes down. Wakes
    /// any reader parked in [`Self::wait_below_cap`] — a no-op lock/
    /// notify when nobody is waiting (the common case: the queue rarely
    /// reaches [`READER_QUEUE_CAP_BYTES`] at all).
    fn sub(&self, n: usize) {
        self.count.fetch_sub(n, Ordering::AcqRel);
        drop(self.gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));
        self.room.notify_all();
    }

    fn load(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }

    /// Blocks the calling (reader) thread until either the count drops
    /// below `cap` or `stop` is set — woken by [`Self::sub`]'s notify or
    /// by [`Self::notify_stop`], never a periodic poll. Every wake
    /// re-checks both real conditions before deciding to park again, so
    /// a notify racing the initial check is never lost: it can only land
    /// while this thread holds `gate` (already past the point where it
    /// would go on to wait) or while it is genuinely parked inside
    /// `Condvar::wait` (where it is, by definition, listening).
    fn wait_below_cap(&self, cap: usize, stop: &AtomicBool) {
        self.wait_below_cap_traced(cap, stop, || {})
    }

    /// Same as [`Self::wait_below_cap`], plus `on_wake` — called once
    /// per loop iteration (initial entry, and again each time
    /// `Condvar::wait` returns). Lets tests count how many times this
    /// waits wakes instead of trusting wall-clock latency (which
    /// scheduler noise on a loaded CI runner makes an unreliable witness
    /// either way) to tell a genuinely notified wait apart from a poll.
    fn wait_below_cap_traced(&self, cap: usize, stop: &AtomicBool, on_wake: impl Fn()) {
        let mut guard = self.gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            on_wake();
            if self.load() < cap || stop.load(Ordering::Acquire) {
                return;
            }
            guard = self.room.wait(guard).unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    /// Wakes a reader parked in [`Self::wait_below_cap`] because `stop`
    /// was just set on the same `AtomicBool` it polls — called once, at
    /// episode teardown, immediately after that store (an `AtomicBool`
    /// write alone has nothing to make a parked `Condvar::wait` notice
    /// it).
    fn notify_stop(&self) {
        drop(self.gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));
        self.room.notify_all();
    }
}

// -----------------------------------------------------------------------
// The episode-scoped attach-connection reader
// -----------------------------------------------------------------------

/// Blocking read, decode via its own `FrameSplitter`, forward each frame
/// to the worker. Gates its OWN next `read()` call on the SHARED
/// [`QueuedBytes`] counter (Codex review round, finding 7: "the FE STOPS
/// READING THE PIPE" is now real — this is the SAME `Arc` clone
/// [`FeAttachClient::pump`] decrements, not a private, immediately-
/// released one). `stop` breaks the backpressure wait itself (a notified
/// wait `cancel()` cannot reach); a normal teardown sets it and calls
/// [`QueuedBytes::notify_stop`] just before calling `cancel()`.
/// `Keepalive` is answered directly here (bounced back byte-identical),
/// never round-tripped through the worker.
fn run_attach_reader<E: Endpoint>(
    conn: Arc<E::Client>,
    mut reader: FrameReader,
    tx: Sender<WorkerMsg>,
    queued_bytes: Arc<QueuedBytes>,
    stop: Arc<AtomicBool>,
) {
    loop {
        queued_bytes.wait_below_cap(READER_QUEUE_CAP_BYTES, &stop);
        if stop.load(Ordering::Acquire) {
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(3600); // steady-state: no artificial read deadline; EOF/cancel end it
        let frame = match reader.next_frame::<E>(&conn, deadline) {
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
            queued_bytes.add(bytes.len());
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
fn run_steady_state<E: Endpoint>(
    cmd_rx: &Receiver<WorkerMsg>,
    events_tx: &Sender<ClientEvent>,
    wake: &(dyn Fn() + Send),
    h: &str,
    attach_conn: &Arc<E::Client>,
    supervisor_conn: &mut E::Client,
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
    recorded_bytes: &Arc<AtomicU64>,
    last_input_outcome: &Arc<Mutex<Option<InputOutcome>>>,
) -> SteadyOutcome {
    let emit = |e: ClientEvent| {
        let _ = events_tx.send(e);
        wake();
    };

    loop {
        match cmd_rx.recv_timeout(WORKER_TICK) {
            Ok(WorkerMsg::Shutdown) => return SteadyOutcome::Shutdown,
            Ok(WorkerMsg::Input(bytes)) => match take.role() {
                Role::Watching => {
                    for action in take.on_input_while_watching(&bytes) {
                        apply_single_take_action::<E>(action, attach_conn, controller_id, &emit);
                    }
                }
                Role::Taking | Role::Resizing => {
                    for action in take.on_input_while_pending(&bytes) {
                        apply_single_take_action::<E>(action, attach_conn, controller_id, &emit);
                    }
                }
                Role::Driving => {
                    if outstanding.outstanding().is_some() {
                        // Ruling (b), Codex review round finding 5: an
                        // input already outstanding queues the next one
                        // rather than dropping it.
                        for action in take.queue_while_driving(&bytes) {
                            apply_single_take_action::<E>(action, attach_conn, controller_id, &emit);
                        }
                    } else {
                        send_new_input::<E>(attach_conn, outstanding, *take_epoch, controller_id, voyage, bytes);
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
                        let _ = write_bounded::<E>(attach_conn, &enc, Instant::now() + WRITE_BUDGET);
                    }
                }
            }
            Ok(WorkerMsg::Quit(reason)) => {
                // `run_quit` now loops internally (entirely within this
                // call) until the dispatcher reaches a terminal state --
                // see its own doc for why verification cannot depend on
                // this steady-state loop running again (the attach
                // connection dies with the capsule end_run tears down).
                run_quit::<E>(supervisor_conn, sup_reader, h, voyage, reason, quit, outstanding, &emit);
            }
            Ok(WorkerMsg::Frame(frame)) => {
                match handle_attach_frame::<E>(
                    frame, attach_conn, take, take_intent, outstanding, take_epoch, controller_id, voyage, *cols,
                    *rows, &emit, recorded_bytes, last_input_outcome,
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
        if quit.should_exit() {
            return SteadyOutcome::QuitEnded;
        }
        for action in take.tick_checkpoint_retry(now) {
            apply_single_take_action::<E>(action, attach_conn, controller_id, &emit);
        }

        if now.duration_since(*last_liveness_poll) >= LIVENESS_POLL_INTERVAL {
            *last_liveness_poll = now;
            match supervisor_status::<E>(supervisor_conn, sup_reader) {
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

fn send_wire_input<E: Endpoint>(attach_conn: &E::Client, controller_id: &str, take_epoch: u64, idem_key: [u8; 16], payload: Vec<u8>) {
    let frame = AttachClient::Input { controller_id: controller_id.to_string(), take_epoch, idem_key, payload };
    if let Ok(enc) = wire::encode_attach_client(&frame) {
        let _ = write_bounded::<E>(attach_conn, &enc, Instant::now() + WRITE_BUDGET);
    }
}

/// Records a FRESH outstanding input (a new idem key) and sends it —
/// the ordinary path for both a first Driving-idle keystroke and a
/// flushed queue entry.
fn send_new_input<E: Endpoint>(
    attach_conn: &E::Client,
    outstanding: &mut OutstandingSlot,
    take_epoch: u64,
    controller_id: &str,
    voyage: &str,
    bytes: Vec<u8>,
) {
    let idem_key = outstanding.record(voyage.to_string(), take_epoch, bytes.clone(), mint_idem_key);
    send_wire_input::<E>(attach_conn, controller_id, take_epoch, idem_key, bytes);
}

/// Dispatches one `TakeAction`. `SendInput` no longer exists as a
/// variant (Codex review round, finding 3: flushing the queue is never
/// bundled with `take_ok`'s own actions) — every input send in this
/// module goes through [`send_new_input`]/[`send_wire_input`] instead,
/// called from the specific points ruling (b)/(c) pin (after
/// `resize_ok`, after an outstanding reply resolves while DRIVING).
fn apply_single_take_action<E: Endpoint>(action: TakeAction, attach_conn: &E::Client, controller_id: &str, emit: &dyn Fn(ClientEvent)) {
    match action {
        TakeAction::SendTake => {
            let frame = AttachClient::Take { controller_id: controller_id.to_string() };
            if let Ok(enc) = wire::encode_attach_client(&frame) {
                let _ = write_bounded::<E>(attach_conn, &enc, Instant::now() + WRITE_BUDGET);
            }
        }
        TakeAction::SendResize { cols, rows } => {
            let frame = AttachClient::Resize { cols, rows };
            if let Ok(enc) = wire::encode_attach_client(&frame) {
                let _ = write_bounded::<E>(attach_conn, &enc, Instant::now() + WRITE_BUDGET);
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
fn flush_after_pen_secured<E: Endpoint>(
    attach_conn: &E::Client,
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
                send_wire_input::<E>(attach_conn, controller_id, o.take_epoch, o.idem_key, o.bytes.clone());
            }
        }
        TakeIntent::StaleRetry => {
            let resolution = outstanding.apply_outcome(InputWireOutcome::RefusedStale, take_epoch, mint_idem_key);
            if let fe_client::OutstandingResolution::RetryNewEpoch { idem_key } = resolution {
                if let Some(o) = outstanding.outstanding() {
                    send_wire_input::<E>(attach_conn, controller_id, take_epoch, idem_key, o.bytes.clone());
                }
            }
        }
        TakeIntent::Ordinary => {
            if let Some(bytes) = take.take_queued() {
                send_new_input::<E>(attach_conn, outstanding, take_epoch, controller_id, voyage, bytes);
            }
        }
    }
}

/// After an outstanding input's reply resolves (`InputRecorded`/
/// `InputDeliveryUnknown`) while still DRIVING: flush whatever the take
/// transaction queued behind it (ruling (b), Codex review round finding
/// 5's own "dispatch queued bytes after the outstanding reply").
fn flush_next_driving_input<E: Endpoint>(
    attach_conn: &E::Client,
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
        send_new_input::<E>(attach_conn, outstanding, take_epoch, controller_id, voyage, bytes);
    }
}

/// Dispatches one incoming attach-lane frame (unsolicited `Output` or a
/// reply to whatever the worker most recently sent).
#[allow(clippy::too_many_arguments)]
fn handle_attach_frame<E: Endpoint>(
    frame: DecodedFrame,
    attach_conn: &E::Client,
    take: &mut TakeTransaction,
    take_intent: &mut TakeIntent,
    outstanding: &mut OutstandingSlot,
    take_epoch: &mut u64,
    controller_id: &str,
    voyage: &str,
    cols: u16,
    rows: u16,
    emit: &dyn Fn(ClientEvent),
    recorded_bytes: &Arc<AtomicU64>,
    last_input_outcome: &Arc<Mutex<Option<InputOutcome>>>,
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
                apply_single_take_action::<E>(action, attach_conn, controller_id, emit);
            }
            // A HEADLESS take's own `on_take_ok` (above) skips RESIZING
            // entirely and promotes straight to DRIVING, returning no
            // actions — the ordinary `ResizeOk` frame that would normally
            // trigger the queue flush never arrives for it, so flush right
            // here whenever `on_take_ok` already landed in DRIVING. A
            // NON-headless transaction never reaches DRIVING from this
            // call (it always lands in RESIZING), so this is a no-op for
            // the frontend's own client.
            if take.role() == Role::Driving {
                flush_after_pen_secured::<E>(attach_conn, take, take_intent, outstanding, *take_epoch, controller_id, voyage);
            }
            FrameOutcome::Handled
        }
        DecodedFrame::AttachServer(AttachServer::TakeRefused { reason }) => {
            match reason {
                TakeRefusedReason::NotAttached => {
                    let actions = take.on_take_refused_not_attached();
                    let reattach = actions.contains(&TakeAction::Reattach);
                    for action in actions {
                        apply_single_take_action::<E>(action, attach_conn, controller_id, emit);
                    }
                    if reattach {
                        return FrameOutcome::ReattachRequested;
                    }
                }
                TakeRefusedReason::CheckpointInFlight => {
                    for action in take.on_take_refused_checkpoint_in_flight(Instant::now()) {
                        apply_single_take_action::<E>(action, attach_conn, controller_id, emit);
                    }
                }
            }
            FrameOutcome::Handled
        }
        DecodedFrame::AttachServer(AttachServer::ResizeOk) => {
            take.on_resize_ok();
            flush_after_pen_secured::<E>(attach_conn, take, take_intent, outstanding, *take_epoch, controller_id, voyage);
            FrameOutcome::Handled
        }
        DecodedFrame::AttachServer(AttachServer::ResizeRefused { reason }) => {
            let was_resizing = take.role() == Role::Resizing;
            for action in take.on_resize_refused(reason) {
                apply_single_take_action::<E>(action, attach_conn, controller_id, emit);
            }
            if was_resizing && reason == ResizeRefusedReason::OutOfBudget {
                // `on_resize_refused` already promoted RESIZING ->
                // DRIVING for this refusal -- the pen is still held, so
                // whatever was queued behind the take-ok flushes exactly
                // as it would after a real `resize_ok`.
                flush_after_pen_secured::<E>(attach_conn, take, take_intent, outstanding, *take_epoch, controller_id, voyage);
            }
            FrameOutcome::Handled
        }
        DecodedFrame::AttachServer(AttachServer::InputRecorded) => {
            // Capture the length BEFORE `apply_outcome` clears it — ADR
            // 0042 amendment's own observable: "success requires
            // `InputRecorded` covering the WHOLE payload," which a caller
            // verifies by summing recorded lengths, not by counting acks.
            let recorded_len = outstanding.outstanding().map(|o| o.bytes.len());
            let _ = outstanding.apply_outcome(InputWireOutcome::Recorded, *take_epoch, mint_idem_key);
            if let Some(len) = recorded_len {
                recorded_bytes.fetch_add(len as u64, Ordering::AcqRel);
            }
            if let Ok(mut g) = last_input_outcome.lock() {
                *g = Some(InputOutcome::Recorded);
            }
            flush_next_driving_input::<E>(attach_conn, take, outstanding, *take_epoch, controller_id, voyage);
            FrameOutcome::Handled
        }
        DecodedFrame::AttachServer(AttachServer::InputRefusedStale) => {
            // Ruling (c), Codex review round finding 6: re-take FIRST;
            // the new key is minted once the fresh `take_ok` arrives
            // (see the `TakeOk` arm above and `flush_after_pen_secured`'s
            // own `StaleRetry` handling). ADR 0042 amendment: a headless
            // caller treats THIS send as failed and does not wait for that
            // retry (the daemon never retries an input on its own) — the
            // retry below is this client's own ordinary wire-protocol
            // behavior, unrelated to whatever the headless caller does
            // once it observes the outcome recorded here.
            if let Ok(mut g) = last_input_outcome.lock() {
                *g = Some(InputOutcome::RefusedStale);
            }
            *take_intent = TakeIntent::StaleRetry;
            for action in take.retake_while_driving() {
                apply_single_take_action::<E>(action, attach_conn, controller_id, emit);
            }
            FrameOutcome::Handled
        }
        DecodedFrame::AttachServer(AttachServer::InputDeliveryUnknown) => {
            let res = outstanding.apply_outcome(InputWireOutcome::DeliveryUnknown, *take_epoch, mint_idem_key);
            if matches!(res, fe_client::OutstandingResolution::Unknown) {
                emit(ClientEvent::Status("input delivery unknown".to_string()));
                if let Ok(mut g) = last_input_outcome.lock() {
                    *g = Some(InputOutcome::DeliveryUnknown);
                }
            }
            flush_next_driving_input::<E>(attach_conn, take, outstanding, *take_epoch, controller_id, voyage);
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

// -----------------------------------------------------------------------
// Pure-logic unit tests. Most of this module's behavior needs a real
// supervisor + capsule process to attach to (`tests/fe_client.rs`'s own
// real-process harness -- L1-unix LU3c ungated it to run on Linux too,
// against a real socket lane, exactly like Windows); these pieces are pure
// enough to test directly on every platform this module now compiles on.
// LU6a's `checkpointed` test is the one exception: it builds an
// `FeAttachClient` by hand (a private-field struct literal, legal from
// this child module) rather than a real worker thread, so it can drive
// `pump` -- the SAME path a real worker's events go through -- with a
// synthetic `ClientEvent::Checkpoint` and no process/network dependency.
// -----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Codex round on #194, finding 3: a per-frame deadline that keeps
    /// re-arming itself, forever, is not a bound at all. This proves the
    /// clamp both ways -- an aggregate deadline far in the future never
    /// shortens an ordinary per-frame budget, and one that has already
    /// arrived is what a faulty capsule dripping a technically-legal
    /// frame every `STATUS_BUDGET` eventually runs into.
    #[test]
    fn checkpoint_frame_deadline_never_exceeds_the_aggregate_one() {
        let now = Instant::now();
        let far_off = now + Duration::from_secs(3600);
        assert_eq!(checkpoint_frame_deadline(now, far_off), now + STATUS_BUDGET);

        let already_here = now + Duration::from_millis(1);
        assert_eq!(checkpoint_frame_deadline(now, already_here), already_here);
    }

    /// Sanity on the constant itself: finite, and generous enough to
    /// cover at least one ordinary frame -- a zero or absurdly small
    /// budget would defeat its own purpose (refusing a checkpoint that
    /// could otherwise complete in time).
    #[test]
    fn checkpoint_transfer_budget_is_a_real_bound() {
        assert!(CHECKPOINT_TRANSFER_BUDGET >= STATUS_BUDGET);
        assert!(CHECKPOINT_TRANSFER_BUDGET <= Duration::from_secs(300));
    }

    /// LU6a: a fresh client reports `is_checkpointed() == false`, and
    /// `true` once a `Checkpoint` event has gone through `pump` -- the
    /// SAME arm a real worker's checkpoint event drains through. Built by
    /// hand rather than via `attach` (which spawns a real worker thread
    /// and needs a real state dir/lane): this module's own child-module
    /// privacy lets the struct literal reach every private field, and
    /// `pump` neither knows nor cares whether `events_tx` belongs to a
    /// worker thread or a test.
    #[test]
    fn checkpoint_event_marks_the_client_checkpointed() {
        let (events_tx, events_rx) = mpsc::channel();
        let (msg_tx, _msg_rx) = mpsc::channel();
        let mut client = FeAttachClient::<PlatformEndpoint> {
            _endpoint: PhantomData,
            parser: vt100_ctt::Parser::new(24, 80, 100),
            pane_size: (24, 80),
            msg_tx,
            events_rx,
            status: "connecting\u{2026}".to_string(),
            notice: None,
            quit_message: None,
            should_exit: false,
            dead: false,
            checkpointed: false,
            restore_ok: false,
            queued_bytes: Arc::new(QueuedBytes::new()),
            pending_fe_down_markers: VecDeque::new(),
            headless: false,
            recorded_bytes: Arc::new(AtomicU64::new(0)),
            last_input_outcome: Arc::new(Mutex::new(None)),
            worker_handle: None,
        };
        assert!(!client.is_checkpointed(), "a fresh client must not report checkpointed");
        assert!(!client.restore_ok(), "a fresh client must not report a successful restore");

        let bytes = vt100_ctt::Parser::new(24, 80, 100)
            .screen()
            .checkpoint()
            .expect("encode a checkpoint of a fresh, in-range screen");
        events_tx.send(ClientEvent::Checkpoint(bytes)).expect("send a synthetic checkpoint event");
        client.pump();
        assert!(
            client.is_checkpointed(),
            "pump()'s Checkpoint arm must mark the client checkpointed"
        );
        assert!(client.restore_ok(), "a well-formed checkpoint must restore successfully");
    }

    /// switch-latency Phase 1: `restore_ok` must go `false`, even though
    /// `is_checkpointed` still goes `true` (LU6a's own "the checkpoint
    /// EVENT landed either way") -- the exact gap `restore_ok` exists to
    /// close for a caller's instrumentation.
    #[test]
    fn checkpoint_event_with_undecodable_bytes_marks_checkpointed_but_not_restore_ok() {
        let (events_tx, events_rx) = mpsc::channel();
        let (msg_tx, _msg_rx) = mpsc::channel();
        let mut client = FeAttachClient::<PlatformEndpoint> {
            _endpoint: PhantomData,
            parser: vt100_ctt::Parser::new(24, 80, 100),
            pane_size: (24, 80),
            msg_tx,
            events_rx,
            status: "connecting\u{2026}".to_string(),
            notice: None,
            quit_message: None,
            should_exit: false,
            dead: false,
            checkpointed: false,
            restore_ok: false,
            queued_bytes: Arc::new(QueuedBytes::new()),
            pending_fe_down_markers: VecDeque::new(),
            headless: false,
            recorded_bytes: Arc::new(AtomicU64::new(0)),
            last_input_outcome: Arc::new(Mutex::new(None)),
            worker_handle: None,
        };

        events_tx
            .send(ClientEvent::Checkpoint(vec![0xff; 4]))
            .expect("send a synthetic, undecodable checkpoint event");
        client.pump();
        assert!(client.is_checkpointed(), "the checkpoint EVENT still landed");
        assert!(!client.restore_ok(), "a failed restore must not report restore_ok");
    }

    /// switch-latency Phase 1, via `wait_below_cap_traced`'s wake
    /// counter: proves `wait_below_cap` is genuinely NOTIFIED rather than
    /// polled, without depending on wall-clock latency (a loose bound
    /// would admit the old 20ms poll just as easily as the new wait, and
    /// a tight one can miss on a loaded CI runner regardless of which
    /// mechanism is really running). The waiter parks for a real 200ms
    /// before `sub` drains it below the cap; ideally it wakes exactly
    /// twice: once at entry (parks, since the count is still at the cap)
    /// and once more when `sub`'s notify runs. A 20ms poll loop, by
    /// contrast, would wake roughly 200ms / 20ms ~= 10 times over the
    /// same stretch.
    #[test]
    fn queued_bytes_wait_below_cap_wakes_a_bounded_number_of_times_when_released_by_a_drain() {
        let q = Arc::new(QueuedBytes::new());
        q.add(10);
        let stop = Arc::new(AtomicBool::new(false));
        let wakes = Arc::new(AtomicUsize::new(0));

        let waiter_q = Arc::clone(&q);
        let waiter_stop = Arc::clone(&stop);
        let waiter_wakes = Arc::clone(&wakes);
        let waiter = thread::spawn(move || {
            waiter_q.wait_below_cap_traced(10, &waiter_stop, || {
                waiter_wakes.fetch_add(1, Ordering::SeqCst);
            });
        });

        thread::sleep(Duration::from_millis(200));
        q.sub(1); // 9 < 10: below the cap
        waiter.join().expect("waiter thread must not panic");

        let wake_count = wakes.load(Ordering::SeqCst);
        assert!(
            wake_count <= 6,
            "waiter woke {wake_count} times across a 200ms hold -- expected entry plus one \
             notified wake (2, with slack for a loaded CI runner's own spurious wakeups), not \
             a 20ms poll cadence (which would be ~10)"
        );
    }

    /// The other release path, proven the same way: `stop` alone (via
    /// `notify_stop`) must unblock a waiter that would otherwise stay
    /// above the cap forever, and must do so without a poll cadence --
    /// this is exactly the mechanism `run_attach_reader`'s own episode
    /// teardown depends on, exercised here with NOTHING ever draining
    /// the queue (no `FeAttachClient`, no `pump()` call at all): `stop`
    /// is the ONLY way out.
    #[test]
    fn queued_bytes_wait_below_cap_wakes_a_bounded_number_of_times_when_released_by_stop_with_no_drain_ever_happening()
    {
        let q = Arc::new(QueuedBytes::new());
        q.add(10); // stays at/above the cap for the whole test -- nothing ever calls sub()
        let stop = Arc::new(AtomicBool::new(false));
        let wakes = Arc::new(AtomicUsize::new(0));

        let waiter_q = Arc::clone(&q);
        let waiter_stop = Arc::clone(&stop);
        let waiter_wakes = Arc::clone(&wakes);
        let waiter = thread::spawn(move || {
            waiter_q.wait_below_cap_traced(10, &waiter_stop, || {
                waiter_wakes.fetch_add(1, Ordering::SeqCst);
            });
        });

        thread::sleep(Duration::from_millis(200));
        stop.store(true, Ordering::Release);
        q.notify_stop();
        waiter.join().expect("waiter thread must not panic");

        let wake_count = wakes.load(Ordering::SeqCst);
        assert!(
            wake_count <= 6,
            "waiter woke {wake_count} times across a 200ms hold -- expected entry plus one \
             notified wake (2, with slack for a loaded CI runner's own spurious wakeups), not \
             a 20ms poll cadence (which would be ~10)"
        );
    }

    /// Codex review round finding 2: a fresh checkpoint (a new attach
    /// episode, possibly against a DIFFERENT leg after a reconnect) must
    /// retract whatever notice was showing for the PREVIOUS leg, rather
    /// than leaving it standing until a new `Notice` event replaces it.
    /// The attach notice is emitted only after the checkpoint (see
    /// `run_worker`'s own reorder comment), so without this clear, a
    /// caller reading `notice()` right after this `pump()` call -- before
    /// the worker's own mgmt-lane lookup for the NEW leg has produced its
    /// own `Notice` -- would see leg A's stale text rendered over leg B's
    /// freshly restored screen.
    #[test]
    fn checkpoint_event_clears_a_notice_left_over_from_the_previous_leg() {
        let (events_tx, events_rx) = mpsc::channel();
        let (msg_tx, _msg_rx) = mpsc::channel();
        let mut client = FeAttachClient::<PlatformEndpoint> {
            _endpoint: PhantomData,
            parser: vt100_ctt::Parser::new(24, 80, 100),
            pane_size: (24, 80),
            msg_tx,
            events_rx,
            status: "connecting\u{2026}".to_string(),
            notice: None,
            quit_message: None,
            should_exit: false,
            dead: false,
            checkpointed: false,
            restore_ok: false,
            queued_bytes: Arc::new(QueuedBytes::new()),
            pending_fe_down_markers: VecDeque::new(),
            headless: false,
            recorded_bytes: Arc::new(AtomicU64::new(0)),
            last_input_outcome: Arc::new(Mutex::new(None)),
            worker_handle: None,
        };

        events_tx
            .send(ClientEvent::Notice("leg A started at ...".to_string()))
            .expect("send a synthetic notice for leg A");
        client.pump();
        assert_eq!(client.notice(), Some("leg A started at ..."));

        let bytes = vt100_ctt::Parser::new(24, 80, 100)
            .screen()
            .checkpoint()
            .expect("encode a checkpoint of a fresh, in-range screen");
        events_tx
            .send(ClientEvent::Checkpoint(bytes))
            .expect("send leg B's own checkpoint -- no Notice for leg B has arrived yet");
        client.pump();
        assert_eq!(
            client.notice(),
            None,
            "leg A's notice must be retracted the moment leg B's checkpoint lands, not left \
             standing until leg B's own Notice (if any) arrives"
        );
    }

    /// Successive checkpoints (each attach episode gets its own) must
    /// each report `restore_ok` for THEIR OWN restore, not a value stuck
    /// from an earlier one -- proven here across three in a row:
    /// success, failure, success again.
    #[test]
    fn restore_ok_reflects_only_the_most_recent_checkpoint_across_several_in_a_row() {
        let (events_tx, events_rx) = mpsc::channel();
        let (msg_tx, _msg_rx) = mpsc::channel();
        let mut client = FeAttachClient::<PlatformEndpoint> {
            _endpoint: PhantomData,
            parser: vt100_ctt::Parser::new(24, 80, 100),
            pane_size: (24, 80),
            msg_tx,
            events_rx,
            status: "connecting\u{2026}".to_string(),
            notice: None,
            quit_message: None,
            should_exit: false,
            dead: false,
            checkpointed: false,
            restore_ok: false,
            queued_bytes: Arc::new(QueuedBytes::new()),
            pending_fe_down_markers: VecDeque::new(),
            headless: false,
            recorded_bytes: Arc::new(AtomicU64::new(0)),
            last_input_outcome: Arc::new(Mutex::new(None)),
            worker_handle: None,
        };
        let good_checkpoint = || {
            vt100_ctt::Parser::new(24, 80, 100)
                .screen()
                .checkpoint()
                .expect("encode a checkpoint of a fresh, in-range screen")
        };

        events_tx.send(ClientEvent::Checkpoint(good_checkpoint())).expect("send checkpoint 1 (good)");
        client.pump();
        assert!(client.restore_ok(), "checkpoint 1 (good) must report restore_ok");

        events_tx.send(ClientEvent::Checkpoint(vec![0xff; 4])).expect("send checkpoint 2 (bad)");
        client.pump();
        assert!(!client.restore_ok(), "checkpoint 2 (bad) must clear restore_ok, not inherit checkpoint 1's");

        events_tx.send(ClientEvent::Checkpoint(good_checkpoint())).expect("send checkpoint 3 (good)");
        client.pump();
        assert!(client.restore_ok(), "checkpoint 3 (good) must report restore_ok again, not inherit checkpoint 2's");
    }
}
