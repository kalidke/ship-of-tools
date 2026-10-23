//! Supervisor start tests: start readiness, the identity it reports, first_leg_only, the watch, exit codes.

use super::*;

/// ADR 0043 decision 27/30: with the InitialProbe's connect no longer
/// paying the full [`sot_log::lane::transport::CONNECT_BOUND`] on an absent
/// run's pipe (it fails fast on `ENOENT`/`ECONNREFUSED` now, retried
/// only at [`sot_log::supervisor`]'s own 250ms `ATTEMPT_INTERVAL`), a
/// fresh `--start` should reach `Ready` in the two seconds the agent
/// must live to be ready, not the old fixed wait on top of them. Asserted
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
    let run = wait_for_ready(&conn, Duration::from_secs(30));
    let elapsed = started.elapsed();
    println!("LU6b start_reaches_ready_promptly: spawn->Ready = {elapsed:?}");
    assert!(elapsed < Duration::from_secs(30), "expected Ready well within the generous 30s bound, took {elapsed:?}");

    end_run_and_expect_ended(&conn, &run, "cleanup");
    stop(&conn);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// `ready` is the agent having lived 2 s, not having written: an agent that writes at once is `starting` until the
/// supervisor's own two seconds have passed. The proof is a lower bound on the clock and an upper bound on the wait: no
/// report says `ready` before the supervisor's launch plus 2 s, and one comes within 5 s after it.
#[cfg(unix)]
#[test]
fn a_watch_reports_ready_only_after_the_agent_lived_two_seconds() {
    use sot_log::attach_client::supervisor_client::{watch, CancelHandle, WatchStep};
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let spawned = Instant::now();
    let mut guard = spawn_supervisor(&state_dir, "--start", &["/bin/sh", "-c", "echo hello; exec cat"]);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let mut held = watch(&state_dir, &CancelHandle::new()).expect("the watch");
    let mut ready_at = (held.first().phase == SupervisorPhase::Ready).then(Instant::now);
    while ready_at.is_none() && spawned.elapsed() < Duration::from_secs(7) {
        match held.next(Duration::from_millis(100)) {
            WatchStep::Phase(report) if report.phase == SupervisorPhase::Ready => ready_at = Some(Instant::now()),
            WatchStep::Ended => panic!("the watch ended before the supervisor was ready"),
            _ => {}
        }
    }
    let ready_at = ready_at.expect("a report said ready within 7 s of the launch");
    assert!(
        ready_at.duration_since(spawned) >= Duration::from_secs(2),
        "ready was reported {:?} after the launch, before the agent had lived 2 s",
        ready_at.duration_since(spawned)
    );

    let run = wait_for_ready(&conn, Duration::from_secs(30));
    end_run_and_expect_ended(&conn, &run, "cleanup");
    drop(held);
    stop(&conn);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// A `--resume` over a state folder with no record of an end (the supervisor that left it died before any run) starts
/// the agent like a `--start` does, rather than ending with an error.
#[test]
fn a_resume_with_nothing_to_resume_starts_the_agent() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--resume", SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let run = wait_for_ready(&conn, Duration::from_secs(30));
    end_run_and_expect_ended(&conn, &run, "cleanup");
    stop(&conn);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// A run asked to end stays ended across supervisor restarts: the record is `<state_dir>/ended`, a `--resume` over it
/// rests in `ended_no_respawn` naming no run (a supervisor names only the runs it began) and starts no agent, and a
/// second restart reads it again.
#[cfg(unix)]
#[test]
fn a_run_asked_to_end_rests_after_a_supervisor_restart() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);
    let starts = dir.path().join("starts");
    let script = format!("echo started >> {}; exec cat", starts.display());
    let argv = ["/bin/sh", "-c", script.as_str()];
    let started = || std::fs::read_to_string(&starts).unwrap().lines().count();

    let mut first = spawn_supervisor(&state_dir, "--start", &argv);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let run = wait_for_ready(&conn, Duration::from_secs(30));
    end_run_and_expect_ended(&conn, &run, "cleanup");
    let record = std::fs::read(state_dir.join("ended")).expect("the end is recorded in <state_dir>/ended");
    let record: Value = serde_json::from_slice(&record).expect("the record is JSON");
    assert_eq!(record["requested"], true, "the record says the end was asked for: {record}");
    first.child_mut().kill().unwrap();
    first.child_mut().wait().unwrap();
    drop(conn);
    assert_eq!(started(), 1, "one agent ran");

    for restart in ["second", "third"] {
        let mut resumed = spawn_supervisor(&state_dir, "--resume", &argv);
        let conn = wait_for_lane(&h, Duration::from_secs(30));
        let rested = poll_until(
            || Some(status(&conn)).filter(|s| s.phase != SupervisorPhase::Starting),
            Duration::from_secs(30),
            "the resumed supervisor to settle",
        );
        assert_eq!(rested.phase, SupervisorPhase::EndedNoRespawn, "the {restart} supervisor rests on the requested end");
        assert_eq!(rested.run, None, "a supervisor restarted onto an end names no run");
        assert_eq!(started(), 1, "no second agent started under the {restart} supervisor");
        if restart == "second" {
            resumed.child_mut().kill().unwrap();
            resumed.child_mut().wait().unwrap();
        } else {
            stop(&conn);
            let _ = wait_for_exit(&mut resumed, Duration::from_secs(30));
        }
    }
}

/// A supervisor leaves a state folder holding its fence, the screen and the end record, and no voyage folder.
#[test]
fn no_voyage_folder_is_written() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let run = wait_for_ready(&conn, Duration::from_secs(90));
    end_run_and_expect_ended(&conn, &run, "cleanup");
    stop(&conn);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));

    let mut names: Vec<String> =
        std::fs::read_dir(&state_dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    let allowed = ["supervisor.lock", "screen", "ended"];
    assert!(names.iter().all(|n| allowed.contains(&n.as_str())), "the state folder holds {names:?}, beyond {allowed:?}");
}

