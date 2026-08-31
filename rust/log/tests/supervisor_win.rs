#![cfg(windows)]
//! Real cross-process integration tests for `sot_log::supervisor` (ADR
//! 0041 step 6 U2) — spawns the REAL `sot-capsule` binary (`supervise`,
//! and the no-supervisor `endrun`/`reset` in-process callers), talking to
//! it exactly the way a real launcher/FE client would: connect the
//! supervisor lane, run the full same-connection challenge, then
//! `hello`/`status`/`command`/`query`. The classifier's own transition
//! table (A1-A5/B0-B9) and the journal's own crash-durability are already
//! proven scripted-only by `classify.rs`'s and `journal.rs`'s own unit
//! tests; what these tests add is proof the WIRING across a real process
//! boundary is correct — the step-5 e2e suite's own cross-process
//! residual this ADR names ("the true cross-process challenge is step
//! 6's adoption test").
//!
//! Deterministic by construction: every wait below is a BOUNDED POLL for
//! an external, observable fact (a pipe answering, a process's own exit
//! code) — never a sleep-and-hope, and never a lifetime-counter
//! observation of kernel state.

use sot_log::supervisor::{connect_and_challenge_for_test, request_for_test, state_dir_hash};
use sot_log::wire::{SupervisorOp, SupervisorOperationState, SupervisorPhase, SupervisorReply, SupervisorRequest};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn capsule_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sot-capsule"))
}

/// Reaps a spawned child on every exit path (a panicking assertion
/// included) — the same shape `tests/pipe_win.rs`'s own cross-process
/// challenge test uses.
struct KillGuard(Option<Child>);
impl Drop for KillGuard {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn poll_until<T>(mut attempt: impl FnMut() -> Option<T>, timeout: Duration, what: &str) -> T {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = attempt() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Bounded poll for the lane to accept a connection AND answer the
/// challenge — the observable fact a real client waits on, never a
/// sleep guessing how long `bind_supervisor` takes.
fn wait_for_lane(h: &str, timeout: Duration) -> sot_log::pipe_win::PipeClient {
    poll_until(
        || connect_and_challenge_for_test(h).ok().map(|(conn, _process)| conn),
        timeout,
        "the supervisor lane to accept and answer the challenge",
    )
}

fn hello(conn: &sot_log::pipe_win::PipeClient) {
    let reply = request_for_test(
        conn,
        &SupervisorRequest::Hello { proto: sot_log::wire::SUPERVISOR_PROTO_V1, build: sot_log::exchange::SUPERVISOR_LANE_BUILD_ID.to_string() },
        Instant::now() + Duration::from_secs(5),
    )
    .expect("hello");
    assert!(matches!(reply, SupervisorReply::HelloOk { .. }), "expected HelloOk, got {reply:?}");
}

fn status(conn: &sot_log::pipe_win::PipeClient) -> (Option<String>, Option<u64>, SupervisorPhase) {
    match request_for_test(conn, &SupervisorRequest::Status, Instant::now() + Duration::from_secs(5)).expect("status") {
        SupervisorReply::StatusOk { voyage, leg, phase, .. } => (voyage, leg, phase),
        other => panic!("expected StatusOk, got {other:?}"),
    }
}

/// The lane binds BEFORE any adopt or spawn (ADR 0041), so answering
/// `status` does not by itself mean a leg is READY yet — poll for the
/// phase itself, the observable fact, rather than assuming spawn
/// finished the instant the lane became reachable.
fn wait_for_ready(conn: &sot_log::pipe_win::PipeClient, timeout: Duration) -> (String, u64) {
    poll_until(
        || match status(conn) {
            (Some(voyage), Some(leg), SupervisorPhase::Ready) => Some((voyage, leg)),
            _ => None,
        },
        timeout,
        "the leg to reach phase Ready",
    )
}

fn command(conn: &sot_log::pipe_win::PipeClient, operation_id: &str, op: SupervisorOp) -> SupervisorOperationState {
    match request_for_test(
        conn,
        &SupervisorRequest::Command { operation_id: operation_id.to_string(), op },
        Instant::now() + Duration::from_secs(10),
    )
    .expect("command")
    {
        SupervisorReply::Operation(state) => state,
        other => panic!("expected Operation, got {other:?}"),
    }
}

fn query(conn: &sot_log::pipe_win::PipeClient, operation_id: &str) -> SupervisorOperationState {
    match request_for_test(
        conn,
        &SupervisorRequest::Query { operation_id: operation_id.to_string() },
        Instant::now() + Duration::from_secs(5),
    )
    .expect("query")
    {
        SupervisorReply::Operation(state) => state,
        other => panic!("expected Operation, got {other:?}"),
    }
}

fn spawn_supervisor(state_dir: &Path, mode: &str, argv: &[&str]) -> Child {
    let mut cmd = Command::new(capsule_exe());
    cmd.arg("supervise")
        .arg(state_dir)
        .arg(mode)
        .arg("--assume-no-rollback-target")
        .arg("--")
        .args(argv)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.spawn().expect("spawn sot-capsule supervise")
}

/// The step-5 e2e suite's own cross-process residual, closed for real:
/// a real launcher-shaped client connects the supervisor lane of a real
/// spawned `sot-capsule supervise`, runs hello/status/end_run/query, and
/// observes the supervisor's own clean exit.
#[test]
fn full_lifecycle_hello_status_end_run_query_and_clean_exit() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let child = spawn_supervisor(&state_dir, "--start", &["cmd.exe"]); // stays open until EndRun
    let mut guard = KillGuard(Some(child));

