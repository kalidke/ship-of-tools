//! Supervisor authority tests: the rules of a reset, and a voyage's identity across one.

use super::*;

/// `reset` is admissible only from `ended_no_respawn`: while a run is live it is refused `failed`, naming the live run,
/// and the pointer is not changed.
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
    let (voyage, run) = wait_for_ready(&conn, Duration::from_secs(90));

    let reply = conn.call("row.reset", json!({}), Duration::from_secs(30));
    assert_eq!(reply["outcome"], "failed", "{reply}");
    let detail = reply["detail"].as_str().unwrap_or_default().to_lowercase();
    assert!(detail.contains("live"), "expected a live-run refusal, got {detail:?}");

    match sot_log::supervisor::journal::pointer::validate(&state_dir) {
        sot_log::supervisor::journal::pointer::PointerState::Valid(id) => assert_eq!(id, voyage, "the pointer must be unchanged after the refusal"),
        other => panic!("expected the pointer to still be valid and unchanged, got {other:?}"),
    }

    end_run_and_expect_ended(&conn, &run, "cleanup");
    stop(&conn);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// Once `ended_no_respawn`, `reset` is admissible and produces a genuinely new voyage the supervisor then starts a fresh
/// agent for.
#[test]
fn reset_from_ended_no_respawn_mints_a_new_voyage_and_starts_an_agent_for_it() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (old_voyage, old_run) = wait_for_ready(&conn, Duration::from_secs(90));

    end_run_and_expect_ended(&conn, &old_run, "test");
    assert_eq!(status(&conn).phase, SupervisorPhase::EndedNoRespawn);

    let new_voyage = reset(&conn);
    assert_ne!(new_voyage, old_voyage);

    // The supervisor starts a fresh agent for the new voyage.
    let (now_voyage, new_run) = wait_for_ready(&conn, Duration::from_secs(90));
    assert_eq!(now_voyage, new_voyage);
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
    let (_voyage, run) = wait_for_ready(&conn, Duration::from_secs(90));
    end_run_and_expect_ended(&conn, &run, "test");
    let new_voyage = reset(&conn);

    let again = conn.call("row.reset", json!({}), Duration::from_secs(30));
    assert_eq!(again["outcome"], "failed", "{again}");
    assert_eq!(status(&conn).voyage.as_deref(), Some(new_voyage.as_str()), "the second reset changed nothing");

    let (_voyage, run) = wait_for_ready(&conn, Duration::from_secs(90));
    end_run_and_expect_ended(&conn, &run, "cleanup");
    stop(&conn);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}
