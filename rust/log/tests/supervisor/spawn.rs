//! Supervisor start tests: start readiness, the identity it reports, first_leg_only, client survival, cancel, exit codes.

use super::*;

/// ADR 0043 decision 27/30: with the InitialProbe's connect no longer
/// paying the full [`sot_log::lane::transport::CONNECT_BOUND`] on an absent
/// voyage pipe (it fails fast on `ENOENT`/`ECONNREFUSED` now, retried
/// only at [`sot_log::supervisor`]'s own 250ms `ATTEMPT_INTERVAL`), a
/// fresh `--start` should reach `Ready` in well under a second rather
/// than the 2+ seconds the old fixed wait alone used to cost. Asserted
/// against a GENEROUS 30s bound — this proves behaviour (Ready is
/// reached at all, promptly), not a tight perf gate; the actual measured
/// time is printed for the report.
#[test]
fn start_reaches_ready_promptly() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let started = Instant::now();
    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (_voyage, run) = wait_for_ready(&conn, Duration::from_secs(30));
    let elapsed = started.elapsed();
    println!("LU6b start_reaches_ready_promptly: spawn->Ready = {elapsed:?}");
    assert!(elapsed < Duration::from_secs(30), "expected Ready well within the generous 30s bound, took {elapsed:?}");

    end_run_and_expect_ended(&conn, &run, "cleanup");
    stop(&conn);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// The supervisor-epoch rule's whole Linux safety argument, in
/// executable form: the start-time the DAEMON reads off a freshly
/// spawned supervisor is bit-identical to the `created` that same
/// supervisor later authors for itself on the wire. They are not merely
/// compatible units -- `challenge_unix::self_start_ticks` (what
/// `capsule::self_status` reports) is literally
/// `process_start_ticks(std::process::id())`, the same `/proc/<pid>/stat`
/// field 22 read this test performs from the parent side. Because the
/// two authors provably agree, the daemon's spawn-side read buys nothing
/// but EARLINESS and was deleted; only the moment the value is learned
/// changed, never the value. Worth keeping permanently: it pins the two
/// authors together for as long as Linux has both.
#[cfg(target_os = "linux")]
#[test]
fn a_spawned_supervisors_start_ticks_equal_the_created_it_reports() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let spawned_pid = guard.id();
    // The spawn-side read, performed EXACTLY as the daemon's deleted
    // `spawned_identity` performed it: this pid, this function, from the
    // parent, the instant after spawn.
    let spawn_side_ticks =
        sot_platform::identity::challenge_unix::process_start_ticks(spawned_pid).expect("read the child's own /proc start ticks");

    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (reported_pid, reported_created) = conn.peer();

    assert_eq!(reported_pid, spawned_pid, "the supervisor reports the pid the daemon spawned");
    assert_eq!(
        reported_created, spawn_side_ticks,
        "the supervisor's self-authored `created` must equal the start ticks the spawner read for the same pid -- \
         if this ever fails, the daemon's spawn-side identity read was NOT a second implementation of the same value"
    );

    let (_voyage, run) = wait_for_ready(&conn, Duration::from_secs(30));
    end_run_and_expect_ended(&conn, &run, "cleanup");
    stop(&conn);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// `--first-leg-only <token>` keeps the token on the first run this supervisor process starts and strips it from every
/// run after an agent has run: the daemon passes a reauth's `--resume <id>` this way, so only the reauth's own run
/// resumes by id. The agent appends its own argv to a file and sleeps, so each run's own line is unambiguous; the run
/// is ended after Ready and reset, for a fast second run.
#[cfg(target_os = "linux")]
#[test]
fn first_leg_only_keeps_a_token_on_the_first_leg_and_strips_it_after_a_producer_ran() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);
    let log_path = dir.path().join("argv.log");

    // Quoted `"$*"` is always exactly one word (empty when there are no
    // positional params), so `printf` writes exactly one line per leg
    // regardless of whether the token survived.
    let script = format!("printf '%s\\n' \"$*\" >> '{}'; exec sleep 300", log_path.display());
    let mut cmd = Command::new(capsule_exe());
    cmd.arg("supervise")
        .arg(&state_dir)
        .arg("--start")
        .arg("--first-leg-only")
        .arg("--resume")
        .arg("--assume-no-rollback-target")
        .arg("--")
        .arg("/bin/sh")
        .arg("-c")
        .arg(&script)
        .arg("leg")
        .arg("--resume")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut guard = CapsuleGuard::spawn(&mut cmd);

    let read_lines = |path: &Path| -> Option<Vec<String>> {
        std::fs::read_to_string(path).ok().map(|c| c.lines().map(str::to_string).collect())
    };

    let lines = poll_until(
        || read_lines(&log_path).filter(|l| !l.is_empty()),
        Duration::from_secs(30),
        "the first leg to record its own argv",
    );
    assert_eq!(lines[0], "--resume", "the first leg must keep its first-leg-only token");

    // Ready first: that is the agent having run, after which no run carries the token.
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (_voyage, run) = wait_for_ready(&conn, Duration::from_secs(30));
    end_run_and_expect_ended(&conn, &run, "test");
    poll_until(
        || (status(&conn).phase == SupervisorPhase::EndedNoRespawn).then_some(()),
        Duration::from_secs(30),
        "the supervisor to rest at EndedNoRespawn after its run was ended",
    );
    reset(&conn);

    let lines = poll_until(
        || read_lines(&log_path).filter(|l| l.len() >= 2),
        Duration::from_secs(30),
        "the run after the reset to record its own argv",
    );
    assert_eq!(lines[1], "", "a run after an agent ran must not carry the first-leg-only token");

    let (_voyage, run) = wait_for_ready(&conn, Duration::from_secs(30));
    end_run_and_expect_ended(&conn, &run, "cleanup");
    stop(&conn);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// `sot_log::attach_client::supervisor_client::Persistent` reconnects transparently across real processes: the held
