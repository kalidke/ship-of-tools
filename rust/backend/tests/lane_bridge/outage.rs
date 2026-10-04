//! A blackhole, a daemon outage past the window, and a terminal row after the window.

use super::*;

// -----------------------------------------------------------------------
// (iv)+(v) A blackhole is unreachable and retried, never terminal; a cut
// tunnel does not charge the health window.
// -----------------------------------------------------------------------

#[tokio::test]
async fn a_blackhole_is_unreachable_and_retried() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found — build it first");

    let env = Env::new("lb4");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (_workspace_id, target) = create_ready_capsule_row(&env, &mut conn, &mut next_id, "lb4-workspace").await;

    let relay = Relay::start(env.socket_path.clone()).await;
    relay.blackhole(true);

    let (_woke, wake) = wake_flag_for_test();
    let mut client = FeAttachClient::<DaemonLaneEndpoint>::attach(
        daemon_lane_endpoint(&relay),
        target,
        80,
        24,
        "test-fe".to_string(),
        "test-fe".to_string(),
        None,
        wake,
    )
    .expect("attach starts even against a blackholed relay");

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        client.pump();
        assert!(!client.is_dead(), "must never go terminal while blackholed: {}", client.status_line());
        if client.status_line().contains("daemon unreachable") {
            break;
        }
        assert!(Instant::now() < deadline, "status never reported \"daemon unreachable\" within 3s, got {:?}", client.status_line());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        client.pump();
        assert!(!client.is_dead(), "must never go terminal while blackholed: {}", client.status_line());
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    relay.blackhole(false);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !client.is_checkpointed() {
        client.pump();
        assert!(!client.is_dead(), "died after un-blackholing: {}", client.status_line());
        assert!(Instant::now() < deadline, "never checkpointed after un-blackholing");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    relay.cut();
    let cut_deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < cut_deadline {
        client.pump();
        assert!(!client.is_dead(), "must not die during a 30s tunnel cut: {}", client.status_line());
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    relay.resume();

    kill_supervisor_only(&env.state_root);

    let alive_deadline = Instant::now() + Duration::from_secs(100);
    while Instant::now() < alive_deadline {
        client.pump();
        assert!(!client.is_dead(), "the prior 30s outage must not have been charged to the health window: {}", client.status_line());
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    drop(client);
    relay.cut();
    env.kill_daemon_bounded().await;
}

// -----------------------------------------------------------------------
// (v) A daemon outage past the health window keeps retrying, never
// terminal — recovery is the daemon's own next dial, outstanding input
// survives.
// -----------------------------------------------------------------------

#[tokio::test]
async fn a_daemon_outage_past_the_window_keeps_retrying() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found — build it first");

    let env = Env::new("lb5");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (workspace_id, target) = create_ready_capsule_row(&env, &mut conn, &mut next_id, "lb5-workspace").await;
    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    let row = find_row(&list_payload, &workspace_id).expect("row listed");
    let state_dir = PathBuf::from(row["state_dir"].as_str().expect("state_dir"));

    let relay = Relay::start(env.socket_path.clone()).await;
    let (_woke, wake) = wake_flag_for_test();
    let mut client = FeAttachClient::<DaemonLaneEndpoint>::attach(
        daemon_lane_endpoint(&relay),
        target,
        80,
        24,
        "test-fe".to_string(),
        "test-fe".to_string(),
        None,
        wake,
    )
    .expect("attach");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !client.is_checkpointed() {
        client.pump();
        assert!(!client.is_dead(), "died before a checkpoint: {}", client.status_line());
        assert!(Instant::now() < deadline, "never checkpointed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let marker: &[u8] = b"echo lb5pre\r"; // 12 bytes -- a length unique to this send
    client.send_input(marker);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        client.pump();
        if client.last_input_outcome() == Some(InputOutcome::Recorded) {
            break;
        }
        assert!(!client.is_dead(), "died before its pre-cut outcome: {}", client.status_line());
        assert!(Instant::now() < deadline, "pre-cut input never recorded");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;

    relay.cut();
    let cut_deadline = Instant::now() + Duration::from_secs(130);
    while Instant::now() < cut_deadline {
        client.pump();
        assert!(!client.is_dead(), "a 130s outage must still be retrying, never terminal: {}", client.status_line());
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(client.status_line().contains("unreachable"), "status must show the retry, got {:?}", client.status_line());

    relay.resume();
    // A weak "some content is on screen" proxy races the drop: `queued
    // Input has no live attach connection yet to act on and is dropped`
    // (`wait_for_retry_or_shutdown`'s own documented behavior) would
    // silently eat a `send_input` issued mid-backoff. `status_line() ==
    // "attached"` (the literal text `pump`'s `Checkpoint` arm queues
    // right behind a successfully restored checkpoint) is the one
    // unambiguous "this episode is ready to drive" signal.
    let attached_deadline = Instant::now() + Duration::from_secs(60);
    loop {
        client.pump();
        assert!(!client.is_dead(), "died while resuming: {}", client.status_line());
        if client.status_line() == "attached" {
            break;
        }
        assert!(Instant::now() < attached_deadline, "never resumed after the daemon outage ended (status={})", client.status_line());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let marker2: &[u8] = b"echo lb5post\r"; // 13 bytes -- distinct length
    client.send_input(marker2);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        client.pump();
        if screen_text(&client).contains("lb5post") {
            break;
        }
        assert!(!client.is_dead(), "died before the post-resume input echoed: {}", client.status_line());
        assert!(Instant::now() < deadline, "post-resume input never echoed");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let voyage = tokio::task::spawn_blocking({
        let d = state_dir.clone();
        move || sot_log::supervisor_client::query_status(&d).expect("query_status").0.voyage
    })
    .await
    .unwrap()
    .expect("a ready row has a voyage");
    let outcome = tokio::task::spawn_blocking({
        let d = state_dir.clone();
        let v = voyage.clone();
        move || sot_log::supervisor_client::end_run(&d, &v, "lane bridge test teardown")
    })
    .await
    .unwrap()
    .expect("end_run");
    assert!(
        matches!(
            outcome,
            sot_log::supervisor_client::EndRunOutcome::RecordVerified | sot_log::supervisor_client::EndRunOutcome::RecordClosed
        ),
        "end_run did not verify: {outcome:?}"
    );

    let frames = sealed_frames(&state_dir, &voyage);
    let recorded_once = frames
        .iter()
        .filter(|f| {
            f.class == sot_log::envelope::Class::Input
                && f.source.actor.controller_id.as_deref() == Some("test-fe")
                && f.payload.as_ref().and_then(|p| p.get("length")?.as_u64()).map(|l| l as usize) == Some(marker.len())
        })
        .count();
    assert_eq!(recorded_once, 1, "the pre-cut input must resolve Recorded exactly once in the sealed record");

    drop(client);
    env.kill_daemon_bounded().await;
}

// -----------------------------------------------------------------------
// (vi) A terminal row is terminal after the health window — every dial
// answers lane_absent, no supervise is ever spawned.
// -----------------------------------------------------------------------

#[tokio::test]
async fn a_terminal_row_is_terminal_after_the_window() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found — build it first");

    let env = Env::new("lb6");
    env.seed_default_capsule_toml("claude");
    let fake_claude_dir = env.seed_fake_unlaunchable_claude();
    env.spawn_sotd_with_prepended_path(&fake_claude_dir);
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id += 1;
    let default_row = list_payload["workspaces"]
        .as_array()
        .expect("workspaces array")
        .iter()
        .find(|w| w["is_default"].as_bool() == Some(true))
        .cloned()
        .expect("a default workspace row");
    let default_workspace_id = default_row["workspace_id"].as_str().expect("workspace_id").to_string();
    let default_target = default_row["session_name"].as_str().expect("session_name").to_string();

    let pty_req = serde_json::json!({ "cols": 80, "rows": 24, "user_switch": true, "target": default_target });
    let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    next_id += 1;
    assert_eq!(pty_res.payload["code"], "attach_direct", "pty.open payload: {:?}", pty_res.payload);

    let terminal_deadline = Instant::now() + BOUND;
    loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, &default_workspace_id) {
            if row["phase"].as_str() == Some("terminal") {
                break;
            }
        }
        assert!(Instant::now() < terminal_deadline, "timed out waiting for the unlaunchable-agent row to reach \"terminal\"");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "supervise", &env.state_root);
    // `workspace.list` reports phase "terminal" the moment `sot-capsule
    // supervise` LOGS the transition, which is still up to
    // `TERMINAL_EXIT_GRACE` (2s) before the process actually self-exits
    // -- poll it gone rather than assert instantly, or this races that
    // grace window.
    assert!(poll_until_no_process_matches(&pattern, BOUND), "the row is terminal — its supervise process must exit within the terminal-exit grace period");

    let relay = Relay::start(env.socket_path.clone()).await;
    let (_woke, wake) = wake_flag_for_test();
    let mut client = FeAttachClient::<DaemonLaneEndpoint>::attach(
        daemon_lane_endpoint(&relay),
        default_target,
        80,
        24,
        "test-fe".to_string(),
        "test-fe".to_string(),
        None,
        wake,
    )
    .expect("attach starts even against a terminal row");

    let start = Instant::now();
    let mut last_pgrep_check = Instant::now();
    loop {
        client.pump();
        if client.is_dead() {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(150), "must reach HealthWindowExpired within 150s of a terminal row, status={}", client.status_line());
        if last_pgrep_check.elapsed() >= Duration::from_secs(10) {
            assert!(!any_process_matches(&pattern), "a terminal row must never be resumed — a supervise process appeared");
            last_pgrep_check = Instant::now();
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let elapsed = start.elapsed();
    assert!(elapsed >= Duration::from_secs(120), "went terminal too early ({elapsed:?}) — the health window must run its full 120s");
    assert!(client.status_line().contains("HealthWindowExpired"), "status must name HealthWindowExpired, got {:?}", client.status_line());
    assert!(!any_process_matches(&pattern), "a terminal row must never be resumed");

    drop(client);
    relay.cut();
    env.kill_daemon_bounded().await;
}

