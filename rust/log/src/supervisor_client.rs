#![cfg(windows)]
//! ADR 0042 slice L1a: a small, PRODUCTION supervisor-lane client for a
//! caller OUTSIDE this crate that is not the FE — today, the backend
//! daemon's own capsule workspace runtime (`sot-backend`'s
//! `workspaces.rs`). `fe_client_win.rs` already runs this exact
//! connect+hello(build identity)+challenge procedure
//! (`connect_supervisor_lane`) and its own `status` round trip
//! (`supervisor_status`), but both are private fns in that file — right,
//! since the FE's own six rulings own everything downstream of them
//! there. This module is the SAME procedure's production entry point for
//! a caller that only ever needs `status` and `end_run`: it adds no new
//! wire behavior, only the external-facing functions that did not exist
//! yet. `supervisor::connect_and_challenge_for_test` /
//! `request_for_test` are the nearest existing public surface, but both
//! are `#[cfg(any(test, feature = "test-support"))]` — "never enabled by
//! a normal consumer" per this crate's own `Cargo.toml` — and the daemon
//! is a normal consumer, not a test, so it needs its own, ungated path.
//! Every piece below is reused, not reimplemented:
//! `pipe_win::connect_supervisor_pipe_unchallenged`,
//! `supervisor::{state_dir_hash, connect_and_challenge, send_and_read,
//! err_state}`, `exchange::{SupervisorLaneExchange,
//! SUPERVISOR_LANE_BUILD_ID}`, and `wire`'s supervisor-lane frames.

use crate::supervisor::{connect_and_challenge, err_state, send_and_read};
use crate::wire::{SupervisorOp, SupervisorOperationState, SupervisorPhase, SupervisorReply, SupervisorRequest};
use std::path::Path;
use std::time::{Duration, Instant};

/// ADR 0041 Lifecycle "Every op has one budget: connect 2 s, request
/// write 2 s..." — the same figure `fe_client_win.rs`'s own
/// `HELLO_BUDGET`/`WRITE_BUDGET` pin, reused here as this module's own
/// connect+challenge deadline for the same reason: hello doubles as the
/// challenge's own steps 4-5 exchange (`SupervisorRequest::Hello`'s own
/// doc), so one fixed, single-round-trip budget covers both.
const CONNECT_AND_HELLO_BUDGET: Duration = Duration::from_secs(2);
/// "Every client's first act, after the identity check above, is a
/// `status` with a 5 s budget; a lane that accepts but does not answer
/// within it is treated exactly as an absent lane." Matches
/// `fe_client_win.rs`'s own `STATUS_BUDGET`.
const STATUS_BUDGET: Duration = Duration::from_secs(5);
/// How often [`end_run`] re-polls `query` while an operation is still
/// `Accepted`/`RecordClosed` — a UI-facing poll cadence, not an
/// ADR-pinned figure (matching `LANE_IDLE_DEADLINE`'s own "reasoned, not
/// pinned" status in `supervisor.rs`).
const END_RUN_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// What a `status` round trip reports — [`wire::SupervisorPhase`] reused
/// directly rather than a second local enum, since this module adds no
/// meaning to it beyond relaying it.
#[derive(Debug, Clone)]
pub struct StatusReport {
    pub voyage: Option<String>,
    pub leg: Option<u64>,
    pub phase: SupervisorPhase,
}

/// What [`end_run`] settled on. `RecordClosed` (rather than
/// `RecordVerified`) is a legitimate, honestly-reported outcome: it means
/// the run genuinely ended (the marker committed) but this call's own
/// budget ran out before the authority's O(retained history)
/// `record_verified` walk finished — never "unknown", since the request
/// was accepted and the record did close.
#[derive(Debug, Clone)]
pub enum EndRunOutcome {
    /// The run ended and its record verified green.
    RecordVerified,
    /// The run ended (the marker committed) but verification had not
    /// completed within this call's own budget.
    RecordClosed,
    /// The authority reports the operation failed — never ended.
    Failed(String),
    /// Voyage-fenced or id-conflict refusal — the caller's observed
    /// voyage was stale, or `operation_id` collided with a different
    /// command's digest.
    Refused(String),
    /// The `operation_id` this call minted was, impossibly, unknown to
    /// the authority it was just accepted by — surfaced rather than
    /// silently retried, since a production caller should never see this.
    UnknownOperation,
    /// The command was accepted but neither `record_closed` nor a
    /// terminal state arrived within this call's own budget — genuinely
    /// unknown, unlike `RecordClosed` above.
    TimedOut,
}