/// connection survives the supervisor process being killed and a fresh one resuming the same voyage over the same state
/// dir. (The row socket has no idle close, so the idle-expiry half of the old lane's test has nothing to cross.)
#[test]
fn persistent_client_survives_a_supervisor_restart() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut first_guard = spawn_supervisor(&state_dir, "--start", SHELL);
    // Wait for the row socket over the raw helper first, proving the row is up before `Persistent` connects.
    let raw_conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, run) = wait_for_ready(&raw_conn, Duration::from_secs(90));
    drop(raw_conn);

    let mut persistent = sot_log::attach_client::supervisor_client::Persistent::new(&state_dir);
    let report = persistent.status().expect("first status on a freshly-ready row");
    assert_eq!(report.voyage.as_deref(), Some(voyage.as_str()));
    assert_eq!(report.run.as_deref(), Some(run.as_str()));
    assert_eq!(report.phase, SupervisorPhase::Ready);

    // Kill the supervisor (its agent ends with it) and resume a second one over the same state dir.
    first_guard.child_mut().kill().unwrap();
    first_guard.child_mut().wait().unwrap();

    let mut second_guard = spawn_supervisor(&state_dir, "--resume", SHELL);
    // The old connection is now dead; give the new supervisor's row socket a moment to bind.
    let raw_conn2 = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage2, run2) = wait_for_ready(&raw_conn2, Duration::from_secs(30));
    drop(raw_conn2);
    assert_eq!(voyage2, voyage, "the SAME voyage -- resumed, never re-minted");
    assert_ne!(run2, run, "a fresh agent runs in the resumed voyage");

    let report2 = persistent.status().expect("Persistent must transparently reconnect across a supervisor restart");
    assert_eq!(report2.voyage.as_deref(), Some(voyage.as_str()));
    assert_eq!(report2.run.as_deref(), Some(run2.as_str()));
    assert_eq!(report2.phase, SupervisorPhase::Ready);

    // Clean up through the raw helper -- `Persistent`'s own internals are private.
    let conn3 = wait_for_lane(&h, Duration::from_secs(30));
    end_run_and_expect_ended(&conn3, &run2, "cleanup");
    stop(&conn3);
    let _ = wait_for_exit(&mut second_guard, Duration::from_secs(60));
}

/// `cancel()` and publishing a fresh connection share one lock, so a
/// cancel landing inside that race window is never missed — proven
/// deterministically via a test-support hook, with no thread or sleep.
#[test]
fn a_cancel_landing_between_connect_and_publish_is_never_missed() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let raw_conn = wait_for_lane(&h, Duration::from_secs(30));
    let (_voyage, run) = wait_for_ready(&raw_conn, Duration::from_secs(90));
    drop(raw_conn);

    let mut persistent = sot_log::attach_client::supervisor_client::Persistent::new(&state_dir);
    let cancel_handle = persistent.cancel_handle();
    persistent.set_test_hook_after_connect_before_publish(move || cancel_handle.cancel());

    let result = persistent.status();
    assert!(
        result.is_err(),
        "a connect published after a cancel landed inside the race window must still refuse"
    );

    // Cancellation is persistent, never a one-shot refusal of just the racing call.
    for _ in 0..3 {
        assert!(
            persistent.status().is_err(),
            "a cancelled Persistent must refuse every subsequent status() too, not just the one that raced"
        );
    }

    drop(persistent);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    end_run_and_expect_ended(&conn, &run, "cleanup");
    stop(&conn);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// The reauth's real path: a supervisor started `--resume` on a state dir whose run has ended rests at
