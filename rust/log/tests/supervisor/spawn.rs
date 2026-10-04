//! Supervisor leg-spawn tests: start readiness, tick counts, binary rename, first_leg_without, client survival, cancel.

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
    let (_voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(30));
    let elapsed = started.elapsed();
    println!("LU6b start_reaches_ready_promptly: spawn->Ready = {elapsed:?}");
    assert!(elapsed < Duration::from_secs(30), "expected Ready well within the generous 30s bound, took {elapsed:?}");

    let _ = command(&conn, "start-reaches-ready-stop", SupervisorOp::Stop);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// The supervisor-epoch ruling's whole Linux safety argument, in
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
        sot_log::identity::challenge_unix::process_start_ticks(spawned_pid).expect("read the child's own /proc start ticks");

    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (reported_pid, reported_created) =
        match request_for_test(&conn, &SupervisorRequest::Status, Instant::now() + Duration::from_secs(5)).expect("status")
        {
            SupervisorReply::StatusOk { pid, created, .. } => (pid, created),
            other => panic!("expected StatusOk, got {other:?}"),
        };

    assert_eq!(reported_pid, spawned_pid, "the supervisor reports the pid the daemon spawned");
    assert_eq!(
        reported_created, spawn_side_ticks,
        "the supervisor's self-authored `created` must equal the start ticks the spawner read for the same pid -- \
         if this ever fails, the daemon's spawn-side identity read was NOT a second implementation of the same value"
    );

    let _ = command(&conn, "identity-equality-stop", SupervisorOp::Stop);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// ADR 0043 decision 33's retirement clause: a leg forks from the SUPERVISOR's own running
