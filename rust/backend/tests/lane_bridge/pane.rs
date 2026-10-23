//! A busy pane over a slow link, and a headless write while a bridged client drives.

use super::*;

// -----------------------------------------------------------------------
// (vii) A busy pane over a throttled relay converges — recording never
// waits on the slow subscriber.
// -----------------------------------------------------------------------

#[tokio::test]
async fn a_busy_pane_over_a_slow_link_converges() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found — build it first");

    let env = Env::new("lb7");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (workspace_id, target) = create_ready_capsule_row(&env, &mut conn, &mut next_id, "lb7-workspace").await;
    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    let row = find_row(&list_payload, &workspace_id).expect("row listed");
    let state_dir = PathBuf::from(row["state_dir"].as_str().expect("state_dir"));

    let relay = Relay::start(env.socket_path.clone()).await;
    relay.throttle(256 * 1024);

    let (_woke, wake) = wake_flag_for_test();
    let mut client = FeAttachClient::attach(
        daemon_lane_endpoint(&relay),
        target,
        80,
        24,
        wake,
    )
    .expect("attach over a throttled relay");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !client.is_checkpointed() {
        client.pump();
        assert!(!client.is_dead(), "died before a checkpoint over the throttled relay: {}", client.status_line());
        assert!(Instant::now() < deadline, "never checkpointed over the throttled relay");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    client.send_input(b"yes\r");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        client.pump();
        if client.last_input_outcome() == Some(InputOutcome::Recorded) {
            break;
        }
        assert!(!client.is_dead(), "died before \"yes\" recorded: {}", client.status_line());
        assert!(Instant::now() < deadline, "\"yes\" was never recorded");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let initial_screen = screen_modified(&state_dir);

    // The row socket resyncs a viewer that falls 256 output frames behind and cuts one that stalls writing, but this is
    // neither: this relay's own throttle paces writes without ever refusing to drain its Unix-socket read, so the
    // pipe's natural backpressure keeps the supervisor's per-connection unsent queue bounded to roughly one
    // in-flight chunk. A genuinely STALLED reader is already covered by
    // [`a_blackhole_is_unreachable_and_retried`] and the `cut()` cases;
    // this test verifies decision 11's actually-observable core claim for
    // a merely slow one instead: output handling never blocks on it, and the
    // client stays alive and converges once the flood ends.
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        client.pump();
        assert!(!client.is_dead(), "must not go terminal under a busy pane over a slow link: {}", client.status_line());
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let kept_screen = screen_modified(&state_dir);
    assert!(
        kept_screen.is_some() && kept_screen > initial_screen,
        "the capsule must keep handling the flood and keep its screen file: {initial_screen:?} -> {kept_screen:?}"
    );

    relay.throttle(0);
    // The row resyncs a viewer that falls behind and never cuts it, so one Ctrl-C is answered, written once, and ends the
    // flood: no second key.
    client.send_input(&[0x03]);
    let deadline = Instant::now() + CHECKPOINT_TRANSFER_BUDGET;
    while client.last_input_outcome() != Some(InputOutcome::Recorded) {
        client.pump();
        assert_ne!(client.last_input_outcome(), Some(InputOutcome::DeliveryUnknown), "the Ctrl-C's answer was lost: {}", client.status_line());
        assert!(!client.is_dead(), "died while converging after the flood: {}", client.status_line());
        assert!(Instant::now() < deadline, "the Ctrl-C was never answered within the checkpoint transfer budget");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(client.recorded_bytes(), 5, "\"yes\\r\" and one Ctrl-C written, each once");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = screen_modified(&state_dir);
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let now = screen_modified(&state_dir);
        if now == seen {
            break;
        }
        assert!(Instant::now() < deadline, "the flood did not stop after the one Ctrl-C");
        seen = now;
    }

    drop(client);
    relay.cut();
    env.kill_daemon_bounded().await;
}

// -----------------------------------------------------------------------
// (viii) `rust/log/tests/fe_client/`'s own pen-contention proof,
// repeated through the bridge: a headless write via the daemon's OWN
// `pty.input` demotes the bridged FE; its next input retakes; watcher
// screen reads never take.
// -----------------------------------------------------------------------