/// `EndedNoRespawn` with no producer run, and the reset's leg is the reauth's own leg, so it keeps its first-leg-only
/// token; once that leg has reached Ready, the respawn after it does not.
#[cfg(target_os = "linux")]
#[test]
fn a_reauths_reset_leg_keeps_its_first_leg_only_token_and_its_respawn_drops_it() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);
    let log_path = dir.path().join("argv.log");

    // The run the reauth ends: start, Ready, end, stop.
    let mut first = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (_voyage, run) = wait_for_ready(&conn, Duration::from_secs(90));
    end_run_and_expect_ended(&conn, &run, "test");
    stop(&conn);
    let _ = wait_for_exit(&mut first, Duration::from_secs(30));
    drop(conn);

    // The reauth's supervisor: `--resume` mode with the id first-leg-only.
    let script = format!("printf '%s\\n' \"$*\" >> '{}'; exec sleep 300", log_path.display());
    let mut cmd = Command::new(capsule_exe());
    cmd.arg("supervise")
        .arg(&state_dir)
        .arg("--resume")
        .arg("--first-leg-only")
        .arg("--resume")
        .arg("--assume-no-rollback-target")
        .arg("--")
        .arg("/bin/sh")
        .arg("-c")
        .arg(&script)
        .arg("leg")
        .arg("--resume")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut guard = CapsuleGuard::spawn(&mut cmd);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    poll_until(
        || (status(&conn).phase == SupervisorPhase::EndedNoRespawn).then_some(()),
        Duration::from_secs(30),
        "the reauth's supervisor to rest at EndedNoRespawn",
    );
    reset(&conn);
    let (new_voyage, new_run) = wait_for_ready(&conn, Duration::from_secs(90));

    let read_lines = |path: &Path| -> Option<Vec<String>> {
        std::fs::read_to_string(path).ok().map(|c| c.lines().map(str::to_string).collect())
    };
    let lines = poll_until(|| read_lines(&log_path).filter(|l| !l.is_empty()), Duration::from_secs(30), "the reset's leg argv");
    assert_eq!(lines[0], "--resume", "the reauth's own leg (the reset's) must keep its first-leg-only token");

    end_run_and_expect_ended(&conn, &new_run, "test");
    poll_until(
        || (status(&conn).phase == SupervisorPhase::EndedNoRespawn).then_some(()),
        Duration::from_secs(30),
        "EndedNoRespawn after the reset's run was ended",
    );
    reset(&conn);
    let (current, current_run) = wait_for_ready(&conn, Duration::from_secs(90));
    assert_ne!(current, new_voyage);
    let lines = poll_until(|| read_lines(&log_path).filter(|l| l.len() >= 2), Duration::from_secs(30), "the next run's argv");
    assert_eq!(lines[1], "", "a run after the reauth's own run must not carry the token");

    // A later reset, after a producer ran, starts a leg without the token too (the reset site goes through
    // `leg_argv`, never the captured argv whole).
    end_run_and_expect_ended(&conn, &current_run, "test");
    poll_until(
        || (status(&conn).phase == SupervisorPhase::EndedNoRespawn).then_some(()),
        Duration::from_secs(30),
        "EndedNoRespawn before the second reset",
    );
    reset(&conn);
    let (_last, last_run) = wait_for_ready(&conn, Duration::from_secs(90));
    let lines = poll_until(|| read_lines(&log_path).filter(|l| l.len() >= 3), Duration::from_secs(30), "the second reset's leg argv");
    assert_eq!(lines[2], "", "a reset after a producer ran must not carry the token");

    end_run_and_expect_ended(&conn, &last_run, "cleanup");
    stop(&conn);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// An agent that ends by itself, with no end asked for, ends the supervisor with 76: the daemon's watchdog restarts
/// it, and the supervisor never rests at `EndedNoRespawn` over an end nobody requested.
#[test]
fn an_agent_that_ends_by_itself_ends_the_supervisor_with_76() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    #[cfg(windows)]
    let exits: &[&str] = &["cmd.exe", "/d", "/c", "exit 3"];
    #[cfg(not(windows))]
    let exits: &[&str] = &["/bin/sh", "-c", "exit 3"];
    let mut guard = spawn_supervisor(&state_dir, "--start", exits);
    let status = wait_for_exit(&mut guard, Duration::from_secs(30));
    assert_eq!(status.code(), Some(sot_log::supervisor::EXIT_AGENT_ENDED), "exit status: {status:?}");
}

