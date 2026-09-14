//! ADR 0046 decision 3 (lane B3a): the attach lane's transport half,
//! extracted out of `fe_client_io.rs` into a reusable worker,
//! [`AttachWorker`], with a caller-supplied event sink. This lane is a
//! PURE, behavior-preserving extraction — connect, hello, attach,
//! checkpoint reassembly (still whole, as before — chunked delivery is
//! B3b2's own change), the episode reader, the 2s liveness poll,
//! take/input transactions, [`OutstandingSlot`], [`ReconnectState`], and
//! quit (`request_quit`, the ONE dispatcher, unchanged) move here out of
//! `fe_client_io.rs` verbatim in spirit; [`WorkerEvent`] is exactly the
//! pre-extraction module's own `ClientEvent`, renamed, plus nothing else
//! — every other candidate addition (per-request input outcomes,
//! chunked checkpoint delivery, `Reattach`, `Take`, pen/geometry events,
//! `SupervisorStatus` observation) belongs to the lane that first
//! consumes it (B1, B3b1, B3b2, B3b3) and is deliberately NOT here.
//!
//! The one genuine addition is bounded ingress:
//! [`AttachWorker::send_input`] reserves `bytes.len()` against a fixed
//! byte budget before the command channel is ever touched, refusing
//! with [`IngressRefused`] rather than queuing unbounded. The
//! reservation is OWNERSHIP-based ([`IngressReservation`]): it rides
//! inside the queued command itself and releases in `Drop`, so it is
//! freed correctly however that command is disposed of — consumed
//! normally, discarded mid-drain, or still sitting in the channel when
//! the whole worker (and its `Receiver`) exits — without any call site
//! needing to remember to release it by hand.
//!
//! `fe_client_io::FeAttachClient` is the thin wrapper: it owns the
//! `vt100_ctt::Parser` and `pump()`'s UI bookkeeping, subscribing an
//! `AttachWorker` via a sink closure built from its own mpsc channel +
//! wake, exactly as the pre-extraction module's `run_worker` was spawned
//! from its `attach_inner`. Its public surface (`attach`/
//! `attach_headless`/`pump`/`screen`/`send_input`/`resize`/
//! `request_quit`/`quit_message`/`should_exit`/`shutdown`/...) is
//! unchanged for its existing callers: the frontend's Terminal drawer
//! and session pane (`gpu.rs`) and the daemon's headless callers
//! (`capsule_workspace.rs`).

use crate::challenge::{ChallengeOutcome, PeerAuthOutcome};
use crate::client::{transport_error_to_io, Client, Endpoint};
use crate::exchange::{SupervisorLaneExchange, SUPERVISOR_LANE_BUILD_ID};
use crate::fe_client::{
    self, FeDownBaseline, InputWireOutcome, OutstandingSlot, QuitDispatcher, QuitState,
    ReconnectDecision, ReconnectState, Role, TakeAction, TakeTransaction,
};
use crate::transport::TransportError;
use crate::wire::{
    self, AttachClient, AttachServer, DecodedFrame, ResizeRefusedReason, SupervisorOp,
    SupervisorPhase, SupervisorReply, SupervisorRequest, TakeRefusedReason,
};
use std::collections::VecDeque;
use std::io::ErrorKind;
use std::marker::PhantomData;
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
/// Ruling (d): "The reader's unbounded channel becomes BYTE-ACCOUNTED
/// and bounded at 4 MiB — bytes, not items... When it is full the FE
/// STOPS READING THE PIPE." (Codex review round, finding 7: the first
/// landing's counter was local to the reader thread and released
/// immediately, never actually shared with the consumer — see
/// [`FeAttachClient::pump`]'s own doc for the real, shared half.)
const READER_QUEUE_CAP_BYTES: usize = 4 * 1024 * 1024;

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
    /// ADR 0045 decision 4: the daemon behind a `DaemonLaneEndpoint`
    /// dial refused `lane.connect` — `code`/`detail` kept SEPARATE
    /// (not joined into one string) so every caller can preserve the
    /// daemon's own diagnostic through to whatever terminal shape it
    /// produces, rather than a caller needing to re-parse a formatted
    /// string to recover the code.
    Refused { code: String, detail: String },
    /// ADR 0045 decision 4: the dial or handshake to the daemon's own
    /// lane bridge failed — retried after backoff, never charged to the
    /// absence window ([`ReconnectState::clear_unresponsive`]).
    Unreachable(String),
    /// ADR 0045 decision 4: the daemon answered `undetermined` — its own
    /// identity check on the lane it dialed could not complete. Retried
    /// exactly like `Unreachable`.
    Undetermined(String),
}

impl std::fmt::Display for LaneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LaneError::Io(e) => write!(f, "io: {e}"),
            LaneError::Timeout => write!(f, "timed out"),
            LaneError::Eof => write!(f, "connection closed"),
            LaneError::Wire(e) => write!(f, "wire: {e}"),
            LaneError::Protocol(s) => write!(f, "protocol: {s}"),
            LaneError::Refused { code, detail } => write!(f, "refused ({code}): {detail}"),
            LaneError::Unreachable(s) => write!(f, "unreachable: {s}"),
            LaneError::Undetermined(s) => write!(f, "undetermined: {s}"),
        }
    }
}

