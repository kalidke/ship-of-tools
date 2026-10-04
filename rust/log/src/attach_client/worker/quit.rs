//! The quit path: `run_end_run_and_wait`, `run_quit` and the supervisor-lane reconnect it needs.

use crate::lane::client::Endpoint;
use crate::attach_client::rules::{OutstandingSlot, QuitDispatcher, QuitState};
use crate::lane::wire::{self, DecodedFrame, SupervisorOp, SupervisorReply, SupervisorRequest};
use std::thread;
use std::time::{Duration, Instant};

use super::*;


/// Steady heartbeat cadence for `run_quit`'s own loop, chosen against a
/// CONFIRMED read of U2's own idle-eviction code (`supervisor/`'s
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
/// `attach_client::rules::QUIT_CUTOFF` (90 s, ADR 0041's own bound-graph figure)
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
pub(super) fn run_quit<E: Endpoint>(
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
pub(super) fn reconnect_supervisor_lane_for_quit<E: Endpoint>(
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
