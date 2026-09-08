#![cfg(any(windows, target_os = "linux"))]
//! ADR 0042 slice L1a, generalized by L1-unix LU3b (ADR 0043 decision
//! 20): a small, PRODUCTION supervisor-lane client for a caller OUTSIDE
//! this crate that is not the FE — today, the backend daemon's own
//! capsule workspace runtime (`sot-backend`'s `capsule_workspace.rs`).
//! `fe_client_io.rs` already runs this exact connect+hello(build
//! identity)+challenge procedure (`connect_and_challenge`, moved HERE
//! from `supervisor.rs` this lane) and its own `status` round trip
//! (`fe_client_io::supervisor_status`, still private there — right, since
//! the FE's own six rulings own everything downstream of it there). This
//! module is the SAME procedure's production entry point for a caller
//! that only ever needs `status`/`stop`/`end_run`/`reset`: it adds no new
//! wire behavior, only the external-facing functions that did not exist
//! yet. `supervisor::connect_and_challenge_for_test` / `request_for_test`
//! are the nearest existing public surface, but both are
//! `#[cfg(any(test, feature = "test-support"))]` — "never enabled by a
//! normal consumer" per this crate's own `Cargo.toml` — and the daemon is
//! a normal consumer, not a test, so it needs its own, ungated path.
//!
//! Generic over [`Endpoint`], instantiated at [`PlatformEndpoint`] for
//! every `pub fn` below — the daemon keeps calling
//! `sot_log::supervisor_client::{query_status, stop, end_run, reset}`
//! unchanged; only the TYPE `ChallengedProcess` names now resolves via
//! `PlatformEndpoint` rather than hard-coding `challenge_win`'s. The
//! connect/send/read/error helpers this module and `supervisor.rs` both
//! need (`connect_and_challenge`, `send_and_read`, `read_one_frame`,
//! `err_state`) moved HERE from `supervisor.rs` this lane, alongside
//! `state_dir_hash` (moved to `state_dir.rs`, a neutral pure function) —
//! the dependency now points server (`supervisor.rs`) -> client-helpers
//! (this module), the right way round: a supervisor-lane CLIENT's own
//! helpers should not have lived inside the SERVER module they were
//! factored out of in the first place. Every piece below is reused, not
//! reimplemented: `Endpoint::connect_supervisor_unchallenged`,
//! `Endpoint::challenge`, `exchange::{SupervisorLaneExchange,
//! SUPERVISOR_LANE_BUILD_ID}`, and `wire`'s supervisor-lane frames.

use crate::client::{Client, Endpoint, PlatformEndpoint};
use crate::fe_client::{QuitDispatcher, QuitState};
use crate::fe_client_io::{run_end_run_and_wait, FrameReader};
use crate::transport::TEARDOWN_AGGREGATE_DEADLINE;
use crate::wire::{
    self, DecodedFrame, SupervisorOp, SupervisorOperationState, SupervisorPhase, SupervisorReply, SupervisorRequest,
};
use std::path::Path;
use std::time::{Duration, Instant};

/// The retained-process type every `pub fn` below returns/accepts —
/// [`PlatformEndpoint`]'s own `Process`, so a caller outside this crate
/// (`sot-backend`'s `capsule_workspace.rs`) can name it without depending
/// on `challenge_win`/`challenge_unix` directly. The SAME type Windows
/// named before this lane (`challenge_win::ChallengedProcess`, since
/// `PlatformEndpoint = PipeEndpoint` there) — zero backend edits
/// expected.
pub type ChallengedProcess = <PlatformEndpoint as Endpoint>::Process;