/// An agent that cannot be started ends the supervisor with 75, never `terminal` (69): the watchdog's budget decides.
#[test]
fn an_agent_that_cannot_be_started_ends_the_supervisor_with_75() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let mut guard = spawn_supervisor(&state_dir, "--start", &["/nonexistent/agent"]);
    let status = wait_for_exit(&mut guard, Duration::from_secs(30));
    assert_eq!(status.code(), Some(sot_log::supervisor::EXIT_AGENT_NOT_STARTED), "exit status: {status:?}");
}

/// A stop is not an end of the run: it leaves no request on the voyage, so the next start resumes the row with a new
/// run in the same voyage.
#[test]
fn a_stop_leaves_no_end_request_so_the_next_start_runs_the_agent_again() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut first = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, run) = wait_for_ready(&conn, Duration::from_secs(90));
    stop(&conn);
    let status = wait_for_exit(&mut first, Duration::from_secs(30));
    assert_eq!(status.code(), Some(sot_log::supervisor::EXIT_CLEAN), "exit status: {status:?}");
    drop(conn);

    let mut second = spawn_supervisor(&state_dir, "--resume", SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage_after, run_after) = wait_for_ready(&conn, Duration::from_secs(90));
    assert_eq!(voyage_after, voyage, "the same voyage");
    assert_ne!(run_after, run, "one new run");
    end_run_and_expect_ended(&conn, &run_after, "cleanup");
    stop(&conn);
    let _ = wait_for_exit(&mut second, Duration::from_secs(30));
}

/// An `end_run` that was acknowledged is a requested end even when the agent exits on its own first: the request reaches
/// the run only after the agent is gone (`SOT_TEST_END_RUN_HOLD_MS`), yet the voyage records it, so a stop and a resume
/// rest and never start another run.
#[cfg(target_os = "linux")]
#[test]
fn an_end_run_the_agent_beat_is_still_a_requested_end_after_a_stop_and_a_resume() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);
    let trigger = dir.path().join("exit-now");
    let script = format!("while [ ! -e '{}' ]; do sleep 0.05; done", trigger.display());
    let argv = ["/bin/sh", "-c", script.as_str()];

    let mut first = spawn_supervisor_with_env(&state_dir, "--start", &argv, &[("SOT_TEST_END_RUN_HOLD_MS", "4000")]);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (_voyage, run) = wait_for_ready(&conn, Duration::from_secs(90));
    let ender = {
        let conn = wait_for_lane(&h, Duration::from_secs(30));
        std::thread::spawn(move || end_run(&conn, &run, "test"))
    };
    // The request is admitted; the agent ends before the run is told.
    poll_until(
        || (status(&conn).phase == SupervisorPhase::Ending).then_some(()),
        Duration::from_secs(30),
        "the end_run to be admitted",
    );
    std::fs::write(&trigger, b"").unwrap();
    assert_eq!(ender.join().unwrap()["outcome"], "ended");
    stop(&conn);
    let _ = wait_for_exit(&mut first, Duration::from_secs(30));
    drop(conn);

    let mut second = spawn_supervisor(&state_dir, "--resume", &argv);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    poll_until(
        || (status(&conn).phase == SupervisorPhase::EndedNoRespawn).then_some(()),
        Duration::from_secs(30),
        "the resumed supervisor to rest: the acknowledged end was requested",
    );
    stop(&conn);
    let _ = wait_for_exit(&mut second, Duration::from_secs(30));
}

/// A requested end that cannot be recorded is not an end: the supervisor does not answer `ended`, starts no other agent
/// and ends terminal. The record is the only thing a later `--resume` reads, so an end it never read would run again.
#[cfg(target_os = "linux")]
#[test]
fn a_requested_end_that_cannot_be_recorded_is_not_answered_ended() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor_with_env(&state_dir, "--start", SHELL, &[("SOT_TEST_END_MARK_FAILS", "1")]);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (_voyage, run) = wait_for_ready(&conn, Duration::from_secs(90));
    drop(conn);

    let outcome = sot_log::attach_client::supervisor_client::end_run(&state_dir, &run, "test").expect("the end_run call is answered");
    match outcome {
        sot_log::attach_client::supervisor_client::EndRunOutcome::Failed(detail) => {
            assert!(detail.contains("record"), "the failure names the record: {detail}");
        }
        other => panic!("an end that was not recorded was answered {other:?}"),
    }
    let (report, _process) = sot_log::attach_client::supervisor_client::query_status(&state_dir).expect("the terminal supervisor still answers");
    assert_eq!(report.phase, SupervisorPhase::Terminal);
    let status = wait_for_exit(&mut guard, Duration::from_secs(30));
    assert_eq!(status.code(), Some(sot_log::supervisor::EXIT_TERMINAL), "exit status: {status:?}");
}
