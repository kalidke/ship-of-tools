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

use crate::fe_client::{QuitDispatcher, QuitState};
use crate::fe_client_win::{run_end_run_and_wait, FrameReader};
use crate::pipe_win::TEARDOWN_AGGREGATE_DEADLINE;
use crate::supervisor::{connect_and_challenge, err_state, send_and_read};
use crate::wire::{SupervisorOp, SupervisorOperationState, SupervisorPhase, SupervisorReply, SupervisorRequest};
use std::path::Path;
use std::time::{Duration, Instant};

/// Re-exported so a caller outside this crate (`sot-backend`'s
/// `capsule_workspace.rs`) can name the retained-process type this
/// module's own [`connect`]/[`query_status`] return without also
/// depending on `sot_log::challenge` directly — the SAME type, not a
/// second one: `ChallengedProcess` IS the retained handle the challenge
/// proves, reused here rather than wrapped.
pub use crate::challenge::ChallengedProcess;

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

/// What a `status` round trip reports — [`wire::SupervisorPhase`] reused
/// directly rather than a second local enum, since this module adds no
/// meaning to it beyond relaying it.
#[derive(Debug, Clone)]
pub struct StatusReport {
    pub voyage: Option<String>,
    pub leg: Option<u64>,
    pub phase: SupervisorPhase,
}

/// What [`end_run`] settled on — [`QuitState`]'s own terminal vocabulary
/// (ADR 0042 L1a, Codex review finding 4: `end_run` shares
/// [`run_end_run_and_wait`] with the FE's own quit path rather than
/// carrying a second state machine, so its outcomes are exactly that
/// dispatcher's). `RecordClosed` is not one of `QuitDispatcher`'s own
/// states — `Verifying` is non-terminal, so the dispatcher alone cannot
/// distinguish "closed, not yet verified when the 90 s cutoff hit" from
/// "never even got a reply." [`end_run`] recovers that ONE extra bit
/// itself, externally, via `on_transition` (never touching
/// `QuitDispatcher`'s shared state machine) — see that function's own
/// comment.
#[derive(Debug, Clone)]
pub enum EndRunOutcome {
    /// The run ended and its record verified green.
    RecordVerified,
    /// The run ended (the marker committed — `record_closed` was
    /// observed) but the authority's own O(retained history)
    /// `record_verified` walk had not completed by the ADR's 90 s
    /// cutoff. The marker itself is the irrevocable acceptance (ADR
    /// 0041 Lifecycle), so a caller may still treat this as "ended."
    RecordClosed,
    /// The authority reports the operation failed — never ended.
    Failed(String),
    /// Voyage-fenced or id-conflict refusal — the caller's observed
    /// voyage was stale, or `operation_id` collided with a different
    /// command's digest.
    Refused(String),
    /// Neither `record_closed` nor a terminal reply was ever observed
    /// before `QuitDispatcher`'s own ADR-pinned 90 s cutoff — genuinely
    /// unknown, not merely slow.
    OutcomeUnknown,
}

/// Connect the supervisor lane at `state_dir` and run the full
/// same-connection challenge with this crate's own build identity — the
/// production analog of `supervisor::connect_and_challenge_for_test`,
/// reusing the exact same [`connect_and_challenge`] the test helper now
/// delegates to.
fn connect(
    state_dir: &Path,
    deadline: Instant,
) -> crate::Result<(crate::pipe_win::PipeClient, ChallengedProcess)> {
    let h = crate::supervisor::state_dir_hash(state_dir);
    connect_and_challenge(&h, crate::exchange::SUPERVISOR_LANE_BUILD_ID, deadline)
}

/// Connect, challenge, and run one `status` request — everything a
/// caller needs to map a capsule workspace's supervisor lane to a
/// `runtime: "capsule"` `workspace.list` row's `phase` (ADR 0042 L1a).
/// Any failure — connect refused, the challenge proving `Foreign` or
/// `Undetermined`, a timeout, a malformed reply — is folded into one
/// `Err`: the caller has no use here for distinguishing WHY the lane is
/// unreachable, only THAT it is (`workspace.list`'s own "failure ->
/// unreachable" rule).
///
/// Returns the [`ChallengedProcess`] ALONGSIDE the status (round-2 Codex
/// finding, daemon-boot-adopts-supervisor fix): the challenge already
/// proves and retains a live handle to the process on the other end of
/// this connection, and a caller that just ADOPTED a lane (found it
/// alive rather than spawning into it) needs exactly that handle as its
/// own death signal — the same role a spawned child's own `Child` plays
/// for a leg this process spawned itself. A caller with no use for it
/// (most callers) simply drops the second element; dropping closes the
/// handle.
pub fn query_status(state_dir: &Path) -> crate::Result<(StatusReport, ChallengedProcess)> {
    let deadline = Instant::now() + CONNECT_AND_HELLO_BUDGET;
    let (conn, process) = connect(state_dir, deadline)?;
    match send_and_read(&conn, &SupervisorRequest::Status, Instant::now() + STATUS_BUDGET)? {
        SupervisorReply::StatusOk { voyage, leg, phase, .. } => Ok((StatusReport { voyage, leg, phase }, process)),
        other => Err(err_state(format!("expected status_ok, got {other:?}"))),
    }
}