/// The three bridge-only `TransportError` arms (ADR 0045 decision 4),
/// classified BEFORE any conversion through [`transport_error_to_io`] —
/// unwrapping `Unreachable`/a bridge-sourced `Undetermined` that way
/// would make transport uncertainty look exactly like the absence
/// `TransportError::is_endpoint_absent()` reports, folding a hiccup and
/// a genuinely dead row into one signal (the invariant this whole
/// decision exists to keep). Every other `TransportError` — the
/// platform endpoints' own `Io`/`RuntimeDir`/etc., including a
/// direct-sourced `Undetermined`/the unit `Foreign` a CHALLENGED
/// connect can still produce elsewhere in this crate, never from an
/// `Endpoint::connect_*_unchallenged` call — keeps going through
/// [`transport_error_to_io`] exactly as before this decision landed.
/// `Refused{code: "unauthenticated", ..}` reaching here is ALWAYS a
/// bridge-speaking daemon's own bad-token refusal — `sot_protocol::
/// lane_client::classify_reply` already renames an old daemon's
/// coincidentally-`unauthenticated`-coded control-loop gate to
/// `no_bridge` at the source, so this function never has to re-guess it
/// from message text.
fn classify_transport(e: TransportError) -> LaneError {
    match e {
        TransportError::Refused { code, detail } => LaneError::Refused { code, detail },
        TransportError::Unreachable(io) => LaneError::Unreachable(io.to_string()),
        TransportError::Undetermined { detail, .. } => LaneError::Undetermined(detail),
        other => LaneError::Io(transport_error_to_io(other)),
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
/// only), reusing the SAME primitives (`Endpoint::connect_supervisor_
/// unchallenged`, `Endpoint::challenge`, `SupervisorLaneExchange`) rather
/// than depending on that test-gated helper.
fn connect_supervisor_lane<E: Endpoint>(endpoint: &E, h: &str) -> Result<(E::Client, E::Process), LaneError> {
    let conn = endpoint.connect_supervisor_unchallenged(h).map_err(classify_transport)?;
    let mut exchange = SupervisorLaneExchange::new(SUPERVISOR_LANE_BUILD_ID);
    let deadline = Instant::now() + HELLO_BUDGET;
    match endpoint.challenge(&conn, &mut exchange, deadline) {
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
/// ADR 0043 decision 28, ADR 0045 decision 6: `voyage` is now OPTIONAL —
/// the attach client converges on the supervisor's own word only, never a
/// pointer file, so this probes the voyage id the supervisor LAST
/// REPORTED to this client (`run_worker`'s own `voyage_uuid`, carried
/// across episodes) rather than reading anything off disk; an episode
/// reaching this path may not have one yet. A missing id starts the
/// health accounting exactly as an absent supervisor does (there is
/// nothing to probe with, so this cannot distinguish "the capsule
/// survives headless" from "nothing exists yet" — both retry under the
/// same clock); an answered `Status` on a later round still clears the
/// unresponsive count via `ReconnectState::attached` or
/// `clear_unresponsive`, whichever path reaches it.
fn on_supervisor_absent_or_unresponsive<E: Endpoint>(
    endpoint: &E,
    reconnect: &mut ReconnectState,
    lane: &str,
    voyage: Option<&str>,
    now: Instant,
) -> ReconnectDecision {
    let Some(voyage) = voyage else {
        return reconnect.classify_unresponsive(now);
    };
    // `lane` is the row's own name in the endpoint's namespace (the
    // worker's own `lane: String` -- ADR 0045 decision 5's row-identity
    // argument, the daemon-lane endpoint's own `target`), NEVER `voyage`
    // itself: a lane-bridge dial resolves the voyage id THROUGH the row
    // that owns it, so passing `voyage` for both used to answer
    // `unknown_workspace` for a row that is very much still there
    // (ADR 0045 lane B4a Codex review, blocker: "health probing sends
    // (voyage, voyage); the daemon expects (row, voyage)"). The platform
    // endpoints still ignore this argument regardless.
    match endpoint.connect_voyage_unchallenged(lane, voyage) {
        Ok(_probe) => {
            reconnect.clear_unresponsive();
            ReconnectDecision::Retry
        }
        // ADR 0045 decision 4: the two uncertain arms are transport
        // hiccups, never charged to the health window — clear the clock
        // and retry exactly like a live voyage pipe would. A refusal
        // whose code is `unauthenticated` keeps this probe's own prior
        // access-denied classification; every OTHER refusal preserves
        // its own `{code, detail}` via `classify_lane_refused` rather
        // than collapsing into a generic `ForeignPipe`/`AccessDenied`
        // that would discard the daemon's own diagnostic.
        Err(e) => match classify_transport(e) {
            LaneError::Refused { code, .. } if code == "unauthenticated" => reconnect.classify_access_denied(),
            LaneError::Refused { code, detail } => reconnect.classify_lane_refused(code, detail),
            LaneError::Unreachable(_) | LaneError::Undetermined(_) => {
                reconnect.clear_unresponsive();
                ReconnectDecision::Retry
            }
            LaneError::Io(io) => {
                if is_access_denied(&io) {
                    reconnect.classify_access_denied()
                } else {
                    reconnect.classify_unresponsive(now)
                }
            }
            _ => unreachable!("classify_transport only ever produces Io/Refused/Unreachable/Undetermined"),
        },
    }
}

/// What [`converge_on_ready`] concluded. `Ready` carries the SAME
/// supervisor-lane connection it was given back (the caller keeps using
/// it, first for the "voyage changed" reconciliation, then for a latched
/// `Quit` in the steady-state loop — no second connect+hello) plus the
/// voyage id the supervisor's own `Ready` report named AND the
/// already-connected voyage lane itself — Codex review
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

/// ADR 0043 decision 28, ADR 0045 decision 6: the attach client converges
/// on the supervisor's OWN word ONLY, never on a pointer file. Given an
/// already-connected, already-`hello`'d supervisor lane, polls `Status`
/// on that SAME connection every [`fe_client::RECONNECT_BACKOFF_INITIAL`]
/// (a FIXED interval — Codex review round finding 6: this loop is
/// steady-state polling of a lane that is actively ANSWERING, never a
/// reconnect attempt, so [`ReconnectState::retry_with_backoff`]'s
/// doubling — which stays reserved for genuine reconnect waits in
/// [`run_worker`]'s own outer episode loop — never applies here) until
/// the report says `Ready` with a voyage id AND the voyage lane itself
/// accepts a connection. Every answered `Status`, whatever its phase,
/// clears [`ReconnectState::clear_unresponsive`] (finding 2) — an outage
/// that already resolved must not keep aging through however many "still
/// starting" rounds follow. Two things happen INSIDE this loop, both
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
/// - Once `Ready` names a voyage id, ONE voyage-lane connect attempt
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
    endpoint: &E,
    mut conn: E::Client,
    mut sup_reader: FrameReader,
    h: &str,
    cmd_rx: &Receiver<WorkerMsg>,
    reconnect: &mut ReconnectState,
    latched_quit_reason: &mut Option<String>,
    quit: &mut QuitDispatcher,
    outstanding: &mut OutstandingSlot,
    emit: &dyn Fn(WorkerEvent),
) -> ReadyOutcome<E> {
    // Emitted at most once per "still starting" spell — re-armed every
    // time a latched Quit or a fresh Ready round makes the NEXT status
    // worth announcing again as a fresh wait.
    let mut emitted_starting = false;

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
                run_quit::<E>(endpoint, &mut conn, &mut sup_reader, h, &id, reason, quit, outstanding, emit);
                if quit.should_exit() {
                    return ReadyOutcome::ShouldExit;
                }
                emitted_starting = false;
                continue;
            }
        }

        let Some(id) = (phase == SupervisorPhase::Ready).then_some(sv).flatten() else {
            // Not yet Ready, or Ready with no voyage id yet (should not
            // normally happen -- `discover_or_mint_voyage` publishes
            // before `Ready` -- handled the same as "starting" rather
            // than assumed).
            if !emitted_starting {
                emit(WorkerEvent::Status("supervisor starting \u{2014} waiting\u{2026}".to_string()));
                emitted_starting = true;
            }
            match wait_for_retry_or_shutdown(cmd_rx, fe_client::RECONNECT_BACKOFF_INITIAL, latched_quit_reason) {
                WaitOutcome::Shutdown => return ReadyOutcome::Shutdown,
                WaitOutcome::Continue => continue,
            }
        };

        // Decision 6: the supervisor's own `Ready{voyage}` is authoritative
        // by itself now -- no pointer-file read gates it. Finding 7: drain
        // and latch any control command already queued before committing
        // to the attach transition below — a Quit that arrived while this
        // round's Status check ran must never be allowed to sail through
        // unread.
        if let Some(WaitOutcome::Shutdown) = drain_pending_control(cmd_rx, latched_quit_reason) {
            return ReadyOutcome::Shutdown;
        }
        if let Some(reason) = latched_quit_reason.take() {
            run_quit::<E>(endpoint, &mut conn, &mut sup_reader, h, &id, reason, quit, outstanding, emit);
            if quit.should_exit() {
                return ReadyOutcome::ShouldExit;
            }
            emitted_starting = false;
            continue;
        }
        // Finding 1: the voyage-lane connect is ONE attempt per round,
        // folded into this SAME loop -- "not yet" returns to Status
        // polling above rather than an independent, unbounded retry loop
        // that never sees a supervisor Terminal phase or health
        // accounting again.
        match endpoint.connect_voyage_unchallenged(h, &id) {
            Ok(voyage_conn) => {
                return ReadyOutcome::Ready { conn, sup_reader, voyage_id: id, voyage_conn };
            }
            // ADR 0045 decision 4: classified the SAME way as the
            // supervisor-lane connect above it in `run_worker` — a
            // refusal is terminal, its code named; the two uncertain
            // arms clear the health window's clock and retry at this
            // loop's own fixed per-round interval (unchanged from the
            // plain "not yet available" wait below, since this is a
            // bounded Status-polling round, not the episode-level
            // backoff `ReconnectState::retry_with_backoff` governs).
            Err(e) => match classify_transport(e) {
                LaneError::Refused { code, detail } => {
                    let msg = if code == "no_bridge" {
                        "this daemon has no bridge — it predates the lane bridge (ADR 0045)".to_string()
                    } else {
                        format!("voyage pipe: daemon refused ({code}): {detail}")
                    };
                    return ReadyOutcome::Terminal(msg);
                }
                e @ (LaneError::Unreachable(_) | LaneError::Undetermined(_)) => {
                    reconnect.clear_unresponsive();
                    let msg = match &e {
                        LaneError::Unreachable(d) => format!("daemon unreachable — retrying ({d})"),
                        LaneError::Undetermined(_) => "daemon could not identify the lane — retrying".to_string(),
                        _ => unreachable!("matched above"),
                    };
                    emit(WorkerEvent::Status(msg));
                    match wait_for_retry_or_shutdown(cmd_rx, fe_client::RECONNECT_BACKOFF_INITIAL, latched_quit_reason) {
                        WaitOutcome::Shutdown => return ReadyOutcome::Shutdown,
                        WaitOutcome::Continue => continue,
                    }
                }
                LaneError::Io(io) => {
                    if is_access_denied(&io) {
                        return ReadyOutcome::Terminal("voyage pipe: access denied".to_string());
                    }
                    emit(WorkerEvent::Status(format!("voyage pipe not yet available: {io}")));
                    match wait_for_retry_or_shutdown(cmd_rx, fe_client::RECONNECT_BACKOFF_INITIAL, latched_quit_reason) {
                        WaitOutcome::Shutdown => return ReadyOutcome::Shutdown,
                        WaitOutcome::Continue => continue,
                    }
                }
                _ => unreachable!("classify_transport only ever produces Io/Refused/Unreachable/Undetermined"),
            },
        }
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


/// An ingress reservation, released automatically wherever it is
/// dropped — consumed normally by the worker's own loop, discarded
/// mid-drain (the several places a queued `WorkerMsg` is read and
/// thrown away without further action), or still sitting unconsumed in
/// the channel when the whole worker thread exits and its `Receiver`
/// drops (which drops every value still queued behind it). This is what
/// makes the ingress bound honest under every one of [`AttachWorker::
/// send_input`]'s own exit paths — including a send that fails because
/// the worker has already shut down, where the returned, undelivered
/// message (reservation included) drops right there — without any call
/// site needing to remember to release it by hand.
struct IngressReservation {
    ingress_bytes: Arc<AtomicUsize>,
    n: usize,
}

impl Drop for IngressReservation {
    fn drop(&mut self) {
        self.ingress_bytes.fetch_sub(self.n, Ordering::AcqRel);
    }
}

enum WorkerMsg {
    Input(Vec<u8>, IngressReservation),
    Resize(u16, u16),
    Quit(String),
    Shutdown,
    Frame(DecodedFrame),
    ReaderDone,
}

/// What the worker reports to the foreground — unchanged from the
/// pre-extraction module's own `ClientEvent`, renamed only. `pump()`
/// applies each of these to the parser/UI state.
pub enum WorkerEvent {
    Checkpoint(Vec<u8>),
    Output(Vec<u8>),
    Notice(String),
    Status(String),
    Terminal(String),
    QuitMessage(Option<String>),
    ShouldExit,
    FeDownMarker(serde_json::Value),
}

/// The wire's terminal answer to ONE `input` frame this client sent (ADR
/// 0041), exposed as an observable a caller (the headless daemon client)
/// can poll — unchanged from the pre-extraction module's own type,
/// moved here since it names a worker-level (not rendering-level)
/// observable; `fe_client_io` re-exports it at its own path so existing
/// callers (`capsule_workspace.rs`, `tests/fe_client.rs`) are unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputOutcome {
    /// `input_recorded`: the record has it.
    Recorded,
    /// `input_refused_stale`: the epoch changed; this client's own worker
    /// re-takes automatically, but a headless caller treats the ORIGINAL
    /// send as failed and does not wait for that retry.
    RefusedStale,
    /// `input_delivery_unknown`: the record's own verdict is unknowable
    /// from here.
    DeliveryUnknown,
}

/// [`AttachWorker::send_input`]'s own refusal — the ingress reservation
/// would have exceeded the worker's own bound. Carries nothing: the
/// caller already has `bytes` (it just tried to send them) and the
/// bound itself is a construction-time constant the caller chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngressRefused;

impl std::fmt::Display for IngressRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ingress bound exceeded; refused rather than queued unbounded")
    }
}

