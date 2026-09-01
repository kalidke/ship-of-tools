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

/// Poll `query` past both pre-terminal milestones (`accepted`, the
/// initial durable-but-not-yet-acted-on state, and `record_closed`, the
/// end_run-specific "confirmed exited, not yet verified" milestone) to
/// whatever terminal state follows. The command's OWN immediate reply is
/// `accepted` (ADR 0041 Lifecycle: "typically `Accepted`, once the
/// operation is durably journaled" — never a value obtained by blocking
/// this call on the operation's own OS-facing work), so every caller that
/// used to assert on the immediate reply directly now polls through this
/// instead.
fn poll_to_terminal(conn: &sot_log::pipe_win::PipeClient, operation_id: &str, timeout: Duration) -> SupervisorOperationState {
    poll_until(
        || match query(conn, operation_id) {
            SupervisorOperationState::Accepted | SupervisorOperationState::RecordClosed => None,
            other => Some(other),
        },
        timeout,
        "the operation to reach a terminal state",
    )
}

/// Blocks on `conn.read` in a background thread so an EOF (or any other
/// outcome) can be awaited with a bounded timeout — `PipeClient` is
/// `Sync`/movable across threads by design (its own doc: a second thread
/// may `cancel` a blocking call in flight). Used to verify a connection
/// the supervisor is EXPECTED to close actually does (Codex review round
/// 1, test honesty: the mismatched-build test used to assert only the
/// classification, never the close its own name claims).
fn expect_connection_closes(conn: sot_log::pipe_win::PipeClient, timeout: Duration) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        let _ = tx.send(conn.read(&mut buf));
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(0)) => {} // ordered EOF -- the connection closed, exactly as claimed
        Ok(other) => panic!("expected the connection to close (EOF), got {other:?}"),
        Err(_) => panic!("the connection never closed within {timeout:?}"),
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

    // `wait_for_lane` already ran the FULL same-connection challenge,
    // whose own `hello`/`hello_ok` round trip IS this connection's hello
    // — a second, explicit `hello` here would be a protocol violation
    // (hello_ok is already latched) and the lane would correctly close
    // the connection on it, not answer it.
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    assert_eq!(query(&conn, "unused-op-id"), SupervisorOperationState::UnknownOperation);

    let op_id = "test-end-run-1";
    let reply = command(
        &conn,
        op_id,
        SupervisorOp::EndRun { reason: "integration test".into(), voyage: voyage.clone() },
    );
    // ADR 0041 Lifecycle: "typically `Accepted`, once the operation is
    // durably journaled" -- the command's own reply never blocks on the
    // mgmt-lane exchange, the wait for the leg's exit, or verification,
    // all of which now run on a background thread (Codex review round 1,
    // finding 1).
    assert_eq!(reply, SupervisorOperationState::Accepted, "the COMMAND's own immediate reply is accepted");

    let final_state = poll_to_terminal(&conn, op_id, Duration::from_secs(60));
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
/// `run_end_requested` | do not spawn." Codex review round 1, finding 3:
/// the SECOND supervisor must NOT exit immediately -- it serves
/// ended-no-respawn (so a client polling `query` for the operation that
/// ended it, right after the crash-restart, still finds a supervisor to
/// ask) and only exits once explicitly told to `stop`.
#[test]
fn resume_after_a_requested_end_serves_ended_no_respawn_then_exits_on_stop() {
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
    assert_eq!(reply, SupervisorOperationState::Accepted);
    // Wait for record_verified through `query` first, so the marker this
    // test's whole premise depends on is definitely durable before the
    // second supervisor ever looks.
    let final_state = poll_to_terminal(&conn, "req-end", Duration::from_secs(60));
    assert_eq!(final_state, SupervisorOperationState::RecordVerified);
    assert_eq!(command(&conn, "req-stop", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let status1 = wait_for_exit(child, Duration::from_secs(30));
    assert_eq!(status1.code(), Some(sot_log::supervisor::EXIT_CLEAN));

    // A SECOND supervisor, `--resume`, against the SAME state-dir: the
    // latest leg is sealed carrying its own marker, so it recovers
    // straight into ended-no-respawn -- never spawns `cmd.exe` again, but
    // also never exits on its own. Prove BOTH: the lane answers (it is
    // actually SERVING, not merely still starting up) and reports the
    // right phase/voyage, and the client's own OLD operation id is still
    // answerable from this fresh process.
    let child2 = spawn_supervisor(&state_dir, "--resume", &["cmd.exe"]);
    let mut guard2 = KillGuard(Some(child2));
    let conn2 = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage2, leg2, phase2) = poll_until(
        || {
            let (voyage, leg, phase) = status(&conn2);
            (phase == SupervisorPhase::EndedNoRespawn).then_some((voyage, leg, phase))
        },
        Duration::from_secs(30),
        "the resumed supervisor to report ended-no-respawn",
    );
    assert_eq!(phase2, SupervisorPhase::EndedNoRespawn);
    assert!(leg2.is_none(), "no leg is running once ended-no-respawn");
    assert!(voyage2.is_some(), "the voyage id survives recovery");
    assert_eq!(
        query(&conn2, "req-end"),
        SupervisorOperationState::RecordVerified,
        "a query for the ORIGINAL operation id, against a brand-new process, still answers"
    );

    assert_eq!(command(&conn2, "req-stop-2", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let child2 = guard2.0.take().unwrap();
    let status2 = wait_for_exit(child2, Duration::from_secs(30));
    assert_eq!(status2.code(), Some(sot_log::supervisor::EXIT_CLEAN));
}

/// ADR 0041 "the flap bound": a leg that reaches READY and then dies
/// increments the ONE counter to its threshold. Codex review round 1,
/// test honesty: an earlier version's shell died SO fast it usually never
/// answered a single challenge, exercising only the pre-READY (A2
/// `LegEnded`) counting path rather than the READY-then-died path the
/// anti-flap bound actually exists to catch (`ready_at.elapsed() <
/// STABILITY_INTERVAL`) -- a short delay before exiting gives the
/// capsule's own mgmt pipe a real chance to answer at least one challenge
/// first.
#[test]
fn a_shell_that_dies_shortly_after_ready_trips_the_anti_flap_bound() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let child = spawn_supervisor(
        &state_dir,
        "--start",
        &["cmd.exe", "/d", "/c", "ping -n 2 127.0.0.1 >nul & exit 1"],
    );
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
    assert_eq!(reply, SupervisorOperationState::Accepted);
    let _ = poll_to_terminal(&conn2, "cleanup-end", Duration::from_secs(60));
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
    // The challenge's OWN `hello` IS the connection's one first frame —
    // drive it directly with a WRONG build (never send a second, separate
    // `hello` after a correct-build challenge already latched `hello_ok`;
    // the lane closes on that as a protocol violation, not a version-skew
    // refusal — see `full_lifecycle_...`'s own comment on this exact
    // point).
    let (conn, outcome) = poll_until(
        || sot_log::supervisor::connect_and_challenge_with_build_for_test(&h, "some-other-build").ok(),
        Duration::from_secs(30),
        "the supervisor lane to accept a connection",
    );
    assert!(
        matches!(outcome, sot_log::challenge::ChallengeOutcome::Foreign),
        "a wrong build must be classified Foreign (refused{{version_skew}}), got {outcome:?}"
    );
    // Codex review round 1, test honesty: this test's own name claims the
    // connection closes -- verify it actually does, not merely that the
    // classification came back Foreign.
    expect_connection_closes(conn, Duration::from_secs(5));
    // `_guard` reaps the supervisor process on drop below.
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

/// Codex review round 1 fix 5 (commit 3d031ebb): a SECOND `hello` on an
/// already-challenged connection used to crash the WHOLE authority
/// (`unreachable!()`). It must be a plain protocol violation instead --
/// this connection closes, but the supervisor PROCESS survives and keeps
/// answering everyone else.
#[test]
fn a_second_hello_closes_the_connection_but_the_authority_survives() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let child = spawn_supervisor(&state_dir, "--start", &["cmd.exe"]);
    let mut guard = KillGuard(Some(child));
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    wait_for_ready(&conn, Duration::from_secs(90));

    let second_hello = sot_log::wire::encode_supervisor_request(&SupervisorRequest::Hello {
        proto: sot_log::wire::SUPERVISOR_PROTO_V1,
        build: sot_log::exchange::SUPERVISOR_LANE_BUILD_ID.to_string(),
    })
    .unwrap();
    conn.write_all(&second_hello).unwrap();
    expect_connection_closes(conn, Duration::from_secs(5));

    // The authority itself must have survived: a FRESH connection still
    // gets a normal, correct answer.
    let conn2 = wait_for_lane(&h, Duration::from_secs(10));
    let (voyage2, _leg2, phase2) = status(&conn2);
    assert_eq!(phase2, SupervisorPhase::Ready, "the authority must still be alive and serving after the protocol violation");

    let reply = command(&conn2, "cleanup-end", SupervisorOp::EndRun { reason: "cleanup".into(), voyage: voyage2.unwrap() });
    assert_eq!(reply, SupervisorOperationState::Accepted);
    let _ = poll_to_terminal(&conn2, "cleanup-end", Duration::from_secs(60));
    assert_eq!(command(&conn2, "cleanup-stop", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let child = guard.0.take().unwrap();
    let _ = wait_for_exit(child, Duration::from_secs(30));
}

/// Codex review round 1, finding 3: a crash mid `end_run` must be
/// RECOVERABLE by a fresh supervisor -- the ORIGINAL client's operation
/// id must still answer through `query` against the new process, never
/// silently vanish because the process that accepted it is gone.
#[test]
fn a_crashed_supervisor_s_end_run_is_recovered_and_queryable_by_a_fresh_one() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let first = spawn_supervisor(&state_dir, "--start", &["cmd.exe"]);
    let mut first_guard = KillGuard(Some(first));
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    let reply = command(&conn, "op-recover", SupervisorOp::EndRun { reason: "test".into(), voyage });
    assert_eq!(reply, SupervisorOperationState::Accepted);

    // A bounded real-world pause before killing: `end_run`'s own
    // background thread (mgmt-lane connect/challenge/write) needs a
    // moment of real OS scheduling to actually dispatch the shutdown
    // request before this test yanks the process out from under it --
    // this is deliberately landing the simulated crash PAST that point,
    // not an ADR-observable production wait.
    std::thread::sleep(Duration::from_millis(500));

    // Kill the supervisor -- simulating a crash mid end_run. The LEG
    // process is not in the supervisor's job (ADR 0041: "the supervisor
    // dying must be harmless to the run"), and the shutdown request was
    // already delivered to IT directly, so its own teardown proceeds
    // independently of whether the supervisor that asked for it is still
    // alive to see the result.
    let mut first = first_guard.0.take().unwrap();
    first.kill().unwrap();
    first.wait().unwrap();

    // A SECOND supervisor, `--resume`: recovery reconciles the in-flight
    // end_run via the leg's own durable marker (never assuming success,
    // nor fabricating failure, just because the process that accepted it
    // is gone), and this operation id must still answer.
    let second = spawn_supervisor(&state_dir, "--resume", &["cmd.exe"]);
    let mut second_guard = KillGuard(Some(second));
    let conn2 = wait_for_lane(&h, Duration::from_secs(30));

    let final_state = poll_to_terminal(&conn2, "op-recover", Duration::from_secs(60));
    assert_eq!(final_state, SupervisorOperationState::RecordVerified);

    // Recovering an end_run for the CURRENT voyage means no leg is ever
    // spawned -- eventually straight to ended-no-respawn. NOT necessarily
    // immediately: the lane is phase-total now (Codex review round 2), so
    // a `status` mid-`Starting` is a legitimate observation, not a bug --
    // this test's own 500ms pre-kill sleep does not pin whether the FIRST
    // supervisor's end_run had already reached its OWN terminal record
    // before the kill landed. Either way it resolves to ended-no-respawn:
    // if the first supervisor finished first, THIS supervisor never finds
    // anything active to reconcile and instead reaches it via the ordinary
    // start-mode path (adopt-only probe -> Absent -> should_spawn_after_
    // absent sees the sealed, marked leg); if it was truly interrupted,
    // reconcile_journal_on_startup reaches it directly at startup. Poll
    // for the observable fact instead of asserting the immediate value.
    let (_voyage2, leg2, phase2) = poll_until(
        || {
            let (voyage, leg, phase) = status(&conn2);
            (phase == SupervisorPhase::EndedNoRespawn).then_some((voyage, leg, phase))
        },
        Duration::from_secs(90),
        "the resumed supervisor to reach ended-no-respawn",
    );
    assert_eq!(phase2, SupervisorPhase::EndedNoRespawn);
    assert!(leg2.is_none(), "no leg is running once ended-no-respawn");

    assert_eq!(command(&conn2, "op-recover-stop", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let second = second_guard.0.take().unwrap();
    let _ = wait_for_exit(second, Duration::from_secs(30));
}

/// ADR 0041 voyage-fencing: a mismatch is refused `stale_voyage` with NO
/// mutation, checked before the journal is ever touched -- so the SAME
/// operation id, resubmitted with the CORRECT voyage, is still
/// admissible afterward (never blocked as a spurious id_conflict from
/// the refused attempt).
#[test]
fn a_command_naming_the_wrong_voyage_is_refused_stale_voyage() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let child = spawn_supervisor(&state_dir, "--start", &["cmd.exe"]);
    let mut guard = KillGuard(Some(child));
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    let wrong_voyage = "00000000-0000-0000-0000-000000000000".to_string();
    assert_ne!(wrong_voyage, voyage);
    let reply = command(&conn, "stale-1", SupervisorOp::EndRun { reason: "test".into(), voyage: wrong_voyage });
    assert_eq!(reply, SupervisorOperationState::Refused { reason: sot_log::wire::SupervisorRefusedReason::StaleVoyage });

    let reply2 = command(&conn, "stale-1", SupervisorOp::EndRun { reason: "test".into(), voyage });
    assert_eq!(reply2, SupervisorOperationState::Accepted, "the SAME id with the CORRECT voyage must still be admissible");
    let _ = poll_to_terminal(&conn, "stale-1", Duration::from_secs(60));
    assert_eq!(command(&conn, "stale-1-stop", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let child = guard.0.take().unwrap();
    let _ = wait_for_exit(child, Duration::from_secs(30));
}

/// ADR 0041: `reset` while a leg is live is refused through the generic
/// `Failed{detail}` shape (there is no dedicated wire refusal reason for
/// it); `Reset{voyage: None}` while a live voyage exists is refused as
/// `stale_voyage` (Codex review round 1, finding 4: `None` is legal ONLY
/// when there is truly no live voyage to fence against). Neither
/// mutates the pointer.
#[test]
fn reset_is_refused_while_a_leg_is_live() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let child = spawn_supervisor(&state_dir, "--start", &["cmd.exe"]);
    let mut guard = KillGuard(Some(child));
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    let reply = command(&conn, "reset-while-live", SupervisorOp::Reset { voyage: Some(voyage.clone()) });
    match reply {
        SupervisorOperationState::Failed { detail } => {
            assert!(detail.to_lowercase().contains("live"), "expected a live-leg refusal, got {detail:?}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }

    let reply2 = command(&conn, "reset-none-while-live", SupervisorOp::Reset { voyage: None });
    assert_eq!(reply2, SupervisorOperationState::Refused { reason: sot_log::wire::SupervisorRefusedReason::StaleVoyage });

    // Neither refusal mutated the pointer.
    match sot_log::pointer::validate(&state_dir) {
        sot_log::pointer::PointerState::Valid(id) => assert_eq!(id, voyage, "the pointer must be unchanged after both refusals"),
        other => panic!("expected the pointer to still be valid and unchanged, got {other:?}"),
    }

    let reply3 = command(&conn, "cleanup-end", SupervisorOp::EndRun { reason: "cleanup".into(), voyage });
    assert_eq!(reply3, SupervisorOperationState::Accepted);
    let _ = poll_to_terminal(&conn, "cleanup-end", Duration::from_secs(60));
    assert_eq!(command(&conn, "cleanup-stop", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let child = guard.0.take().unwrap();
    let _ = wait_for_exit(child, Duration::from_secs(30));
}