/// Connect, challenge, and send `stop` — the authority acknowledges
/// `stopping` and then exits, while its own capsule LEG survives:
/// "Legs are spawned as CHILD PROCESSES and deliberately NOT placed in
/// the supervisor's job... the supervisor dying must be harmless to the
/// run, which is the whole reason adoption exists" (ADR 0041 Lifecycle).
/// This is therefore the clean, protocol-level way to end JUST the
/// authority — used today by `tests/capsule_workspaces.rs` (ADR 0042
/// L1a, Codex review finding 13) to prove ADOPTION (a fresh `--resume`
/// finding the SAME leg still alive) rather than mere detachment (an
/// untouched, already-running supervisor surviving a daemon restart).
///
/// WAITS for confirmed process death after the ACK (round-2 Codex
/// finding, daemon-boot-adopts-supervisor fix): an earlier version
/// returned the instant `Stopping` was acknowledged, before the process
/// had actually exited or released `supervisor.lock` — a caller that
/// immediately acted on "stopped" (e.g. starting a fresh authority)
/// could still race the old one's own teardown. The RPC connection is
/// dropped first (never held open across a wait the peer has no reason
/// to answer on), then this blocks on the SAME retained
/// [`ChallengedProcess`] handle [`query_status`]'s own caller would use
/// as a death signal, bounded by [`TEARDOWN_AGGREGATE_DEADLINE`] — the
/// authority's own documented worst-case teardown budget (it drops its
/// lane before releasing the fence), so a caller that waits this long
/// and still sees no exit has a genuine, reportable problem, not mere
/// impatience.
pub fn stop(state_dir: &Path) -> crate::Result<()> {
    let deadline = Instant::now() + CONNECT_AND_HELLO_BUDGET;
    let (conn, process) = connect(state_dir, deadline)?;
    let operation_id = format!("sot-backend-stop-{}", uuid::Uuid::now_v7());
    let request = SupervisorRequest::Command { operation_id, op: SupervisorOp::Stop };
    match send_and_read(&conn, &request, Instant::now() + STATUS_BUDGET)? {
        SupervisorReply::Operation(SupervisorOperationState::Stopping) => {}
        other => return Err(err_state(format!("expected Operation(Stopping), got {other:?}"))),
    }
    // Close the RPC connection first -- the peer owes it no further
    // reply once it has accepted `stopping`, so holding it open across
    // the wait below only delays ITS OWN teardown for no benefit here.
    drop(conn);
    match process.wait(TEARDOWN_AGGREGATE_DEADLINE) {
        Ok(true) => Ok(()),
        Ok(false) => Err(err_state(format!(
            "supervisor acknowledged stop but did not exit within {TEARDOWN_AGGREGATE_DEADLINE:?}"
        ))),
        Err(e) => Err(err_state(format!("waiting for the stopped supervisor to exit: {e}"))),
    }
}