impl std::error::Error for IngressRefused {}

/// Default bound for [`AttachWorker::send_input`]'s own ingress
/// reservation — a memory bound on "commands enqueued, not yet popped
/// by the worker's own loop," generous relative to [`fe_client::
/// TAKE_QUEUE_CAP`] (8 KiB: several pastes' worth of headroom) without
/// being unbounded. `fe_client_io::FeAttachClient` uses this constant;
/// a caller free to choose a tighter or looser bound may construct an
/// `AttachWorker` with its own.
pub const DEFAULT_INGRESS_BOUND_BYTES: usize = 64 * 1024;

// -----------------------------------------------------------------------
// Public surface: the worker handle
// -----------------------------------------------------------------------

/// A reusable attach transport, generic over `E: Endpoint` exactly like
/// the pre-extraction `FeAttachClient` was (see that type's own doc,
/// `fe_client_io.rs`, for why). Owns the background worker thread.
pub struct AttachWorker<E: Endpoint> {
    msg_tx: Sender<WorkerMsg>,
    ingress_bytes: Arc<AtomicUsize>,
    ingress_bound: usize,
    /// The episode reader's own byte-accounted backpressure — see
    /// [`QueuedBytes`]'s own doc. Released by [`Self::ack_output_consumed`].
    queued_bytes: Arc<QueuedBytes>,
    worker_handle: Option<thread::JoinHandle<()>>,
    _endpoint: PhantomData<E>,
}