/// Connect the supervisor lane at `state_dir` and run the full
/// same-connection challenge with this crate's own build identity — the
/// production analog of `supervisor::connect_and_challenge_for_test`,
/// reusing the exact same [`connect_and_challenge`] the test helper now
/// delegates to.
fn connect(state_dir: &Path, deadline: Instant) -> crate::Result<crate::pipe_win::PipeClient> {
    let h = crate::supervisor::state_dir_hash(state_dir);
    let (conn, _process) = connect_and_challenge(&h, crate::exchange::SUPERVISOR_LANE_BUILD_ID, deadline)?;
    Ok(conn)
}

/// Connect, challenge, and run one `status` request — everything a
/// caller needs to map a capsule workspace's supervisor lane to a
/// `runtime: "capsule"` `workspace.list` row's `phase` (ADR 0042 L1a).
/// Any failure — connect refused, the challenge proving `Foreign` or
/// `Undetermined`, a timeout, a malformed reply — is folded into one
/// `Err`: the caller has no use here for distinguishing WHY the lane is
/// unreachable, only THAT it is (`workspace.list`'s own "failure ->
/// unreachable" rule).
pub fn query_status(state_dir: &Path) -> crate::Result<StatusReport> {
    let deadline = Instant::now() + CONNECT_AND_HELLO_BUDGET;
    let conn = connect(state_dir, deadline)?;
    match send_and_read(&conn, &SupervisorRequest::Status, Instant::now() + STATUS_BUDGET)? {
        SupervisorReply::StatusOk { voyage, leg, phase, .. } => Ok(StatusReport { voyage, leg, phase }),
        other => Err(err_state(format!("expected status_ok, got {other:?}"))),
    }
}

/// Connect, challenge, submit `end_run {reason, voyage}`, and poll
/// `query` until a terminal state or `budget` elapses (ADR 0042 L1a:
/// `workspace.delete` on a capsule workspace). `voyage` MUST be the
/// voyage the caller most recently observed via [`query_status`] —
/// lifecycle commands are voyage-fenced (ADR 0041 Lifecycle), so a stale
/// value is safely refused rather than mutated against.
///
/// `budget` bounds the WHOLE call (connect + hello + command + every
/// query poll) — never merely the final poll — so a caller can cap total
/// wall time regardless of how the authority's own O(retained history)
/// verification walk is going.
pub fn end_run(state_dir: &Path, voyage: &str, reason: &str, budget: Duration) -> crate::Result<EndRunOutcome> {
    let deadline = Instant::now() + budget;
    let conn = connect(state_dir, deadline.min(Instant::now() + CONNECT_AND_HELLO_BUDGET))?;
    let operation_id = uuid::Uuid::now_v7().to_string();
    let op = SupervisorOp::EndRun {
        reason: reason.to_string(),
        voyage: voyage.to_string(),
    };
    let request = SupervisorRequest::Command { operation_id: operation_id.clone(), op };
    let mut state = match send_and_read(&conn, &request, deadline)? {
        SupervisorReply::Operation(state) => state,
        other => return Err(err_state(format!("expected an Operation reply to command, got {other:?}"))),
    };
    loop {
        match state {
            SupervisorOperationState::RecordVerified => return Ok(EndRunOutcome::RecordVerified),
            SupervisorOperationState::Failed { detail } => return Ok(EndRunOutcome::Failed(detail)),
            SupervisorOperationState::Refused { reason } => return Ok(EndRunOutcome::Refused(format!("{reason:?}"))),
            SupervisorOperationState::UnknownOperation => return Ok(EndRunOutcome::UnknownOperation),
            SupervisorOperationState::Accepted | SupervisorOperationState::RecordClosed => {
                let now = Instant::now();
                if now >= deadline {
                    return Ok(match state {
                        SupervisorOperationState::RecordClosed => EndRunOutcome::RecordClosed,
                        _ => EndRunOutcome::TimedOut,
                    });
                }
                std::thread::sleep(END_RUN_POLL_INTERVAL.min(deadline.saturating_duration_since(now)));
                let query = SupervisorRequest::Query { operation_id: operation_id.clone() };
                state = match send_and_read(&conn, &query, deadline)? {
                    SupervisorReply::Operation(state) => state,
                    other => return Err(err_state(format!("expected an Operation reply to query, got {other:?}"))),
                };
            }
            // `Stopping`/`ResetDone` are other command families' own
            // terminal shapes; the shared `SupervisorOperationState`
            // vocabulary makes them reachable here in the TYPE, but the
            // authority never actually answers an EndRun command/query
            // with either — a protocol violation, not a case this
            // caller has a meaningful outcome for.
            SupervisorOperationState::Stopping | SupervisorOperationState::ResetDone { .. } => {
                return Err(err_state(format!("unexpected operation state answering end_run: {state:?}")));
            }
        }
    }
}