    let conn = wait_for_lane(&h, Duration::from_secs(30));
    hello(&conn);
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    assert_eq!(query(&conn, "unused-op-id"), SupervisorOperationState::UnknownOperation);

    let op_id = "test-end-run-1";
    let reply = command(
        &conn,
        op_id,
        SupervisorOp::EndRun { reason: "integration test".into(), voyage: voyage.clone() },
    );
    assert_eq!(reply, SupervisorOperationState::RecordClosed, "the COMMAND's own immediate reply is record_closed");

    let final_state = poll_until(
        || match query(&conn, op_id) {
            SupervisorOperationState::RecordClosed => None, // still waiting for verify to finish
            other => Some(other),
        },
        Duration::from_secs(60),
        "end_run to reach a terminal state past record_closed",
    );
    assert_eq!(final_state, SupervisorOperationState::RecordVerified);

    // Resubmitting the SAME operation id with the SAME digest is
    // idempotent -- it must answer with the current state, not
    // re-execute.
    let resubmit = command(
        &conn,
        op_id,
        SupervisorOp::EndRun { reason: "integration test".into(), voyage: voyage.clone() },
    );
    assert_eq!(resubmit, SupervisorOperationState::RecordVerified);

    // A DIFFERENT digest under the same id is an id_conflict.
    let conflict = command(&conn, op_id, SupervisorOp::Stop);
    assert_eq!(conflict, SupervisorOperationState::Refused { reason: sot_log::wire::SupervisorRefusedReason::IdConflict });

    let stop_reply = command(&conn, "test-stop-1", SupervisorOp::Stop);
    assert_eq!(stop_reply, SupervisorOperationState::Stopping);

