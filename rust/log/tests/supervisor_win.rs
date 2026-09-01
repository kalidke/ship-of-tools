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
//! boundary is correct.
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

/// As [`status`], but never panics — `Err`'s own text names what went
/// wrong. Used only where a connection MAY legitimately have stopped
/// answering (a diagnostic best-effort, or a poll loop that itself
/// decides what a failure means).
fn try_status(conn: &sot_log::pipe_win::PipeClient) -> Result<(Option<String>, Option<u64>, SupervisorPhase), String> {
    match request_for_test(conn, &SupervisorRequest::Status, Instant::now() + Duration::from_secs(5)) {
        Ok(SupervisorReply::StatusOk { voyage, leg, phase, .. }) => Ok((voyage, leg, phase)),
        Ok(other) => Err(format!("expected StatusOk, got {other:?}")),
        Err(e) => Err(format!("{e}")),
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
        // Generous: an EndRun's own reply is DEFERRED to record_closed
        // (B3) -- the mgmt-lane exchange plus the leg writing its own
        // marker, both real OS work on a background thread, not a bound
        // this crate itself pins tighter than "well within the ADR's own
        // per-op budgets stacked together".
        Instant::now() + Duration::from_secs(30),
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

/// Poll `query` past both pre-terminal milestones to whatever terminal
/// state follows. Callers that already PROVED `record_closed` via the
/// `end_run` command's own deferred reply (B3) use this only for the
/// remaining `record_closed -> record_verified`/`failed` step.
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

/// Submits `end_run` and asserts its OWN reply is `record_closed` (B3:
/// "the lane reply for an accepted EndRun is sent AT record_closed, not
/// at admission") — proving the FIRST half of the "record_closed then
/// record_verified" sequence directly, rather than silently accepting
/// either as `poll_to_terminal` alone would (Codex review round 2: "the
/// workflow's own description is unproved... never requires observing
/// record_closed").
fn end_run_and_expect_record_closed(conn: &sot_log::pipe_win::PipeClient, operation_id: &str, reason: &str, voyage: String) {
    let reply = command(conn, operation_id, SupervisorOp::EndRun { reason: reason.into(), voyage });
    assert_eq!(reply, SupervisorOperationState::RecordClosed, "end_run's own command reply must arrive AT record_closed (ADR 0041:592)");
}

/// Blocks on `conn.read` in a background thread so an EOF (or any other
/// outcome) can be awaited with a bounded timeout — `PipeClient` is
/// `Sync`/movable across threads by design (its own doc: a second thread
/// may `cancel` a blocking call in flight). Used to verify a connection
/// the supervisor is EXPECTED to close actually does.
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

fn wait_for_exit(mut child: Child, timeout: Duration) -> std::process::ExitStatus {
    poll_until(|| child.try_wait().unwrap(), timeout, "the supervisor process to exit")
}

/// As [`wait_for_exit`], but on timeout makes ONE best-effort attempt to
/// reconnect and report whatever `status` still claims, so a future CI
/// failure NAMES the stuck lifecycle state instead of just timing out
/// (per the coordinator's own round-4 addendum on the flap test).
fn wait_for_exit_with_diagnostics(mut child: Child, h: &str, timeout: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let diagnostic = connect_and_challenge_for_test(h).ok().and_then(|(conn, _)| try_status(&conn).ok());
            panic!(
                "timed out waiting for the supervisor process to exit; last reachable status \
                 (voyage, leg, phase): {diagnostic:?}"
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A real launcher-shaped client connects the supervisor lane of a real
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
    // — a second, explicit `hello` here would be a protocol violation.
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    assert_eq!(query(&conn, "unused-op-id"), SupervisorOperationState::UnknownOperation);

    let op_id = "test-end-run-1";
    end_run_and_expect_record_closed(&conn, op_id, "integration test", voyage.clone());

    let final_state = poll_to_terminal(&conn, op_id, Duration::from_secs(60));
    assert_eq!(final_state, SupervisorOperationState::RecordVerified);

    // Resubmitting the SAME operation id with the SAME digest is
    // idempotent -- it must answer with the current state, not
    // re-execute.
    let resubmit = command(&conn, op_id, SupervisorOp::EndRun { reason: "integration test".into(), voyage: voyage.clone() });
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

/// ADR 0041 start-mode table: "`--resume` | sealed, carrying its own
/// `run_end_requested` | do not spawn." The SECOND supervisor must NOT
/// exit immediately -- it serves ended-no-respawn (so a client polling
/// `query` for the operation that ended it, right after the
/// crash-restart, still finds a supervisor to ask) and only exits once
/// explicitly told to `stop`.
#[test]
fn resume_after_a_requested_end_serves_ended_no_respawn_then_exits_on_stop() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let child = spawn_supervisor(&state_dir, "--start", &["cmd.exe"]);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));
    end_run_and_expect_record_closed(&conn, "req-end", "test", voyage);
    let final_state = poll_to_terminal(&conn, "req-end", Duration::from_secs(60));
    assert_eq!(final_state, SupervisorOperationState::RecordVerified);
    assert_eq!(command(&conn, "req-stop", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let status1 = wait_for_exit(child, Duration::from_secs(30));
    assert_eq!(status1.code(), Some(sot_log::supervisor::EXIT_CLEAN));

    // A SECOND supervisor, `--resume`, against the SAME state-dir.
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
/// increments the ONE counter to its threshold. Explicitly PROVES Ready
/// was observed before relying on the shell's own timed self-exit (the
/// test's own name claims it); logs the observed phase at every poll so
/// a future CI failure names the stuck state instead of just timing out
/// (coordinator's round-4 addendum: the pre-rewrite version of this test
/// wedged past its own grace bound on real Windows CI twice).
#[test]
fn a_shell_that_dies_shortly_after_ready_trips_the_anti_flap_bound() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let child = spawn_supervisor(
        &state_dir,
        "--start",
        &["cmd.exe", "/d", "/c", "ping -n 2 127.0.0.1 >nul & exit 1"],
    );
    let guard = KillGuard(Some(child));

    // Ready observed: connect and poll status, logging every phase.
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let observed_ready = poll_until(
        || {
            let (_voyage, _leg, phase) = status(&conn);
            eprintln!("[flap test] observed phase: {phase:?}");
            (phase == SupervisorPhase::Ready).then_some(true)
        },
        Duration::from_secs(60),
        "the leg to reach Ready at least once before its own timed self-exit",
    );
    assert!(observed_ready);
    drop(conn); // the lane's own 5s idle eviction would close it anyway once flapping starts

    // Shell killed (by its own script) -> flap accounting -> Terminal ->
    // process exit within TERMINAL_EXIT_GRACE of reaching it. Diagnostic
    // on timeout: report whatever `status` still claims.
    let child = KillGuard(guard.0).0.unwrap();
    let status = wait_for_exit_with_diagnostics(child, &h, Duration::from_secs(120));
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
    end_run_and_expect_record_closed(&conn2, "cleanup-end", "test cleanup", voyage2);
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
    let (conn, outcome) = poll_until(
        || sot_log::supervisor::connect_and_challenge_with_build_for_test(&h, "some-other-build").ok(),
        Duration::from_secs(30),
        "the supervisor lane to accept a connection",
    );
    assert!(
        matches!(outcome, sot_log::challenge::ChallengeOutcome::Foreign),
        "a wrong build must be classified Foreign (refused{{version_skew}}), got {outcome:?}"
    );
    expect_connection_closes(conn, Duration::from_secs(5));
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

    assert_eq!(sot_log::supervisor::endrun(&state_dir, None, "no drawer yet".into()), sot_log::supervisor::EXIT_TERMINAL);

    assert_eq!(sot_log::supervisor::reset(&state_dir, None), sot_log::supervisor::EXIT_CLEAN);
    let minted = match sot_log::pointer::validate(&state_dir) {
        sot_log::pointer::PointerState::Valid(id) => id,
        other => panic!("expected a valid pointer after reset, got {other:?}"),
    };

    assert_eq!(sot_log::supervisor::endrun(&state_dir, Some(minted.clone()), "still nothing running".into()), sot_log::supervisor::EXIT_CLEAN);

    assert_eq!(sot_log::supervisor::reset(&state_dir, Some(minted.clone())), sot_log::supervisor::EXIT_CLEAN);
    match sot_log::pointer::validate(&state_dir) {
        sot_log::pointer::PointerState::Valid(id) => assert_ne!(id, minted, "reset must mint a NEW identity, never reuse the old one"),
        other => panic!("expected a valid pointer after the second reset, got {other:?}"),
    }
}

/// A SECOND `hello` on an already-challenged connection is a plain
/// protocol violation -- this connection closes, but the supervisor
/// PROCESS survives and keeps answering everyone else.
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

    end_run_and_expect_record_closed(&conn2, "cleanup-end", "cleanup", voyage2.unwrap());
    let _ = poll_to_terminal(&conn2, "cleanup-end", Duration::from_secs(60));
    assert_eq!(command(&conn2, "cleanup-stop", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let child = guard.0.take().unwrap();
    let _ = wait_for_exit(child, Duration::from_secs(30));
}

/// A crash mid `end_run` must be RECOVERABLE by a fresh supervisor -- the
/// ORIGINAL client's operation id must still answer through `query`
/// against the new process, never silently vanish because the process
/// that accepted it is gone. Proves the operation was genuinely
/// NONTERMINAL before the kill (polling `query` for `accepted`, never
/// assuming the 500ms pre-kill pause alone proves it).
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

    // Submit end_run on a SEPARATE thread: its own reply is deferred to
    // record_closed (B3), which may not arrive before this test kills
    // the supervisor -- block on it in the background rather than in
    // this thread, which needs to move on to killing the process.
    let conn_for_command = conn;
    let submit = std::thread::spawn(move || {
        command(&conn_for_command, "op-recover", SupervisorOp::EndRun { reason: "test".into(), voyage })
    });

    // Prove the operation is genuinely NONTERMINAL before killing --
    // poll a FRESH connection's `query` for `accepted` (durably
    // journaled, per ADR 0041's own "before the first irreversible act")
    // rather than assuming a fixed sleep landed before completion.
    let query_conn = wait_for_lane(&h, Duration::from_secs(10));
    poll_until(
        || matches!(query(&query_conn, "op-recover"), SupervisorOperationState::Accepted).then_some(()),
        Duration::from_secs(10),
        "the end_run operation to be admitted (accepted) before this test kills the supervisor",
    );

    // Kill the supervisor -- simulating a crash mid end_run. The LEG
    // process is not in the supervisor's job, and the shutdown request
    // may already have been delivered to it directly, so its own
    // teardown can proceed independently of whether the supervisor that
    // asked for it is still alive to see the result.
    let mut first = first_guard.0.take().unwrap();
    first.kill().unwrap();
    first.wait().unwrap();
    // The background `command` thread's own connection just died with
    // the process -- let it finish (its own `Err` is fine and ignored;
    // this test only cares about the SECOND supervisor's recovery).
    let _ = submit.join();

    // A SECOND supervisor, `--resume`: recovery reconciles the in-flight
    // end_run via the leg's own durable marker, and this operation id
    // must still answer.
    let second = spawn_supervisor(&state_dir, "--resume", &["cmd.exe"]);
    let mut second_guard = KillGuard(Some(second));
    let conn2 = wait_for_lane(&h, Duration::from_secs(30));

    let final_state = poll_to_terminal(&conn2, "op-recover", Duration::from_secs(60));
    assert_eq!(final_state, SupervisorOperationState::RecordVerified);

    // Recovering an end_run for the CURRENT voyage means no leg is ever
    // spawned -- eventually straight to ended-no-respawn (not necessarily
    // immediately: the lane is phase-total, so a status mid-Starting is a
    // legitimate observation while recovery is still resolving via the
    // ordinary adopt-probe/start-mode path).
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
/// admissible afterward.
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

    end_run_and_expect_record_closed(&conn, "stale-1", "test", voyage);
    let _ = poll_to_terminal(&conn, "stale-1", Duration::from_secs(60));
    assert_eq!(command(&conn, "stale-1-stop", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let child = guard.0.take().unwrap();
    let _ = wait_for_exit(child, Duration::from_secs(30));
}

/// ADR 0041 (Codex review round 2, B2): `reset` is admissible ONLY from
/// `EndedNoRespawn` -- refused while a leg is live, through the generic
/// `Failed{detail}` shape (there is no dedicated wire refusal reason for
/// it); `Reset{voyage: None}` while a live voyage exists is refused as
/// `stale_voyage`. Neither mutates the pointer.
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

    match sot_log::pointer::validate(&state_dir) {
        sot_log::pointer::PointerState::Valid(id) => assert_eq!(id, voyage, "the pointer must be unchanged after both refusals"),
        other => panic!("expected the pointer to still be valid and unchanged, got {other:?}"),
    }

    end_run_and_expect_record_closed(&conn, "cleanup-end", "cleanup", voyage);
    let _ = poll_to_terminal(&conn, "cleanup-end", Duration::from_secs(60));
    assert_eq!(command(&conn, "cleanup-stop", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let child = guard.0.take().unwrap();
    let _ = wait_for_exit(child, Duration::from_secs(30));
}

/// ADR 0041 (Codex review round 2, B2): once `EndedNoRespawn`, `reset`
/// IS admissible and produces a genuinely NEW voyage the authority then
/// spawns a fresh leg for.
#[test]
fn reset_from_ended_no_respawn_mints_a_new_voyage_and_spawns_for_it() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let child = spawn_supervisor(&state_dir, "--start", &["cmd.exe"]);
    let mut guard = KillGuard(Some(child));
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (old_voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    end_run_and_expect_record_closed(&conn, "end-before-reset", "test", old_voyage.clone());
    let _ = poll_to_terminal(&conn, "end-before-reset", Duration::from_secs(60));
    poll_until(
        || matches!(status(&conn), (_, _, SupervisorPhase::EndedNoRespawn)).then_some(()),
        Duration::from_secs(30),
        "ended-no-respawn before submitting reset",
    );

    let reset_reply = command(&conn, "do-reset", SupervisorOp::Reset { voyage: Some(old_voyage.clone()) });
    assert_eq!(reset_reply, SupervisorOperationState::Accepted);
    let reset_final = poll_to_terminal(&conn, "do-reset", Duration::from_secs(30));
    match reset_final {
        SupervisorOperationState::ResetDone { new_voyage } => assert_ne!(new_voyage, old_voyage),
        other => panic!("expected ResetDone, got {other:?}"),
    }

    // The authority spawns a fresh leg for the NEW voyage.
    let (new_voyage, _leg2) = wait_for_ready(&conn, Duration::from_secs(90));
    assert_ne!(new_voyage, old_voyage);

    end_run_and_expect_record_closed(&conn, "cleanup-end", "cleanup", new_voyage);
    let _ = poll_to_terminal(&conn, "cleanup-end", Duration::from_secs(60));
    assert_eq!(command(&conn, "cleanup-stop", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let child = guard.0.take().unwrap();
    let _ = wait_for_exit(child, Duration::from_secs(30));
}