/// ADR 0041 Lifecycle "Every op has one budget: connect 2 s, request
/// write 2 s..." — the same figure `fe_client_io.rs`'s own
/// `HELLO_BUDGET`/`WRITE_BUDGET` pin, reused here as this module's own
/// connect+challenge deadline for the same reason: hello doubles as the
/// challenge's own steps 4-5 exchange (`SupervisorRequest::Hello`'s own
/// doc), so one fixed, single-round-trip budget covers both.
const CONNECT_AND_HELLO_BUDGET: Duration = Duration::from_secs(2);
/// "Every client's first act, after the identity check above, is a
/// `status` with a 5 s budget; a lane that accepts but does not answer
/// within it is treated exactly as an absent lane." Matches
/// `fe_client_io.rs`'s own `STATUS_BUDGET`.
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
) -> crate::Result<(<PlatformEndpoint as Endpoint>::Client, ChallengedProcess)> {
    let h = crate::state_dir::state_dir_hash(state_dir);
    connect_and_challenge::<PlatformEndpoint>(&h, crate::exchange::SUPERVISOR_LANE_BUILD_ID, deadline)
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
/// end_run+heartbeat-query loop `fe_client_io.rs`'s own `run_quit` uses
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
    let h = crate::state_dir::state_dir_hash(state_dir);
    let mut quit = QuitDispatcher::new();
    let operation_id = format!("sot-backend-end-run-{}", uuid::Uuid::now_v7());
    // Recovers the `record_closed`-but-not-yet-`record_verified` case
    // `QuitDispatcher`'s own terminal states cannot express on their
    // own (see `EndRunOutcome::RecordClosed`'s doc) — a plain external
    // observer of the SAME transitions `run_quit` already emits as UI
    // events, never a second copy of the dispatcher's own logic.
    let mut observed_record_closed = false;
    run_end_run_and_wait::<PlatformEndpoint>(
        &mut conn,
        &mut reader,
        |c, r| {
            if let Ok((new_conn, _process)) = connect_and_challenge::<PlatformEndpoint>(
                &h,
                crate::exchange::SUPERVISOR_LANE_BUILD_ID,
                Instant::now() + CONNECT_AND_HELLO_BUDGET,
            ) {
                *c = new_conn;
                *r = FrameReader::new();
                true
            } else {
                false
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

// ---------------------------------------------------------------------
// L1-unix LU3b: the client-side supervisor-lane helpers, moved here from
// `supervisor.rs` (ADR 0043 decision 20) — a supervisor-lane CLIENT's own
// connect/send/read/error primitives belong in the client module, not
// the server module they were factored out of; `supervisor.rs` (the
// server, still Windows-only until LU3c) now imports `err_state` from
// HERE and its three test-support helpers instantiate
// [`connect_and_challenge`]/[`send_and_read`] at `pipe_win::PipeEndpoint`
// explicitly, the same way this module's own `pub fn`s instantiate them
// at [`PlatformEndpoint`].
// ---------------------------------------------------------------------

/// Connect the supervisor lane by state-dir hash and run the full
/// same-connection challenge with THIS build's own identity, folding
/// `Foreign`/`Undetermined` straight into `Err` — a production caller
/// (this module's own [`connect`], and `supervisor::
/// connect_and_challenge_for_test`) has no use for telling those two
/// apart any further than "not a proven connection to my own
/// supervisor".
pub(crate) fn connect_and_challenge<E: Endpoint>(
    h: &str,
    build: &str,
    deadline: Instant,
) -> crate::Result<(E::Client, E::Process)> {
    let conn = E::connect_supervisor_unchallenged(h)?;
    let mut exchange = crate::exchange::SupervisorLaneExchange::new(build.to_string());
    match E::challenge(&conn, &mut exchange, deadline) {
        crate::challenge::ChallengeOutcome::Proven(process) => Ok((conn, process)),
        // ADR 0030 §8 decision 31c: the ONE `Foreign` cause that is
        // typed, not text — `exchange.is_version_skew()` is read AFTER
        // the challenge, off the SAME concrete exchange this call
        // constructed (never a trait object here), so it reflects
        // exactly what the terminal reply was.
        crate::challenge::ChallengeOutcome::Foreign if exchange.is_version_skew() => {
            Err(crate::Error::VersionSkew)
        }
        crate::challenge::ChallengeOutcome::Foreign => Err(err_state("supervisor lane challenge: foreign")),
        crate::challenge::ChallengeOutcome::Undetermined => Err(err_state("supervisor lane challenge: undetermined")),
    }
}

/// Encode `request`, write it, and read back exactly one reply — the one
/// request/reply round trip every supervisor-lane caller needs after its
/// own connect+challenge, factored out so `supervisor::request_for_test`
/// (test-only) and this module's own production `pub fn`s share one
/// implementation rather than two that could drift. Generic over
/// [`Client`] alone (not [`Endpoint`]): sending and reading a reply needs
/// no Endpoint-level operation, so any concrete client either platform's
/// `Endpoint::Client` names satisfies this directly.
pub(crate) fn send_and_read<C: Client>(
    conn: &C,
    request: &SupervisorRequest,
    deadline: Instant,
) -> crate::Result<SupervisorReply> {
    let bytes = wire::encode_supervisor_request(request).map_err(|e| err_state(format!("{e}")))?;
    conn.write_all(&bytes)?;
    match read_one_frame(conn, deadline)? {
        DecodedFrame::SupervisorReply(reply) => Ok(reply),
        other => Err(err_state(format!("expected a SupervisorReply, got {other:?}"))),
    }
}

/// `pub(crate)`: `supervisor.rs`'s own `end_run_over_mgmt_lane` (a
/// Windows mechanism function this lane does not touch — LU3c's job)
/// reads the mgmt-lane shutdown ack with this SAME primitive, so it
/// needs crate visibility, not merely module-private.
pub(crate) fn read_one_frame<C: Client>(conn: &C, deadline: Instant) -> crate::Result<DecodedFrame> {
    let result = crate::deadline::run_with_deadline(
        deadline,
        || conn.cancel(),
        move || -> crate::Result<DecodedFrame> {
            let mut splitter = wire::FrameSplitter::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = conn.read(&mut buf)?;
                if n == 0 {
                    return Err(err_state("connection closed before a reply arrived"));
                }
                let (frames, err) = splitter.feed(&buf[..n]);
                if let Some(e) = err {
                    return Err(err_state(format!("wire error waiting for a reply: {e}")));
                }
                if let Some(frame) = frames.into_iter().next() {
                    return Ok(frame);
                }
            }
        },
    );
    result.unwrap_or_else(|| Err(err_state("timed out waiting for a reply")))
}

/// The one "malformed/unexpected protocol shape" error shape every
/// caller in this module (and `supervisor.rs`, which imports this) uses
/// rather than minting a second one.
pub(crate) fn err_state(msg: impl Into<String>) -> crate::Error {
    crate::Error::State(msg.into())
}