/// image via `/proc/self/exe`, never a path string resolved fresh off
/// disk at spawn time -- so an `sot-apply`-style rename of a new binary
/// over the old launch path cannot make an already-running supervisor
/// hand a freshly spawned leg the NEW build. Proven by launching a
/// supervisor from a COPY of the built `sot-capsule`, renaming that copy
/// away once a leg is up, then killing the leg so the supervisor's own
/// anti-flap respawn fires a fresh spawn under the (now binary-less)
/// launch path: the respawned leg's `/proc/<pid>/exe` -- the kernel's own
/// live-mapping identity, immune to the very rename this test performs --
/// must still match the SUPERVISOR's own (dev, ino), not whatever (or
/// nothing) now sits at the renamed-away path.
#[cfg(target_os = "linux")]
#[test]
fn a_leg_spawned_after_the_binary_is_renamed_runs_the_supervisors_own_inode() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    // A COPY, launched from a path this test can rename out from under --
    // the built binary itself (`capsule_exe()`) must stay put for every
    // other test in this file.
    let copy_path = dir.path().join("sot-capsule-copy");
    std::fs::copy(capsule_exe(), &copy_path).expect("copy sot-capsule for a renameable launch path");
    std::fs::set_permissions(&copy_path, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut cmd = Command::new(&copy_path);
    cmd.arg("supervise")
        .arg(&state_dir)
        .arg("--start")
        .arg("--assume-no-rollback-target")
        .arg("--")
        .args(SHELL)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut guard = CapsuleGuard::new_for_exe(
        cmd.spawn().expect("spawn sot-capsule supervise from the copy"),
        &copy_path,
        &state_dir,
    );
    let supervisor_pid = guard.id();

    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    let original_leg_pid = poll_until(
        || direct_children_of(supervisor_pid).into_iter().next(),
        Duration::from_secs(10),
        "the supervisor's own leg child pid to appear",
    );

    // The identity every leg spawn must actually run: the running
    // supervisor's OWN loaded image, read through its own magic
    // `/proc/<pid>/exe` link (a live-mapping identity, not a path
    // lookup a rename can redirect).
    let supervisor_exe = std::fs::metadata(format!("/proc/{supervisor_pid}/exe")).expect("stat the supervisor's own /proc/<pid>/exe");
    let supervisor_identity = (supervisor_exe.dev(), supervisor_exe.ino());

    // Displace the launch path -- exactly what an `sot-apply` rename-based
    // install does. The already-running supervisor and its live leg are
    // unaffected (their own mapped inodes stay open); only a fresh spawn
    // that re-resolved a PATH would notice anything happened here.
    std::fs::rename(&copy_path, dir.path().join("sot-capsule-copy.renamed-aside")).expect("rename the launch path away");

    // Kill the LEG (not the supervisor authority) so its own anti-flap
    // respawn -- one kill, well under `FLAP_THRESHOLD` -- fires a fresh
    // spawn under the now binary-less launch path: exactly the spawn
    // this decision protects.
    unsafe {
        libc::kill(original_leg_pid as libc::pid_t, libc::SIGKILL);
    }

    let respawned_leg_pid = poll_until(
        || direct_children_of(supervisor_pid).into_iter().find(|&pid| pid != original_leg_pid),
        Duration::from_secs(30),
        "a respawned leg child pid to appear after the original was killed",
    );
    let respawned_exe = std::fs::metadata(format!("/proc/{respawned_leg_pid}/exe"))
        .expect("stat the respawned leg's own /proc/<pid>/exe");
    assert_eq!(
        (respawned_exe.dev(), respawned_exe.ino()),
        supervisor_identity,
        "a leg spawned after the launch binary was renamed away must still run the supervisor's OWN inode"
    );

    // End the run FIRST -- `stop` alone ends only the authority (ADR
    // 0041 Lifecycle: legs are deliberately outside its job), which would
    // otherwise leak the respawned leg's own shell as an orphaned process
    // once this test's tempdir goes away with no supervisor left to
    // adopt it. Re-waits for Ready: the respawned leg's OWN pid appearing
    // (already proven above) is not the same fact as the supervisor's
    // phase having caught up to it -- `end_run` refuses a voyage with no
    // leg it considers currently running.
    wait_for_ready(&conn, Duration::from_secs(30));
    end_run_and_expect_record_closed(&conn, "cleanup-end", "cleanup", voyage);
    let _ = poll_to_terminal(&conn, "cleanup-end", Duration::from_secs(60));
    let _ = command(&conn, "cleanup-stop", SupervisorOp::Stop);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// `--first-leg-without <token>` (docs/adr/0042 §1's 2026-09-12/09-14
/// amendments) strips the token from the very first leg this supervisor
/// process spawns, AND from any leg that follows one classified unstable
/// (the self-heal). A SIGKILL moments after Ready is by construction an
/// unstable death (`leg_was_stable` needs `STABILITY_INTERVAL`, 60s, of
/// uptime), so the respawn it forces must ALSO come back without the
/// token -- never the stale argv a plain "first leg only" rule would hand
/// back. The producer appends its own argv to a file and sleeps, rather
/// than self-exiting, so each leg's own line is unambiguous; the leg is
/// killed directly (as the rename test above does) for a fast, direct
/// respawn signal instead of waiting out a timed self-exit.
#[cfg(target_os = "linux")]
#[test]
fn first_leg_without_strips_a_token_from_the_first_leg_and_an_unstable_respawn() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);
    let log_path = dir.path().join("argv.log");

    // Quoted `"$*"` is always exactly one word (empty when there are no
    // positional params), so `printf` writes exactly one line per leg
    // regardless of whether `--continue` survived.
    let script = format!("printf '%s\\n' \"$*\" >> '{}'; exec sleep 300", log_path.display());
    let mut cmd = Command::new(capsule_exe());
    cmd.arg("supervise")
        .arg(&state_dir)
        .arg("--start")
        .arg("--first-leg-without")
        .arg("--continue")
        .arg("--assume-no-rollback-target")
        .arg("--")
        .arg("/bin/sh")
        .arg("-c")
        .arg(&script)
        .arg("leg")
        .arg("--continue")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut guard = CapsuleGuard::new(cmd.spawn().expect("spawn sot-capsule supervise"), &state_dir);
    let supervisor_pid = guard.id();

    let read_lines = |path: &Path| -> Option<Vec<String>> {
        std::fs::read_to_string(path).ok().map(|c| c.lines().map(str::to_string).collect())
    };

    let lines = poll_until(
        || read_lines(&log_path).filter(|l| !l.is_empty()),
        Duration::from_secs(30),
        "the first leg to record its own argv",
    );
    assert_eq!(lines[0], "", "the first leg must have --continue stripped from its argv");

    let original_leg_pid = poll_until(
        || direct_children_of(supervisor_pid).into_iter().next(),
        Duration::from_secs(10),
        "the supervisor's own first leg child pid to appear",
    );
    unsafe {
        libc::kill(original_leg_pid as libc::pid_t, libc::SIGKILL);
    }
    poll_until(
        || direct_children_of(supervisor_pid).into_iter().find(|&pid| pid != original_leg_pid),
        Duration::from_secs(30),
        "a respawned leg child pid to appear after the original was killed",
    );

    let lines = poll_until(
        || read_lines(&log_path).filter(|l| l.len() >= 2),
        Duration::from_secs(30),
        "the respawned leg to record its own argv",
    );
    assert_eq!(
        lines[1], "",
        "a respawn that follows an UNSTABLE leg must also have --continue stripped (the self-heal)"
    );

    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(30));
    end_run_and_expect_record_closed(&conn, "cleanup-end", "cleanup", voyage);
    let _ = poll_to_terminal(&conn, "cleanup-end", Duration::from_secs(60));
    let _ = command(&conn, "cleanup-stop", SupervisorOp::Stop);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// `sot_log::attach_client::supervisor_client::Persistent` reconnects transparently
