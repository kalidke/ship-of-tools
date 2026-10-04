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

    let voyage = tokio::task::spawn_blocking({
        let d = state_dir.clone();
        move || sot_log::supervisor_client::query_status(&d).expect("query_status").0.voyage
    })
    .await
    .unwrap()
    .expect("a ready row has a voyage");
    let seg_dir = state_dir.join("voyages").join(&voyage).join("seg");
    let initial_bytes = dir_bytes(&seg_dir);

    // `attach_proto.rs`'s own `WATCHER_LIVE_QUEUE_BUDGET_BYTES` (4 MiB)
    // eviction exists and is wired (confirmed by reading that source: it
    // closes a watcher whose OWN unsent queue overflows 4 MiB) but is a
    // stalled-reader safety valve, not a slow-but-still-draining one: this
    // relay's own throttle paces writes without ever refusing to drain its
    // Unix-socket read, so the pipe's natural backpressure keeps the
    // supervisor's per-connection unsent queue bounded to roughly one
    // in-flight chunk rather than ever reaching 4 MiB -- empirically
    // confirmed (a 20s run at 256 KiB/s never logged `QueueOverflow`,
    // only an unrelated `MgmtIdleTimeout` on the short-lived status probe
    // connection above). A genuinely STALLED reader is already covered by
    // [`a_blackhole_is_unreachable_and_retried`] and the `cut()` cases;
    // this test verifies decision 11's actually-observable core claim for
    // a merely slow one instead: recording never blocks on it, and the
    // client stays alive and converges once the flood ends.
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        client.pump();
        assert!(!client.is_dead(), "must not go terminal under a busy pane over a slow link: {}", client.status_line());
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let grown_bytes = dir_bytes(&seg_dir);
    assert!(grown_bytes > initial_bytes, "the record must keep growing under the flood: {initial_bytes} -> {grown_bytes}");

    relay.throttle(0);
    client.send_input(&[0x03]);
    let deadline = Instant::now() + CHECKPOINT_TRANSFER_BUDGET;
    loop {
        client.pump();
        if client.last_input_outcome() == Some(InputOutcome::Recorded)
            && !client.status_line().contains("unreachable")
            && !client.status_line().contains("connecting")
        {
            break;
        }
        assert!(!client.is_dead(), "died while converging after the flood: {}", client.status_line());
        assert!(Instant::now() < deadline, "never converged after the flood within the checkpoint transfer budget");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    drop(client);
    relay.cut();
    env.kill_daemon_bounded().await;
}

// -----------------------------------------------------------------------
// (viii) `rust/log/tests/fe_client.rs`'s own pen-contention proof,
// repeated through the bridge: a headless write via the daemon's OWN
// `pty.input` demotes the bridged FE; its next input retakes; watcher
// screen reads never take.
// -----------------------------------------------------------------------

#[tokio::test]
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
    let mut driver = FeAttachClient::<DaemonLaneEndpoint>::attach(
        daemon_lane_endpoint(&relay),
        target,
        80,
        24,
        "lb8-driver".to_string(),
        "lb8-driver".to_string(),
        None,
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

    drop(driver);

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
    let count_for = |cid: &str| {
        frames.iter().filter(|f| f.class == sot_log::envelope::Class::Input && f.source.actor.controller_id.as_deref() == Some(cid)).count()
    };
    // TWO frames, not one: write_and_enter writes text and Enter as separate
    // wire ops, each its own sealed frame -- pinned by LENGTH, not just
    // count, so "text twice" or "Enter twice" (same total of 2) still fails.
    let headless_len = |want: usize| {
        frames
            .iter()
            .filter(|f| {
                f.class == sot_log::envelope::Class::Input
                    && f.source.actor.controller_id.as_deref() == Some("lb8-headless")
                    && f.payload.as_ref().and_then(|p| p.get("length")?.as_u64()).map(|l| l as usize) == Some(want)
            })
            .count()
    };
    assert_eq!(headless_len(headless_text.len()), 1, "exactly one headless frame must carry the text's own length");
    assert_eq!(headless_len(1), 1, "exactly one headless frame must carry length 1, the lone Enter byte");
    assert_eq!(count_for("lb8-headless"), 2, "the headless write must appear as exactly two frames (text, then Enter), never duplicated");
    assert_eq!(count_for("lb8-driver"), 3, "the driver's record: A1 (clean) + A2RETAKE's refused-stale attempt + A2RETAKE's successful retry");
    let refused_stale_count = frames
        .iter()
        .filter(|f| {
            f.class == sot_log::envelope::Class::Lifecycle
                && f.source.actor.controller_id.as_deref() == Some("lb8-driver")
                && f.payload.as_ref().and_then(|p| p.get("fact")?.get("fact")?.as_str()) == Some("refused_stale_epoch")
        })
        .count();
    assert_eq!(refused_stale_count, 1, "exactly ONE of the driver's attempts must be the refused-stale one the demotion causes");

    env.kill_daemon_bounded().await;
}

