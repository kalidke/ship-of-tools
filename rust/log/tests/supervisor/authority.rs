//! Supervisor authority tests: the rules of a reset, and a run's identity across one.

use super::*;

/// `reset` is admissible only from `ended_no_respawn`: while a run is live it is refused `failed`, naming the live run,
/// and the run and the end record are as they were.
#[test]
fn reset_is_refused_while_a_run_is_live() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let run = wait_for_ready(&conn, Duration::from_secs(90));

    let reply = conn.call("row.reset", json!({}), Duration::from_secs(30));
    assert_eq!(reply["outcome"], "failed", "{reply}");
    let detail = reply["detail"].as_str().unwrap_or_default().to_lowercase();
    assert!(detail.contains("live"), "expected a live-run refusal, got {detail:?}");

    assert_eq!(status(&conn).run.as_deref(), Some(run.as_str()), "the refused reset left the run as it was");
    assert!(!state_dir.join("ended").exists(), "the refused reset left no end record");

    end_run_and_expect_ended(&conn, &run, "cleanup");
    stop(&conn);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// Once `ended_no_respawn`, `reset` is admissible, removes the end record and starts a fresh agent in a new run.
#[test]
fn reset_from_ended_no_respawn_removes_the_end_record_and_starts_an_agent_in_a_new_run() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let old_run = wait_for_ready(&conn, Duration::from_secs(90));

    end_run_and_expect_ended(&conn, &old_run, "test");
    assert_eq!(status(&conn).phase, SupervisorPhase::EndedNoRespawn);
    assert!(state_dir.join("ended").exists(), "the end is recorded");

    reset(&conn);
    assert!(!state_dir.join("ended").exists(), "the reset removed the end record");

    // The supervisor starts a fresh agent.
    let new_run = wait_for_ready(&conn, Duration::from_secs(90));
    assert_ne!(new_run, old_run, "a new agent start is a new run");

    end_run_and_expect_ended(&conn, &new_run, "cleanup");
    stop(&conn);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// A second reset, asked while the first is in progress or after it started a run, is refused: the phase fences it.
#[test]
fn a_reset_right_after_a_reset_is_refused_while_its_run_is_live() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let run = wait_for_ready(&conn, Duration::from_secs(90));
    end_run_and_expect_ended(&conn, &run, "test");
    reset(&conn);

    let again = conn.call("row.reset", json!({}), Duration::from_secs(30));
    assert_eq!(again["outcome"], "failed", "{again}");

    let run = wait_for_ready(&conn, Duration::from_secs(90));
    end_run_and_expect_ended(&conn, &run, "cleanup");
    stop(&conn);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// A reset's answer is its publication: a supervisor killed after it is answered and before its next run starts is
/// resumed into an agent, never back onto the end the reset removed.
#[cfg(target_os = "linux")]
#[test]
fn a_reset_answered_before_a_kill_is_not_undone_by_the_resume() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);
    let starts = dir.path().join("starts");
    let script = format!("echo started >> {}; exec cat", starts.display());
    let argv = ["/bin/sh", "-c", script.as_str()];

    let mut first = spawn_supervisor_with_env(&state_dir, "--start", &argv, &[("SOT_TEST_RESET_HOLD_MS", "5000")]);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let run = wait_for_ready(&conn, Duration::from_secs(90));
    end_run_and_expect_ended(&conn, &run, "test");
    let reply = conn.call("row.reset", json!({}), Duration::from_secs(40));
    assert_eq!(reply["outcome"], "reset", "{reply}");
    first.child_mut().kill().unwrap();
    first.child_mut().wait().unwrap();
    drop(conn);

    let mut second = spawn_supervisor(&state_dir, "--resume", &argv);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let run = wait_for_ready(&conn, Duration::from_secs(90));
    assert!(std::fs::read_to_string(&starts).unwrap().lines().count() >= 2, "the resume started the agent again");
    end_run_and_expect_ended(&conn, &run, "cleanup");
    stop(&conn);
    let _ = wait_for_exit(&mut second, Duration::from_secs(30));
}
