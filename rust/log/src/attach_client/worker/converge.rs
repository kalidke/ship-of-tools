//! Supervisor-lane connect, probe and converge-on-Ready, then the attach lane hello and checkpoint collection.

use crate::identity::challenge::ChallengeOutcome;
use crate::lane::client::Endpoint;
use crate::identity::exchange::{SupervisorLaneExchange, SUPERVISOR_LANE_BUILD_ID};
use crate::attach_client::rules::{self, OutstandingSlot, QuitDispatcher, ReconnectDecision, ReconnectState};
use crate::lane::transport::TEARDOWN_AGGREGATE_DEADLINE;
use crate::lane::wire::{
    self, AttachClient, AttachServer, DecodedFrame,
    SupervisorPhase, SupervisorReply, SupervisorRequest,
};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use super::*;


// -----------------------------------------------------------------------
// The supervisor lane: connect + hello (build identity) + status.
// -----------------------------------------------------------------------

/// Connect the supervisor lane and run the full same-connection
/// challenge with this crate's own build identity — the production
/// analog of `supervisor::connect_and_challenge_for_test` (test-support
/// only), reusing the SAME primitives (`Endpoint::connect_supervisor_
/// unchallenged`, `Endpoint::challenge`, `SupervisorLaneExchange`) rather
/// than depending on that test-gated helper.
pub(super) fn connect_supervisor_lane<E: Endpoint>(endpoint: &E, h: &str) -> Result<(E::Client, E::Process), LaneError> {
    let conn = endpoint.connect_supervisor_unchallenged(h).map_err(classify_transport)?;
    let mut exchange = SupervisorLaneExchange::new(SUPERVISOR_LANE_BUILD_ID);
    let deadline = Instant::now() + HELLO_BUDGET;
    let challenged = endpoint.challenge(&conn, &mut exchange, deadline);
    if !matches!(&challenged, ChallengeOutcome::Proven(_)) {
        endpoint.drop_spare();
    }
    match challenged {
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

pub(super) fn supervisor_status<E: Endpoint>(
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



/// One liveness probe, run OFF the worker's input path -- a keystroke must
/// never wait a `Status` round trip. The inline probe it replaces, minus
/// the status text and health accounting, which stay with the worker:
/// `Status` under its own budgets and, on a miss, the paced re-dial.
pub(super) fn probe_supervisor_lane<E: Endpoint>(endpoint: &E, h: &str, lane: &mut SupLane<E::Client>) -> Result<SupervisorPhase, LaneError> {
    let now = Instant::now();
    let answered = supervisor_status::<E>(&lane.conn, &mut lane.reader).map(|(_, _, phase)| phase);
    if answered.is_ok() {
        lane.redial_at = None;
        return answered;
    }
    // The probe's own deadline shut this socket down (`cancel` is
    // `shutdown(SHUT_RDWR)`), so the lane is dead from here on whatever
    // the supervisor does next -- one stalled link would otherwise
    // leave every later probe failing and the header lying until the
    // next reattach. Re-dial once the wait its lifetime earned has passed
    // (`Redial`: a lane that lasted `STABLE` re-dials at once); the next
    // answered probe restores "attached".
    let due = *lane.redial_at.get_or_insert_with(|| lane.dialed_at + lane.redial.after(now.saturating_duration_since(lane.dialed_at)));
    if now >= due {
        lane.dialed_at = now;
        lane.redial_at = match connect_supervisor_lane::<E>(endpoint, h) {
            Ok((c, _)) => {
                lane.conn = c;
                lane.reader = FrameReader::new();
                None
            }
            Err(_) => Some(now + lane.redial.after(Duration::ZERO)),
        };
    }
    answered
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
/// unresponsive count via `clear_unresponsive`.
pub(super) fn on_supervisor_absent_or_unresponsive<E: Endpoint>(
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
            LaneError::Unreachable(_) | LaneError::Undetermined(_) | LaneError::LinkDown => {
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
            _ => unreachable!("classify_transport only ever produces Io/Refused/Unreachable/Undetermined/LinkDown"),
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
pub(super) enum ReadyOutcome<E: Endpoint> {
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
/// Every message goes through [`hold`], exactly as in
/// `wait_for_retry_or_shutdown`; `Shutdown` (or a disconnected channel) is
/// reported for the caller to act on immediately.
pub(super) fn drain_pending_control(cmd_rx: &Receiver<WorkerMsg>, held: &mut Held) -> Option<WaitOutcome> {
    loop {
        match cmd_rx.try_recv() {
            Ok(msg) => {
                if let Some(outcome) = hold(msg, held) {
                    return Some(outcome);
                }
            }
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => return Some(WaitOutcome::Shutdown),
        }
    }
}

/// How long a first attach tolerates `EndedNoRespawn` before treating it
/// as terminal -- sized to the daemon's own dominant retire cost.
pub(super) const FIRST_ATTACH_ENDED_NO_RESPAWN_BOUND: Duration = TEARDOWN_AGGREGATE_DEADLINE;

/// ADR 0043 decision 28, ADR 0045 decision 6: the attach client converges
/// on the supervisor's OWN word ONLY, never on a pointer file. Given an
/// already-connected, already-`hello`'d supervisor lane, polls `Status`
/// on that SAME connection every [`rules::RECONNECT_BACKOFF_INITIAL`]
/// (a FIXED interval — Codex review round finding 6: this loop is
/// steady-state polling of a lane that is actively ANSWERING, so
/// [`ReconnectState::retry_with_backoff`]'s doubling does not apply to
/// that poll; it does apply to the wait after a failed voyage dial) until
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
pub(super) fn converge_on_ready<E: Endpoint>(
    endpoint: &E,
    mut conn: E::Client,
    mut sup_reader: FrameReader,
    h: &str,
    cmd_rx: &Receiver<WorkerMsg>,
    reconnect: &mut ReconnectState,
    held: &mut Held,
    quit: &mut QuitDispatcher,
    outstanding: &mut OutstandingSlot,
    first_attach_deadline: Option<Instant>,
    viewed: &AtomicBool,
    emit: &dyn Fn(WorkerEvent),
) -> ReadyOutcome<E> {
    // Emitted at most once per "still starting" spell — re-armed every
    // time a latched Quit or a fresh Ready round makes the NEXT status
    // worth announcing again as a fresh wait.
    let mut emitted_starting = false;

    loop {
        let (sv, _leg, phase) = match supervisor_status::<E>(&conn, &mut sup_reader) {
            Ok(v) => v,
            Err(_) => {
                endpoint.drop_spare();
                return ReadyOutcome::LaneDown;
            }
        };
        // Finding 2: an answered Status, whatever its phase, proves the
        // supervisor lane is not the thing that is unresponsive right
        // now -- clear the clock unconditionally, before any Terminal
        // classification or gating below.
        reconnect.clear_unresponsive();
        // Tolerate EndedNoRespawn only within the first attach's deadline.
        let tolerate_ended_no_respawn = phase == SupervisorPhase::EndedNoRespawn
            && first_attach_deadline.is_some_and(|d| Instant::now() < d);
        if !tolerate_ended_no_respawn {
            if let ReconnectDecision::Terminal(reason) = reconnect.classify_supervisor_phase(phase) {
                return ReadyOutcome::Terminal(format!("supervisor: {reason:?}"));
            }
        }

        // Path (ii): a latched Quit needs only the supervisor lane and a
        // known voyage id -- dispatched here, BEFORE the Ready gate
        // below, so a quit never waits on a supervisor that is still
        // starting.
        if let Some(id) = sv.clone() {
            if let Some(reason) = held.quit.take() {
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
            match wait_for_retry_or_shutdown(cmd_rx, rules::RECONNECT_BACKOFF_INITIAL, held) {
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
        if let Some(WaitOutcome::Shutdown) = drain_pending_control(cmd_rx, held) {
            return ReadyOutcome::Shutdown;
        }
        if let Some(reason) = held.quit.take() {
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
            // arms clear the health window's clock and wait the doubling
            // `ReconnectState::retry_with_backoff` (ADR 0043 decision 28:
            // over an ssh lane every failed dial is a login).
            Err(e) => match classify_transport(e) {
                LaneError::Refused { code, detail } => {
                    let msg = if code == "no_bridge" {
                        "this daemon has no bridge — it predates the lane bridge (ADR 0045)".to_string()
                    } else {
                        format!("voyage pipe: daemon refused ({code}): {detail}")
                    };
                    return ReadyOutcome::Terminal(msg);
                }
                LaneError::LinkDown => {
                    reconnect.clear_unresponsive();
                    match pause_for_link(endpoint, cmd_rx, held, viewed, emit) {
                        WaitOutcome::Shutdown => return ReadyOutcome::Shutdown,
                        WaitOutcome::Continue => continue,
                    }
                }
                e @ (LaneError::Unreachable(_) | LaneError::Undetermined(_)) => {
                    reconnect.clear_unresponsive();
                    let msg = match &e {
                        LaneError::Unreachable(d) => format!("daemon unreachable — retrying ({d})"),
                        LaneError::Undetermined(_) => "daemon could not identify the lane — retrying".to_string(),
                        _ => unreachable!("matched above"),
                    };
                    emit(WorkerEvent::Status(msg));
                    match wait_for_retry_or_shutdown(cmd_rx, reconnect.retry_with_backoff(), held) {
                        WaitOutcome::Shutdown => return ReadyOutcome::Shutdown,
                        WaitOutcome::Continue => continue,
                    }
                }
                LaneError::Io(io) => {
                    if is_access_denied(&io) {
                        return ReadyOutcome::Terminal("voyage pipe: access denied".to_string());
                    }
                    emit(WorkerEvent::Status(format!("voyage pipe not yet available: {io}")));
                    match wait_for_retry_or_shutdown(cmd_rx, reconnect.retry_with_backoff(), held) {
                        WaitOutcome::Shutdown => return ReadyOutcome::Shutdown,
                        WaitOutcome::Continue => continue,
                    }
                }
                _ => unreachable!("classify_transport only ever produces Io/Refused/Unreachable/Undetermined/LinkDown"),
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
pub(super) enum HelloOutcome {
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
pub(super) fn attach_lane_hello<E: Endpoint>(
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
pub(super) fn checkpoint_frame_deadline(now: Instant, transfer_deadline: Instant) -> Instant {
    (now + STATUS_BUDGET).min(transfer_deadline)
}

/// Sends `attach{controller_id}` (always arrives as a WATCHER — ADR
/// 0037's who-may-type) and reassembles the checkpoint transfer, bounded
/// at [`wire::MAX_CHECKPOINT_LEN`] the same way `tests/e2e_pipe.rs`'s own
/// `RealFrames::collect_checkpoint` proves the property, and at
/// [`CHECKPOINT_TRANSFER_BUDGET`] in aggregate (see its own doc).
pub(super) fn attach_and_collect_checkpoint<E: Endpoint>(
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
            DecodedFrame::AttachServer(AttachServer::AttachRefused { reason }) => {
                // The episode still retries (neither reason is in the
                // ADR's terminal list), but the reason RIDES OUT: only
                // `GroundTimeout` is transient, and `SubscriberCap` held
                // by orphaned watchers is permanent -- the caller names
                // it in the row's status line rather than retrying in
                // silence behind a pane that still reads "connecting...".
                return Err(LaneError::AttachRefused(reason));
            }
            DecodedFrame::AttachServer(AttachServer::Output { .. }) => {
                return Err(LaneError::Protocol("live output arrived before checkpoint completed"));
            }
            _ => return Err(LaneError::Protocol("unexpected frame during checkpoint transfer")),
        }
    }
}