impl<E: Endpoint> AttachWorker<E> {
    /// Connects through `endpoint` and starts the background worker;
    /// this constructor never blocks on the network — matches the
    /// pre-extraction `FeAttachClient::attach`'s own "returns once the
    /// worker thread is running" contract. `sink` is called
    /// synchronously, ON the worker thread, for every [`WorkerEvent`]
    /// this worker ever produces; a caller wanting cross-thread delivery
    /// (the rendering client's own `pump()`/mpsc shape) builds that INTO
    /// its own `sink` closure, exactly as `fe_client_io::FeAttachClient`
    /// does. `recorded_bytes`/`last_input_outcome` are the SAME shared
    /// observables the pre-extraction worker wrote directly — this
    /// worker still writes them directly; they are not part of
    /// [`WorkerEvent`] and never were.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        endpoint: E,
        lane: String,
        cols: u16,
        rows: u16,
        controller_id: String,
        fe_down_to_handle: String,
        fe_down_last_evidence: Option<String>,
        headless: bool,
        ingress_bound: usize,
        recorded_bytes: Arc<AtomicU64>,
        last_input_outcome: Arc<Mutex<Option<InputOutcome>>>,
        sink: impl Fn(WorkerEvent) + Send + 'static,
    ) -> Result<Self, std::io::Error>
    where
        E: Send + 'static,
        E::Client: 'static,
    {
        let rows = rows.max(2);
        let cols = cols.max(2);
        let (msg_tx, msg_rx) = mpsc::channel::<WorkerMsg>();
        let worker_msg_tx = msg_tx.clone();
        let fe_down = FeDownBaseline::capture(fe_down_last_evidence);
        let queued_bytes = Arc::new(QueuedBytes::new());
        let worker_queued_bytes = Arc::clone(&queued_bytes);
        let ingress_bytes = Arc::new(AtomicUsize::new(0));

        let worker_handle = thread::Builder::new()
            .name("sot-fe-attach-worker".to_string())
            .spawn(move || {
                run_worker::<E>(
                    endpoint,
                    lane,
                    controller_id,
                    fe_down_to_handle,
                    fe_down,
                    cols,
                    rows,
                    msg_rx,
                    worker_msg_tx,
                    sink,
                    worker_queued_bytes,
                    recorded_bytes,
                    last_input_outcome,
                    headless,
                );
            })?;

        Ok(Self { msg_tx, ingress_bytes, ingress_bound, queued_bytes, worker_handle: Some(worker_handle), _endpoint: PhantomData })
    }

    /// A sink that consumes [`WorkerEvent::Output`] bytes calls this with
    /// however many it just consumed, releasing the episode reader's own
    /// backpressure by that much — the ONLY way [`QueuedBytes`] ever goes
    /// down (see that type's own doc). `fe_client_io::FeAttachClient::
    /// pump` calls this exactly where the pre-extraction module's own
    /// `queued_bytes.sub` call was.
    pub fn ack_output_consumed(&self, n: usize) {
        self.queued_bytes.sub(n);
    }

    /// Reserves `bytes.len()` against this worker's own ingress bound
    /// BEFORE the command channel is ever touched (never queued
    /// unbounded — see this module's own top doc). Empty input is a
    /// silent no-op: it types nothing and would otherwise let a caller
    /// enqueue an unlimited number of zero-charge commands despite the
    /// advertised memory bound. A send to an already-exited worker is
    /// not itself a refusal — the reservation is released the moment the
    /// undelivered message (returned by the channel) drops, same as any
    /// other disposal.
    pub fn send_input(&self, bytes: Vec<u8>) -> Result<(), IngressRefused> {
        if bytes.is_empty() {
            return Ok(());
        }
        let n = bytes.len();
        let mut cur = self.ingress_bytes.load(Ordering::Acquire);
        loop {
            if cur.saturating_add(n) > self.ingress_bound {
                return Err(IngressRefused);
            }
            match self.ingress_bytes.compare_exchange_weak(cur, cur + n, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => break,
                Err(actual) => cur = actual,
            }
        }
        let reservation = IngressReservation { ingress_bytes: Arc::clone(&self.ingress_bytes), n };
        // Whether this send succeeds or fails, the reservation is now
        // owned by the message: on success the worker's own loop drops
        // it once consumed (or discards it mid-drain, releasing it just
        // the same); on failure the returned `SendError` carries the
        // message right back here, and letting the `Result` drop
        // unused drops it immediately.
        let _ = self.msg_tx.send(WorkerMsg::Input(bytes, reservation));
        Ok(())
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.msg_tx.send(WorkerMsg::Resize(cols, rows));
    }

    /// Ruling (a): the ONE quit dispatcher — idempotent, latched across a
    /// reconnect in flight, unchanged from the pre-extraction module.
    pub fn request_quit(&self, reason: &str) {
        let _ = self.msg_tx.send(WorkerMsg::Quit(reason.to_string()));
    }

    /// Sends `Shutdown` (same as [`Drop`]) and waits up to `wait` for the
    /// worker thread to exit — unchanged bounded-join shape from the
    /// pre-extraction `FeAttachClient::shutdown`.
    pub fn shutdown(&mut self, wait: Duration) -> bool {
        let _ = self.msg_tx.send(WorkerMsg::Shutdown);
        let Some(handle) = self.worker_handle.take() else {
            return true;
        };
        let deadline = Instant::now() + wait;
        while !handle.is_finished() {
            if Instant::now() >= deadline {
                eprintln!("attach_worker: worker thread still alive {wait:?} after Shutdown was sent");
                return false;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = handle.join();
        true
    }

    /// A worker handle with no worker thread behind it — `fe_client_io`'s
    /// own unit tests build an [`fe_client_io::FeAttachClient`] by struct
    /// literal (this crate's child-module-privacy pattern) and need SOME
    /// value for this field; nothing here is ever sent to. `pub(crate)`:
    /// test-only, never a real caller's construction path.
    #[cfg(test)]
    pub(crate) fn stub_for_test() -> Self {
        let (msg_tx, _msg_rx) = mpsc::channel::<WorkerMsg>();
        Self {
            msg_tx,
            ingress_bytes: Arc::new(AtomicUsize::new(0)),
            ingress_bound: usize::MAX,
            queued_bytes: Arc::new(QueuedBytes::new()),
            worker_handle: None,
            _endpoint: PhantomData,
        }
    }
}

impl<E: Endpoint> Drop for AttachWorker<E> {
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
    endpoint: E,
    lane: String,
    controller_id: String,
    fe_down_to_handle: String,
    mut fe_down: FeDownBaseline,
    initial_cols: u16,
    initial_rows: u16,
    cmd_rx: Receiver<WorkerMsg>,
    msg_tx: Sender<WorkerMsg>,
    sink: impl Fn(WorkerEvent) + Send + 'static,
    queued_bytes: Arc<QueuedBytes>,
    recorded_bytes: Arc<AtomicU64>,
    last_input_outcome: Arc<Mutex<Option<InputOutcome>>>,
    headless: bool,
) where
    E: Send + 'static,
    E::Client: 'static,
{
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

    let emit = |e: WorkerEvent| sink(e);

    'episodes: while !shutdown {
        emit(WorkerEvent::Status("connecting\u{2026}".to_string()));

        // --- supervisor lane: hello (build identity) once, then converge
        // on Ready (ADR 0043 decision 28) -------------------------------
        let supervisor_ready = match connect_supervisor_lane::<E>(&endpoint, &lane) {
            Ok((conn, _proven)) => {
                match converge_on_ready::<E>(
                    &endpoint,
                    conn,
                    FrameReader::new(),
                    &lane,
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
                        emit(WorkerEvent::Terminal(msg));
                        return;
                    }
                    ReadyOutcome::ShouldExit => {
                        emit(WorkerEvent::ShouldExit);
                        return;
                    }
                    ReadyOutcome::Shutdown => break 'episodes,
                    ReadyOutcome::LaneDown => None,
                }
            }
            Err(LaneError::Protocol(p)) if p.contains("version_skew") => {
                // `classify_hello_refused_version_skew` is always `Terminal`
                // (no `Retry` arm exists to discard it into) -- emit and
                // return directly rather than matching a foregone answer.
                reconnect.classify_hello_refused_version_skew();
                emit(WorkerEvent::Terminal(
                    "the row's supervisor refused this client (another lane protocol, or a supervisor from before the protocol-only gate); end the row and recreate it".to_string(),
                ));
                return;
            }
            Err(LaneError::Protocol(p)) if p.contains("foreign") => {
                match reconnect.classify_foreign() {
                    ReconnectDecision::Terminal(reason) => {
                        emit(WorkerEvent::Terminal(format!("supervisor lane: {reason:?}")));
                        return;
                    }
                    ReconnectDecision::Retry => unreachable!("classify_foreign is always terminal"),
                }
            }
            Err(LaneError::Io(e)) if is_access_denied(&e) => {
                emit(WorkerEvent::Terminal("supervisor lane: access denied".to_string()));
                return;
            }
            // ADR 0045 decision 4: the two bridge-only outcomes beyond
            // an ordinary `Io` — a refusal is terminal, its code named
            // in the pane line; the two uncertain arms retry, clearing
            // the health window's clock first so transport uncertainty
            // is never charged to it.
            Err(LaneError::Refused { code, detail }) => {
                let msg = if code == "no_bridge" {
                    "this daemon has no bridge — it predates the lane bridge (ADR 0045)".to_string()
                } else {
                    format!("daemon refused the lane ({code}): {detail}")
                };
                emit(WorkerEvent::Terminal(msg));
                return;
            }
            Err(e @ (LaneError::Unreachable(_) | LaneError::Undetermined(_))) => {
                reconnect.clear_unresponsive();
                let msg = match &e {
                    LaneError::Unreachable(d) => format!("daemon unreachable — retrying ({d})"),
                    LaneError::Undetermined(_) => "daemon could not identify the lane — retrying".to_string(),
                    _ => unreachable!("matched above"),
                };
                emit(WorkerEvent::Status(msg));
                match wait_for_retry_or_shutdown(&cmd_rx, reconnect.retry_with_backoff(), &mut latched_quit_reason) {
                    WaitOutcome::Shutdown => break 'episodes,
                    WaitOutcome::Continue => continue 'episodes,
                }
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
                // connect at all. Decision 6: the probe uses `voyage_uuid`
                // -- the voyage id the supervisor last reported to THIS
                // client, carried across episodes -- never a pointer file.
                match on_supervisor_absent_or_unresponsive::<E>(&endpoint, &mut reconnect, &lane, voyage_uuid.as_deref(), Instant::now()) {
                    ReconnectDecision::Terminal(reason) => {
                        emit(WorkerEvent::Terminal(format!("supervisor lane unreachable: {reason:?}")));
                        return;
                    }
                    ReconnectDecision::Retry => {
                        emit(WorkerEvent::Status("supervisor lane not answering \u{2014} retrying\u{2026}".to_string()));
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
                emit(WorkerEvent::Status(format!(
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
        let attach_identity = match endpoint.authenticate_server(&voyage_conn) {
            PeerAuthOutcome::Authenticated(a) => (a.pid, a.created),
            PeerAuthOutcome::Foreign => {
                emit(WorkerEvent::Terminal("voyage pipe: foreign".to_string()));
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
                    emit(WorkerEvent::Terminal(
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
        emit(WorkerEvent::Checkpoint(checkpoint));
        emit(WorkerEvent::Status("attached".to_string()));
        reconnect.attached();

        // The attach notice names the leg from the attach connection's own
        // daemon-authenticated identity (ADR 0045: the daemon's OS-level
        // observation of the voyage process, bound to this connection).
        // A second, throwaway mgmt-lane dial used to re-prove the same
        // (pid, created) here and blocked input until it finished.
        emit(WorkerEvent::Notice(fe_client::attach_notice_text(&format!("{}", attach_identity.1))));

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
            emit(WorkerEvent::FeDownMarker(marker));
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
                emit(WorkerEvent::Status(format!(
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
                emit(WorkerEvent::Terminal(format!("failed to start the attach reader thread: {e}")));
                return;
            }
        };
        let mut supervisor_conn = supervisor_conn;
        let mut last_liveness_poll = Instant::now();

        // --- steady state ------------------------------------------
        let episode_result = run_steady_state::<E>(
            &endpoint,
            &cmd_rx,
            &emit,
            &lane,
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
                emit(WorkerEvent::ShouldExit);
                shutdown = true;
            }
            SteadyOutcome::Terminal(reason) => {
                emit(WorkerEvent::Terminal(reason));
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
    endpoint: &E,
    supervisor_conn: &mut E::Client,
    sup_reader: &mut FrameReader,
    h: &str,
    voyage: &str,
    reason: String,
    quit: &mut QuitDispatcher,
    outstanding: &mut OutstandingSlot,
    emit: &dyn Fn(WorkerEvent),
) {
    let operation_id = format!("fe-quit-{}", uuid::Uuid::now_v7());
    // Mirrors `run_end_run_and_wait`'s own `request_quit` gate: this is
    // the SAME "is state currently Idle" condition that function checks
    // moments later, read here (nothing else mutates `quit` in between)
    // so the outstanding-input cancel — a ONE-TIME side effect — fires
    // only on the call that actually starts the ending transaction.
    if matches!(quit.state(), QuitState::Idle) {
        if let Some(o) = outstanding.cancel_for_quit() {
            emit(WorkerEvent::Status(format!(
                "input canceled by quit \u{2014} {} byte(s) not confirmed",
                o.bytes.len()
            )));
        }
    }
    let refused = std::cell::Cell::new(false);
    run_end_run_and_wait::<E>(
        supervisor_conn,
        sup_reader,
        |conn, reader| reconnect_supervisor_lane_for_quit::<E>(endpoint, conn, reader, h, &refused),
        quit,
        operation_id,
        reason,
        voyage,
        |quit| emit(WorkerEvent::QuitMessage(quit.message())),
    );
}

/// Reconnects the supervisor lane in place, for [`run_quit`]'s own use.
/// Best-effort and silent-on-failure BY DESIGN (beyond the one stderr
/// line): the caller's own loop simply tries again next iteration,
/// bounded overall by `QuitDispatcher::tick`'s 90 s cutoff -- there is
/// no separate retry budget to manage here, unlike the reconnect EPISODE
/// loop the rest of this module drives for the attach lane.
///
/// `refused`: latched `true` the first time the daemon answers
/// `lane.connect` with a genuine `Refused` (ADR 0045 decision 4, lane
/// B4a Codex review SHOULD-FIX: "the quit reconnect must not retry
/// refusals") — once latched, every FURTHER tick skips the dial
/// entirely rather than hammering a daemon that has already said no.
/// Still returns `false` either way (this closure's `bool` contract has
/// no third "give up" state, and `run_end_run_and_wait`'s own 90 s
/// cutoff is the real bound either way), so the caller's pacing is
/// unchanged — this only stops the WASTED re-dial, not the transaction's
/// own timeout.
fn reconnect_supervisor_lane_for_quit<E: Endpoint>(
    endpoint: &E,
    supervisor_conn: &mut E::Client,
    sup_reader: &mut FrameReader,
    h: &str,
    refused: &std::cell::Cell<bool>,
) -> bool {
    if refused.get() {
        return false;
    }
    match connect_supervisor_lane::<E>(endpoint, h) {
        Ok((conn, _proven)) => {
            *supervisor_conn = conn;
            *sup_reader = FrameReader::new();
            eprintln!("fe-client quit: reconnected the supervisor lane");
            true
        }
        Err(e @ LaneError::Refused { .. }) => {
            refused.set(true);
            eprintln!("fe-client quit: reconnect refused ({e}); giving up on further dials for this quit");
            false
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
    endpoint: &E,
    cmd_rx: &Receiver<WorkerMsg>,
    emit: &dyn Fn(WorkerEvent),
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
    loop {
        match cmd_rx.recv_timeout(WORKER_TICK) {
            Ok(WorkerMsg::Shutdown) => return SteadyOutcome::Shutdown,
            Ok(WorkerMsg::Input(bytes, _reservation)) => match take.role() {
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
                run_quit::<E>(endpoint, supervisor_conn, sup_reader, h, voyage, reason, quit, outstanding, &emit);
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
                    emit(WorkerEvent::Status(
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
fn apply_single_take_action<E: Endpoint>(action: TakeAction, attach_conn: &E::Client, controller_id: &str, emit: &dyn Fn(WorkerEvent)) {
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
            emit(WorkerEvent::Status("input discarded \u{2014} the pen never arrived in time".to_string()));
        }
        TakeAction::GeometryUnrepresentable => {
            emit(WorkerEvent::Status("window size not representable by this session".to_string()));
        }
        TakeAction::PenLost => {
            emit(WorkerEvent::Status("lost the pen".to_string()));
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
    emit: &dyn Fn(WorkerEvent),
    recorded_bytes: &Arc<AtomicU64>,
    last_input_outcome: &Arc<Mutex<Option<InputOutcome>>>,
) -> FrameOutcome {
    match frame {
        DecodedFrame::AttachServer(AttachServer::Output { bytes }) => {
            emit(WorkerEvent::Output(bytes));
            FrameOutcome::Handled
        }
        DecodedFrame::AttachServer(AttachServer::TakeOk { take_epoch: epoch }) => {
            *take_epoch = epoch;
            if *take_intent == TakeIntent::ReconnectResend {
                if let fe_client::ReconnectResendDecision::Cancel { canceled } =
                    outstanding.resend_after_reconnect(voyage, epoch)
                {
                    emit(WorkerEvent::Status(format!(
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
                emit(WorkerEvent::Status("input delivery unknown".to_string()));
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
// synthetic `WorkerEvent::Checkpoint` and no process/network dependency.
// -----------------------------------------------------------------------


#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::PeerIdentity;

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
    // -----------------------------------------------------------------
    // ADR 0045 decision 6: the health probe uses the voyage id the
    // supervisor last reported to THIS client, never a pointer file.
    // -----------------------------------------------------------------

    /// A do-nothing [`Client`] — [`TestEndpoint::connect_voyage_unchallenged`]
    /// is the only method this test ever exercises, and it never touches
    /// the connection it returns.
    struct TestClient;
    impl Client for TestClient {
        fn write_all(&self, _bytes: &[u8]) -> Result<(), crate::transport::TransportError> {
            Ok(())
        }
        fn read(&self, _buf: &mut [u8]) -> Result<usize, crate::transport::TransportError> {
            Ok(0)
        }
        fn cancel(&self) {}
    }

    /// A do-nothing [`PeerIdentity`] — never actually produced by this
    /// test's [`TestEndpoint`] (its `challenge`/`authenticate_server` are
    /// unreachable stubs), but [`Endpoint::Process`] still needs a
    /// concrete type to name.
    struct TestProcess;
    impl PeerIdentity for TestProcess {
        fn pid(&self) -> u32 {
            0
        }
        fn created(&self) -> u64 {
            0
        }
    }

    /// Records the row/id [`Endpoint::connect_voyage_unchallenged`] was
    /// asked for and always answers as if the voyage pipe were reachable
    /// (`Ok`) — proving [`on_supervisor_absent_or_unresponsive`] probes
    /// the SAME row and id its caller passed in, never a pointer file it
    /// reads itself and never the voyage id in BOTH slots (ADR 0045 lane
    /// B4a Codex review blocker: the probe used to send `(voyage,
    /// voyage)`, which a real `DaemonLaneEndpoint` reads as "row =
    /// voyage id", never resolving to any real row). `connect_
    /// supervisor_unchallenged`/`challenge`/`authenticate_server` are
    /// unreachable: this test never drives the supervisor lane.
    struct TestEndpoint {
        last_lane_probed: Mutex<Option<String>>,
        last_voyage_probed: Mutex<Option<String>>,
    }
    impl Endpoint for TestEndpoint {
        type Client = TestClient;
        type Process = TestProcess;

        fn connect_voyage_unchallenged(
            &self,
            lane: &str,
            voyage_id: &str,
        ) -> Result<Self::Client, crate::transport::TransportError> {
            *self.last_lane_probed.lock().unwrap() = Some(lane.to_string());
            *self.last_voyage_probed.lock().unwrap() = Some(voyage_id.to_string());
            Ok(TestClient)
        }

        fn connect_supervisor_unchallenged(&self, _lane: &str) -> Result<Self::Client, crate::transport::TransportError> {
            unreachable!("health_probe_uses_the_last_reported_voyage_id never drives the supervisor lane")
        }

        fn challenge(
            &self,
            _conn: &Self::Client,
            _exchange: &mut dyn crate::exchange::IdentityExchange,
            _deadline: Instant,
        ) -> ChallengeOutcome<Self::Process> {
            unreachable!("health_probe_uses_the_last_reported_voyage_id never challenges")
        }

        fn authenticate_server(&self, _conn: &Self::Client) -> PeerAuthOutcome {
            unreachable!("health_probe_uses_the_last_reported_voyage_id never authenticates")
        }
    }

    #[test]
    fn health_probe_uses_the_last_reported_voyage_id() {
        let ep = TestEndpoint { last_lane_probed: Mutex::new(None), last_voyage_probed: Mutex::new(None) };
        let mut reconnect = ReconnectState::new();
        let now = Instant::now();
        let lane = "sot-capsule-row-1";
        let voyage_id = "11111111-1111-1111-1111-111111111111";

        let decision = on_supervisor_absent_or_unresponsive::<TestEndpoint>(&ep, &mut reconnect, lane, Some(voyage_id), now);
        assert_eq!(decision, ReconnectDecision::Retry, "a reachable voyage pipe must retry, never go terminal");
        assert_eq!(
            ep.last_lane_probed.lock().unwrap().as_deref(),
            Some(lane),
            "the probe must connect through the WORKER'S OWN row, never the voyage id in that slot"
        );
        assert_eq!(
            ep.last_voyage_probed.lock().unwrap().as_deref(),
            Some(voyage_id),
            "the probe must connect to the id its caller passed in, not one it reads itself"
        );

        // `None`: no id to probe with — the health clock starts exactly
        // as it would for an absent supervisor, and a second call
        // `HEALTH_WINDOW` later is `Terminal`.
        let decision = on_supervisor_absent_or_unresponsive::<TestEndpoint>(&ep, &mut reconnect, lane, None, now);
        assert_eq!(decision, ReconnectDecision::Retry, "the clock merely starting is never itself terminal");
        let later = now + fe_client::HEALTH_WINDOW + Duration::from_secs(1);
        let decision = on_supervisor_absent_or_unresponsive::<TestEndpoint>(&ep, &mut reconnect, lane, None, later);
        assert_eq!(
            decision,
            ReconnectDecision::Terminal(fe_client::TerminalReason::HealthWindowExpired),
            "the clock started by the first None call must expire after HEALTH_WINDOW"
        );
    }

    /// A scripted [`Endpoint::connect_voyage_unchallenged`] — one queued
    /// outcome per call, so a test can drive the probe through an exact
    /// absence/uncertainty/absence sequence.
    struct ScriptedEndpoint {
        script: Mutex<std::collections::VecDeque<Result<(), crate::transport::TransportError>>>,
    }
    impl Endpoint for ScriptedEndpoint {
        type Client = TestClient;
        type Process = TestProcess;

        fn connect_voyage_unchallenged(&self, _lane: &str, _voyage_id: &str) -> Result<Self::Client, crate::transport::TransportError> {
            match self.script.lock().unwrap().pop_front().expect("script exhausted before the test finished driving it") {
                Ok(()) => Ok(TestClient),
                Err(e) => Err(e),
            }
        }
        fn connect_supervisor_unchallenged(&self, _lane: &str) -> Result<Self::Client, crate::transport::TransportError> {
            unreachable!("uncertainty_clears_the_absence_clock never drives the supervisor lane")
        }
        fn challenge(&self, _conn: &Self::Client, _exchange: &mut dyn crate::exchange::IdentityExchange, _deadline: Instant) -> ChallengeOutcome<Self::Process> {
            unreachable!("uncertainty_clears_the_absence_clock never challenges")
        }
        fn authenticate_server(&self, _conn: &Self::Client) -> PeerAuthOutcome {
            unreachable!("uncertainty_clears_the_absence_clock never authenticates")
        }
    }

    /// ADR 0045 decision 4: `Unreachable`/`Undetermined` must never be
    /// charged to the health window — they clear its clock exactly like
    /// a reachable voyage pipe would, so a genuine absence that follows
    /// gets a FRESH window rather than inheriting time an outage already
    /// spent. Sequence: absence starts the clock (Retry, clock running);
    /// an `Unreachable` probe clears it (Retry); a later absence starts
    /// its OWN clock (Retry, not yet Terminal even though the ORIGINAL
    /// clock would have expired by now); that fresh clock still expires
    /// on its own after a full `HEALTH_WINDOW` (Terminal) — proving the
    /// clear is real, not a permanent bypass.
    #[test]
    fn uncertainty_clears_the_absence_clock() {
        fn absent() -> crate::transport::TransportError {
            crate::transport::TransportError::Io {
                op: "test",
                source: std::io::Error::new(ErrorKind::NotFound, "absent"),
            }
        }
        fn unreachable_err() -> crate::transport::TransportError {
            crate::transport::TransportError::Unreachable(std::io::Error::new(ErrorKind::TimedOut, "unreachable"))
        }

        let ep = ScriptedEndpoint {
            script: Mutex::new(std::collections::VecDeque::from([Err(absent()), Err(unreachable_err()), Err(absent()), Err(absent())])),
        };
        let mut reconnect = ReconnectState::new();
        let lane = "sot-capsule-row-1";
        let voyage_id = "11111111-1111-1111-1111-111111111111";
        let t0 = Instant::now();

        // 1) A genuine absence starts the clock; not yet terminal.
        let d = on_supervisor_absent_or_unresponsive::<ScriptedEndpoint>(&ep, &mut reconnect, lane, Some(voyage_id), t0);
        assert_eq!(d, ReconnectDecision::Retry, "a clock that just started is never itself terminal");

        // 2) An `Unreachable` probe, well within what would have been
        // the original window, clears the clock instead of merely
        // retrying on top of it.
        let t1 = t0 + fe_client::HEALTH_WINDOW - Duration::from_secs(10);
        let d = on_supervisor_absent_or_unresponsive::<ScriptedEndpoint>(&ep, &mut reconnect, lane, Some(voyage_id), t1);
        assert_eq!(d, ReconnectDecision::Retry, "Unreachable must retry, never go terminal on its own");

        // 3) Past where the ORIGINAL (t0) clock would have expired --
        // still Retry, because step 2 cleared it: this absence starts
        // its OWN fresh window at t2, not inheriting t0's age.
        let t2 = t0 + fe_client::HEALTH_WINDOW + Duration::from_secs(1);
        let d = on_supervisor_absent_or_unresponsive::<ScriptedEndpoint>(&ep, &mut reconnect, lane, Some(voyage_id), t2);
        assert_eq!(
            d,
            ReconnectDecision::Retry,
            "the clock step 2 cleared must not let this absence appear to have been running since t0"
        );

        // 4) The FRESH window from step 3 (t2) does eventually expire on
        // its own -- proving step 2/3 cleared and restarted the clock
        // rather than disabling it.
        let t3 = t2 + fe_client::HEALTH_WINDOW + Duration::from_secs(1);
        let d = on_supervisor_absent_or_unresponsive::<ScriptedEndpoint>(&ep, &mut reconnect, lane, Some(voyage_id), t3);
        assert_eq!(
            d,
            ReconnectDecision::Terminal(fe_client::TerminalReason::HealthWindowExpired),
            "the fresh window started at t2 must still expire after its own full HEALTH_WINDOW"
        );
    }
}