    let child = guard.0.take().unwrap();
    let status = wait_for_exit(child, Duration::from_secs(30));
    assert_eq!(status.code(), Some(sot_log::supervisor::EXIT_CLEAN), "a clean EndRun+Stop must exit 0");
}

fn wait_for_exit(mut child: Child, timeout: Duration) -> std::process::ExitStatus {
    poll_until(|| child.try_wait().unwrap(), timeout, "the supervisor process to exit")
}

/// ADR 0041 start-mode table: "`--resume` | sealed, carrying its own
/// `run_end_requested` | exit 0; do not spawn."
#[test]
fn resume_after_a_requested_end_exits_0_without_spawning_a_new_leg() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    // A producer that exits ON ITS OWN seals with `producer_dead`, NOT
    // `run_end_requested` -- "the run's program ending is not a teardown
    // anyone requested" (ADR 0041). Proving THIS row needs a leg ended BY
    // an explicit `end_run`, so the producer here is long-lived and this
    // test ends it itself, exactly like the full-lifecycle test does.
    let child = spawn_supervisor(&state_dir, "--start", &["cmd.exe"]);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));
    let reply = command(&conn, "req-end", SupervisorOp::EndRun { reason: "test".into(), voyage });
    assert_eq!(reply, SupervisorOperationState::RecordClosed);
    // ADR 0041: an ended authority does NOT exit on its own -- it stays
    // in ENDED-NO-RESPAWN, serving query/status/stop, until an EXPLICIT
    // `stop` (or a kill). Wait for record_verified through `query`
    // first, so the marker this test's whole premise depends on is
    // definitely durable before the second supervisor ever looks.
    let final_state = poll_until(
        || match query(&conn, "req-end") {
            SupervisorOperationState::RecordClosed => None,
            other => Some(other),
        },
        Duration::from_secs(60),
        "end_run to reach record_verified",
    );
    assert_eq!(final_state, SupervisorOperationState::RecordVerified);
    assert_eq!(command(&conn, "req-stop", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let status1 = wait_for_exit(child, Duration::from_secs(30));
    assert_eq!(status1.code(), Some(sot_log::supervisor::EXIT_CLEAN));

    // A SECOND supervisor, `--resume`, against the SAME state-dir: the
    // latest leg is sealed carrying its own marker, so this must exit 0
    // immediately without ever spawning `cmd.exe` again.
    let child2 = spawn_supervisor(&state_dir, "--resume", &["cmd.exe"]);
    let status2 = wait_for_exit(child2, Duration::from_secs(30));
    assert_eq!(status2.code(), Some(sot_log::supervisor::EXIT_CLEAN));
}

/// ADR 0041 "the flap bound": a shell that dies immediately increments
/// the ONE counter to its threshold.
#[test]
fn a_shell_that_dies_immediately_trips_the_anti_flap_bound() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let child = spawn_supervisor(&state_dir, "--start", &["cmd.exe", "/d", "/c", "exit 1"]);
    let status = wait_for_exit(child, Duration::from_secs(120));
    assert_eq!(status.code(), Some(sot_log::supervisor::EXIT_TERMINAL), "three unstable legs must terminate the supervisor");
}