/// Connect, challenge, and run [`run_end_run_and_wait`] — the SAME
/// end_run+heartbeat-query loop `fe_client_win.rs`'s own `run_quit` uses
/// (ADR 0042 L1a, Codex review finding 4), bounded by that function's own
/// ADR-pinned `fe_client::QUIT_CUTOFF` (90 s), never a daemon-invented
/// budget. `voyage` MUST be the voyage the caller most recently observed
/// via [`query_status`] — lifecycle commands are voyage-fenced (ADR 0041
/// Lifecycle), so a stale value is safely refused rather than mutated
/// against.
pub fn end_run(state_dir: &Path, voyage: &str, reason: &str) -> crate::Result<EndRunOutcome> {
    let hello_deadline = Instant::now() + CONNECT_AND_HELLO_BUDGET;
    let (mut conn, _process) = connect(state_dir, hello_deadline)?;
    let mut reader = FrameReader::new();
    let h = crate::supervisor::state_dir_hash(state_dir);
    let mut quit = QuitDispatcher::new();
    let operation_id = format!("sot-backend-end-run-{}", uuid::Uuid::now_v7());
    // Recovers the `record_closed`-but-not-yet-`record_verified` case
    // `QuitDispatcher`'s own terminal states cannot express on their
    // own (see `EndRunOutcome::RecordClosed`'s doc) — a plain external
    // observer of the SAME transitions `run_quit` already emits as UI
    // events, never a second copy of the dispatcher's own logic.
    let mut observed_record_closed = false;
    run_end_run_and_wait(
        &mut conn,
        &mut reader,
        |c, r| {
            if let Ok((new_conn, _process)) =
                connect_and_challenge(&h, crate::exchange::SUPERVISOR_LANE_BUILD_ID, Instant::now() + CONNECT_AND_HELLO_BUDGET)
            {
                *c = new_conn;
                *r = FrameReader::new();
            }
        },
        &mut quit,
        operation_id,
        reason.to_string(),
        voyage,
        |quit| {
            if matches!(quit.state(), QuitState::Verifying { .. } | QuitState::Ended) {
                observed_record_closed = true;
            }
        },
    );
    Ok(match quit.state() {
        QuitState::Ended => EndRunOutcome::RecordVerified,
        QuitState::Failed { detail } => EndRunOutcome::Failed(detail.clone()),
        QuitState::Refused { reason } => EndRunOutcome::Refused(format!("{reason:?}")),
        _ if observed_record_closed => EndRunOutcome::RecordClosed,
        _ => EndRunOutcome::OutcomeUnknown,
    })
}

/// Bound for [`reset`]'s own poll-to-completion after the command is
/// accepted — matches the authority's private `RESETTING_WATCHDOG`
/// (`supervisor.rs`, 30s), the reset transaction's own worst-case
/// budget.
const RESET_BUDGET: Duration = Duration::from_secs(30);
const RESET_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Connect, challenge, and send `reset` — mirrors [`stop`]'s shape.
/// `reset` is the ONE operation an authority resting in `EndedNoRespawn`
/// admits; it mints a FRESH voyage and spawns a new leg over the SAME
/// resident authority — the deliberate alternative to `--resume`, which
/// never resurrects a voyage whose last leg carries the
/// `run_end_requested` marker. Fenced against the voyage observed via
/// this call's own `status` request (voyage-fenced like every lifecycle
/// command).
///
/// The command's own reply is an immediate `Accepted` — the transaction
/// (mint + bootstrap + publish) runs asynchronously, so this polls
/// `Query{operation_id}` on the SAME connection for the terminal
/// `ResetDone { new_voyage }`, bounded by [`RESET_BUDGET`]. Returns the
/// new voyage id.
pub fn reset(state_dir: &Path) -> crate::Result<String> {
    let deadline = Instant::now() + CONNECT_AND_HELLO_BUDGET;
    let (conn, _process) = connect(state_dir, deadline)?;
    let voyage = match send_and_read(
        &conn,
        &SupervisorRequest::Status,
        Instant::now() + STATUS_BUDGET,
    )? {
        SupervisorReply::StatusOk {
            voyage: Some(v), ..
        } => v,
        other => {
            return Err(err_state(format!(
                "reset: expected status_ok with a voyage, got {other:?}"
            )))
        }
    };
    let operation_id = format!("sot-backend-reset-{}", uuid::Uuid::now_v7());
    let request = SupervisorRequest::Command {
        operation_id: operation_id.clone(),
        op: SupervisorOp::Reset {
            voyage: Some(voyage),
        },
    };
    match send_and_read(&conn, &request, Instant::now() + STATUS_BUDGET)? {
        SupervisorReply::Operation(SupervisorOperationState::Accepted) => {}
        other => {
            return Err(err_state(format!(
                "reset: expected Operation(Accepted), got {other:?}"
            )))
        }
    }
    let poll_deadline = Instant::now() + RESET_BUDGET;
    loop {
        let query = SupervisorRequest::Query {
            operation_id: operation_id.clone(),
        };
        match send_and_read(&conn, &query, Instant::now() + STATUS_BUDGET)? {
            SupervisorReply::Operation(SupervisorOperationState::ResetDone { new_voyage }) => {
                return Ok(new_voyage)
            }
            SupervisorReply::Operation(SupervisorOperationState::Accepted) => {} // still in flight
            SupervisorReply::Operation(other) => {
                return Err(err_state(format!("reset did not complete: {other:?}")))
            }
            other => {
                return Err(err_state(format!(
                    "reset: expected an Operation reply, got {other:?}"
                )))
            }
        }
        if Instant::now() >= poll_deadline {
            return Err(err_state(format!(
                "reset accepted but did not complete within {RESET_BUDGET:?}"
            )));
        }
        std::thread::sleep(RESET_POLL_INTERVAL);
    }
}