/// A record of how the last run ended that cannot be read is not a record of no end: a `--resume` over it is terminal
/// before any run and starts no agent.
#[cfg(unix)]
#[test]
fn an_unreadable_end_record_starts_no_agent_on_resume() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let starts = dir.path().join("starts");
    let script = format!("echo started >> {}; exec cat", starts.display());
    let argv = ["/bin/sh", "-c", script.as_str()];
    std::fs::write(state_dir.join("ended"), b"this is not json").unwrap();

    let mut guard = spawn_supervisor(&state_dir, "--resume", &argv);
    let status = wait_for_exit(&mut guard, Duration::from_secs(30));
    assert_eq!(status.code(), Some(sot_log::supervisor::EXIT_TERMINAL), "exit status: {status:?}");
    assert!(!starts.exists(), "no agent started over a record that could not be read");
}

/// The supervisor makes its own state folder: on Windows the start creates nothing before it.
#[test]
fn a_supervisor_creates_its_own_state_folder() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");

    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);
    poll_until(|| state_dir.is_dir().then_some(()), Duration::from_secs(30), "the supervisor to make its state folder");
    let conn = wait_for_lane(&state_dir_hash(&state_dir), Duration::from_secs(30));
    let run = wait_for_ready(&conn, Duration::from_secs(90));
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

    let run = wait_for_ready(&conn, Duration::from_secs(30));
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
    let run = wait_for_ready(&conn, Duration::from_secs(30));
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

    let run = wait_for_ready(&conn, Duration::from_secs(30));
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
    let run = wait_for_ready(&conn, Duration::from_secs(90));
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
    let new_run = wait_for_ready(&conn, Duration::from_secs(90));

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
    let current_run = wait_for_ready(&conn, Duration::from_secs(90));
    assert_ne!(current_run, new_run);
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
    let last_run = wait_for_ready(&conn, Duration::from_secs(90));
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

/// A stop is not an end of the run: it leaves no request in the end record, so the next start resumes the row with a
/// new run.
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
    let run = wait_for_ready(&conn, Duration::from_secs(90));
    stop(&conn);
    let status = wait_for_exit(&mut first, Duration::from_secs(30));
    assert_eq!(status.code(), Some(sot_log::supervisor::EXIT_CLEAN), "exit status: {status:?}");
    drop(conn);

    let mut second = spawn_supervisor(&state_dir, "--resume", SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let run_after = wait_for_ready(&conn, Duration::from_secs(90));
    assert_ne!(run_after, run, "one new run");
    end_run_and_expect_ended(&conn, &run_after, "cleanup");
    stop(&conn);
    let _ = wait_for_exit(&mut second, Duration::from_secs(30));
}

/// An `end_run` that was acknowledged is a requested end even when the agent exits on its own first: the request reaches
/// the run only after the agent is gone (`SOT_TEST_END_RUN_HOLD_MS`), yet the end record says so, so a stop and a resume
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
    let run = wait_for_ready(&conn, Duration::from_secs(90));
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
    let run = wait_for_ready(&conn, Duration::from_secs(90));
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

/// Every end answered `ended` is recorded as requested before the answer, whenever it was asked: after the agent ended
/// by itself, either the end is refused (no run is live), or it is answered `ended` and a kill and a `--resume` rest.
/// The seam holds the main thread after its check for a requested end, in the window the end is asked in.
#[cfg(target_os = "linux")]
#[test]
fn an_end_run_asked_after_the_agent_ended_by_itself_is_never_answered_ended_unrecorded() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);
    let starts = dir.path().join("starts");
    let trigger = dir.path().join("exit-now");
    let script = format!("echo started >> {}; while [ ! -e '{}' ]; do sleep 0.05; done", starts.display(), trigger.display());
    let argv = ["/bin/sh", "-c", script.as_str()];
    let started = || std::fs::read_to_string(&starts).unwrap().lines().count();

    let mut first = spawn_supervisor_with_env(&state_dir, "--start", &argv, &[("SOT_TEST_RUN_OVER_HOLD_MS", "8000")]);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let run = wait_for_ready(&conn, Duration::from_secs(90));
    std::fs::write(&trigger, b"").unwrap();
    // The agent is gone well inside the hold; the end is asked in the window the hold opens.
    std::thread::sleep(Duration::from_secs(3));
    let reply = conn.0.borrow_mut().call("row.end_run", json!({"run": run, "reason": "late"}), Duration::from_secs(40));
    let acknowledged = reply.map(|f| f.payload["outcome"] == "ended").unwrap_or(false);
    first.child_mut().kill().unwrap();
    first.child_mut().wait().unwrap();
    drop(conn);
    assert_eq!(started(), 1, "one agent ran");
    std::fs::remove_file(&trigger).unwrap();

    let mut second = spawn_supervisor(&state_dir, "--resume", &argv);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let settled = poll_until(
        || Some(status(&conn)).filter(|s| s.phase != SupervisorPhase::Starting),
        Duration::from_secs(30),
        "the resumed supervisor to settle",
    );
    assert_eq!(
        settled.phase == SupervisorPhase::EndedNoRespawn,
        acknowledged,
        "an end answered `ended` rests after a resume, one that was not does not (acknowledged: {acknowledged}, phase: {:?})",
        settled.phase
    );
    assert_eq!(started(), if acknowledged { 1 } else { 2 }, "no agent starts over an acknowledged end");
    stop(&conn);
    let _ = wait_for_exit(&mut second, Duration::from_secs(30));
}