/// across REAL processes: (a) the held connection survives the
/// supervisor process being killed and a fresh one adopting the same
/// leg over the same state dir, and (b) it survives the supervisor's
/// own idle-connection expiry (`LANE_IDLE_DEADLINE`, 5s) with no
/// process change at all.
#[test]
fn persistent_client_survives_a_supervisor_restart_and_a_5s_idle_expiry() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut first_guard = spawn_supervisor(&state_dir, "--start", SHELL);
    // Wait for the lane over the raw helper first, proving the row is up before `Persistent` connects.
    let raw_conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, leg) = wait_for_ready(&raw_conn, Duration::from_secs(90));
    drop(raw_conn);

    let mut persistent = sot_log::attach_client::supervisor_client::Persistent::new(&state_dir);
    let report = persistent.status().expect("first status on a freshly-ready row");
    assert_eq!(report.voyage.as_deref(), Some(voyage.as_str()));
    assert_eq!(report.leg, Some(leg));
    assert_eq!(report.phase, SupervisorPhase::Ready);

    // (a) Kill the supervisor only (the leg survives) and spawn a second one over the same state dir.
    first_guard.child_mut().kill().unwrap();
    first_guard.child_mut().wait().unwrap();

    let mut second_guard = spawn_supervisor(&state_dir, "--start", SHELL);
    // The old connection is now dead; give the new supervisor's lane a moment to bind.
    let raw_conn2 = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage2, leg2) = wait_for_ready(&raw_conn2, Duration::from_secs(30));
    drop(raw_conn2);
    assert_eq!(voyage2, voyage, "the SAME voyage -- adopted, never re-minted");
    assert_eq!(leg2, leg, "the SAME leg epoch -- adopted, never a fresh spawn");

    let report2 = persistent
        .status()
        .expect("Persistent must transparently reconnect across a supervisor restart");
    assert_eq!(report2.voyage.as_deref(), Some(voyage.as_str()));
    assert_eq!(report2.leg, Some(leg));
    assert_eq!(report2.phase, SupervisorPhase::Ready);

    // (b) Idle expiry: let the same held connection sit past the supervisor's own 5s deadline, then ask again.
    std::thread::sleep(Duration::from_secs(6));
    let report3 = persistent
        .status()
        .expect("Persistent must transparently reconnect across the supervisor's own 5s idle expiry");
    assert_eq!(report3.voyage.as_deref(), Some(voyage.as_str()));
    assert_eq!(report3.leg, Some(leg));
    assert_eq!(report3.phase, SupervisorPhase::Ready);

    // Clean up through the raw helper -- `Persistent`'s own internals are private.
    let conn3 = wait_for_lane(&h, Duration::from_secs(30));
    end_run_and_expect_record_closed(&conn3, "cleanup-end", "cleanup", voyage);
    let _ = poll_to_terminal(&conn3, "cleanup-end", Duration::from_secs(60));
    let _ = command(&conn3, "cleanup-stop", SupervisorOp::Stop);
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
    let (voyage, _leg) = wait_for_ready(&raw_conn, Duration::from_secs(90));
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
    end_run_and_expect_record_closed(&conn, "cleanup-end", "cleanup", voyage);
    let _ = poll_to_terminal(&conn, "cleanup-end", Duration::from_secs(60));
    let _ = command(&conn, "cleanup-stop", SupervisorOp::Stop);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// The self-heal buys one clean retry, never an exemption: a producer
/// that fails fast for a reason that has NOTHING to do with the stripped
/// token (every leg dies the same way regardless) must still trip the
/// anti-flap bound and end the supervisor Terminal -- exactly
/// [`a_shell_that_dies_shortly_after_ready_trips_the_anti_flap_bound`]
/// above, with `--first-leg-without <token>` also configured, proving the
/// flag does not disable the bound it shares `leg_was_stable`'s own
/// classification with.
#[cfg(target_os = "linux")]
#[test]
fn first_leg_without_does_not_exempt_a_real_crash_loop_from_the_anti_flap_bound() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut cmd = Command::new(capsule_exe());
    cmd.arg("supervise")
        .arg(&state_dir)
        .arg("--start")
        .arg("--first-leg-without")
        .arg("--continue")
        .arg("--assume-no-rollback-target")
        .arg("--")
        .args(SELF_EXITING_PRODUCER)
        .arg("--continue")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut guard = CapsuleGuard::new(cmd.spawn().expect("spawn sot-capsule supervise"), &state_dir);

    let conn = wait_for_lane(&h, Duration::from_secs(30));
    poll_until(
        || {
            let (_voyage, _leg, phase) = status(&conn);
            (phase == SupervisorPhase::Ready).then_some(true)
        },
        Duration::from_secs(60),
        "the leg to reach Ready at least once before its own timed self-exit",
    );
    drop(conn);

    let status = wait_for_exit_with_diagnostics(guard.child_mut(), &h, Duration::from_secs(180));
    assert_eq!(
        status.code(),
        Some(sot_log::supervisor::EXIT_TERMINAL),
        "a producer that fails regardless of the token must still trip the anti-flap bound"
    );
}