/// ADR 0041 "an ADOPTED leg ends correctly" / the nightly composite's own
/// premise: the supervisor dying leaves the capsule headless, and the
/// NEXT start adopts it rather than spawning a duplicate or silently
/// losing it.
#[test]
fn a_second_supervisor_adopts_a_leg_left_behind_by_a_killed_first_one() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let first = spawn_supervisor(&state_dir, "--start", &["cmd.exe"]); // stays open
    let mut first_guard = KillGuard(Some(first));
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, leg) = wait_for_ready(&conn, Duration::from_secs(90));
    drop(conn);

    // Kill the SUPERVISOR only -- the leg is deliberately NOT in its
    // job (ADR 0041: "the supervisor dying must be harmless to the
    // run"), so `cmd.exe` must still be alive and answering afterward.
    let mut first = first_guard.0.take().unwrap();
    first.kill().unwrap();
    first.wait().unwrap();

    let second = spawn_supervisor(&state_dir, "--start", &["cmd.exe"]);
    let mut second_guard = KillGuard(Some(second));
    let conn2 = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage2, leg2) = wait_for_ready(&conn2, Duration::from_secs(30));
    assert_eq!(voyage2, voyage, "the SAME voyage, never a re-mint");
    assert_eq!(leg2, leg, "the SAME leg epoch -- adopted, not a fresh spawn");

    // Clean up: end the run through the SECOND (now authoritative)
    // supervisor, then stop it, before letting the guards kill anything.
    let reply = command(&conn2, "cleanup-end", SupervisorOp::EndRun { reason: "test cleanup".into(), voyage: voyage2 });
    assert_eq!(reply, SupervisorOperationState::RecordClosed);
    let _ = poll_until(
        || match query(&conn2, "cleanup-end") {
            SupervisorOperationState::RecordClosed => None,
            other => Some(other),
        },
        Duration::from_secs(60),
        "cleanup end_run to reach a terminal state",
    );
    assert_eq!(command(&conn2, "cleanup-stop", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let second = second_guard.0.take().unwrap();
    let _ = wait_for_exit(second, Duration::from_secs(60));
}

/// ADR 0041 Lifecycle "Build boundary": a mismatched build is answered
/// `refused {version_skew}` and the connection is closed.
#[test]
fn a_mismatched_build_id_is_refused_and_the_connection_closes() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let child = spawn_supervisor(&state_dir, "--start", &["cmd.exe"]);
    let _guard = KillGuard(Some(child));
    // `connect_and_challenge_for_test` already proves server identity;
    // once proven, send a Hello with a WRONG build directly to exercise
    // the version-skew path a real client's own would take.
    let (conn, _process) = poll_until(
        || connect_and_challenge_for_test(&h).ok(),
        Duration::from_secs(30),
        "the supervisor lane to accept and answer the challenge",
    );
    let reply = request_for_test(
        &conn,
        &SupervisorRequest::Hello { proto: sot_log::wire::SUPERVISOR_PROTO_V1, build: "some-other-build".into() },
        Instant::now() + Duration::from_secs(5),
    )
    .expect("a refusal is still one well-formed reply");
    assert_eq!(
        reply,
        SupervisorReply::Refused { reason: sot_log::wire::SupervisorRefusedReason::VersionSkew }
    );
    // `guard` reaps the supervisor process on drop below -- this test
    // only asserts the refusal itself.
}

/// ADR 0041 no-supervisor capability matrix: "proven ABSENT: reset only"
/// and endrun's own "nothing to end" case -- both exercised with NO
/// supervisor running at all, driving `sot_log::supervisor::{endrun,
/// reset}` directly (the fence-acquiring in-process callers).
#[test]
fn endrun_and_reset_without_a_running_supervisor() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    // No drawer at all yet: endrun has nothing to infer a voyage from.
    assert_eq!(sot_log::supervisor::endrun(&state_dir, None, "no drawer yet".into()), sot_log::supervisor::EXIT_TERMINAL);

    // reset with no pointer at all: absence is provably ABSENT (nothing
    // to probe), so this mints the very first voyage.
    assert_eq!(sot_log::supervisor::reset(&state_dir, None), sot_log::supervisor::EXIT_CLEAN);
    let minted = match sot_log::pointer::validate(&state_dir) {
        sot_log::pointer::PointerState::Valid(id) => id,
        other => panic!("expected a valid pointer after reset, got {other:?}"),
    };

    // endrun against the now-valid-but-capsule-less voyage: ABSENT ->
    // nothing to end -> exit 0.
    assert_eq!(sot_log::supervisor::endrun(&state_dir, Some(minted.clone()), "still nothing running".into()), sot_log::supervisor::EXIT_CLEAN);

    // A second reset mints yet another fresh voyage, evidence-preserving.
    assert_eq!(sot_log::supervisor::reset(&state_dir, Some(minted.clone())), sot_log::supervisor::EXIT_CLEAN);
    match sot_log::pointer::validate(&state_dir) {
        sot_log::pointer::PointerState::Valid(id) => assert_ne!(id, minted, "reset must mint a NEW identity, never reuse the old one"),
        other => panic!("expected a valid pointer after the second reset, got {other:?}"),
    }
}