#[tokio::test]
#[allow(clippy::too_many_lines, reason = "one test scenario: a headless write demotes a driving bridged client without duplicating input")]
async fn headless_write_while_a_bridged_client_is_driving_demotes_it_without_duplicating_input() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found — build it first");

    let env = Env::new("lb8");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (workspace_id, target) = create_ready_capsule_row(&env, &mut conn, &mut next_id, "lb8-workspace").await;
    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id += 1;
    let row = find_row(&list_payload, &workspace_id).expect("row listed");
    let state_dir = PathBuf::from(row["state_dir"].as_str().expect("state_dir"));

    let relay = Relay::start(env.socket_path.clone()).await;
    let (_woke, wake) = wake_flag_for_test();
    let mut driver = FeAttachClient::attach(
        daemon_lane_endpoint(&relay),
        target,
        80,
        24,
        wake,
    )
    .expect("attach the driving client through the bridge");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !driver.is_checkpointed() {
        driver.pump();
        assert!(!driver.is_dead(), "driver died before a checkpoint: {}", driver.status_line());
        assert!(Instant::now() < deadline, "driver never checkpointed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // A watcher read BEFORE any write must not take the pen.
    let pre_screen = call(&mut conn, next_id, op::PTY_SCREEN, serde_json::json!({ "workspace_id": workspace_id })).await;
    next_id += 1;
    assert!(pre_screen.payload.get("error").is_none(), "pty.screen before any write failed: {:?}", pre_screen.payload);

    driver.send_input(b"echo A1\r\n");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        driver.pump();
        if screen_text(&driver).contains("A1") {
            break;
        }
        assert!(!driver.is_dead(), "driver died before its first input echoed: {}", driver.status_line());
        assert!(Instant::now() < deadline, "driver's first input never echoed");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // A watcher read WHILE the bridged driver holds the pen must not
    // itself take it either.
    let mid_screen = call(&mut conn, next_id, op::PTY_SCREEN, serde_json::json!({ "workspace_id": workspace_id })).await;
    next_id += 1;
    assert!(mid_screen.payload.get("error").is_none(), "pty.screen while driving failed: {:?}", mid_screen.payload);

    // The daemon's OWN headless write (never a second `FeAttachClient`)
    // lands on the same row and must demote the bridged driver.
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    let headless_text = "echo HEADLESSB";
    let input_req = serde_json::json!({
        "workspace_id": workspace_id,
        "data_b64": STANDARD.encode(headless_text.as_bytes()),
        "enter": true,
        "origin": "lb8-headless",
    });
    let input_res = call(&mut conn, next_id, op::PTY_INPUT, input_req).await;
    assert!(input_res.payload.get("error").is_none(), "the daemon's own headless pty.input failed: {:?}", input_res.payload);
    assert_eq!(input_res.payload["ok"], true, "headless pty.input: {:?}", input_res.payload);

    // The driver's own NEXT input: its worker sees refused_stale and
    // retakes automatically (ruling (c)) — invisible to this test except
    // for the eventual echo.
    driver.send_input(b"echo A2RETAKE\r\n");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        driver.pump();
        if screen_text(&driver).contains("A2RETAKE") {
            break;
        }
        assert!(!driver.is_dead(), "driver died before its retake echoed: {}", driver.status_line());
        assert!(Instant::now() < deadline, "driver's post-demotion retake never delivered");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Each marker reached the shell exactly once: a shell shows `echo X` and its output `X`, so once is two
    // occurrences on the screen, and a double forward (the headless write duplicated, or the driver's refused attempt
    // and its retry both written) four.
    let text = screen_text(&driver);
    assert_eq!(text.matches("HEADLESSB").count(), 2, "the headless write must reach the shell exactly once: {text:?}");
    assert_eq!(text.matches("A2RETAKE").count(), 2, "the driver's retaken keystroke must reach the shell exactly once: {text:?}");

    drop(driver);

    env.kill_daemon_bounded().await;
}

