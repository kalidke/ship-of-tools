//! `pty.input` and `pty.screen` on a real capsule row.

use super::*;

/// ADR 0042 amendment (2026-09-07), "a session types into and reads a
/// sibling row": the daemon-side proof that `pty.input`/`pty.screen`
/// actually reach a real capsule row over the wire, end to end — the
/// `sot_log::attach_client::client` mechanics themselves are proven directly in
/// `rust/log/tests/fe_client/`'s own `headless_*` tests; this test's
/// job is only "does the WIRE OP reach that machinery and answer
/// correctly for a real daemon."
#[tokio::test]
async fn capsule_pty_input_and_screen_reach_a_real_row_and_leave_the_lane_clean() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("pis");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "pis-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();

    let list_deadline = Instant::now() + BOUND.max(Duration::from_secs(90));
    loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, &workspace_id) {
            if row["phase"].as_str() == Some("ready") {
                break;
            }
        }
        assert!(Instant::now() < list_deadline, "timed out waiting for the new capsule workspace to reach \"ready\"");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // `pty.screen` BEFORE any write: must succeed (a watcher attach, not
    // a refusal) and must NOT take the pen — proven below once a real
    // driving client's first input succeeds without contention.
    let pre_screen_req = serde_json::json!({ "workspace_id": workspace_id });
    let pre_screen_res = call(&mut conn, next_id, op::PTY_SCREEN, pre_screen_req).await;
    next_id += 1;
    assert!(
        pre_screen_res.payload.get("error").is_none(),
        "pty.screen before any write failed: {:?}",
        pre_screen_res.payload
    );
    assert_eq!(pre_screen_res.payload["runtime"], "capsule");

    // `pty.input`, base64("echo sot-lu6c-marker") + a separate Enter.
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    let text = "echo sot-lu6c-marker";
    let input_req = serde_json::json!({
        "workspace_id": workspace_id,
        "data_b64": STANDARD.encode(text.as_bytes()),
        "enter": true,
        "origin": "lu6c-test",
    });
    let input_res = call(&mut conn, next_id, op::PTY_INPUT, input_req).await;
    next_id += 1;
    assert!(input_res.payload.get("error").is_none(), "pty.input failed: {:?}", input_res.payload);
    assert_eq!(input_res.payload["ok"], true);
    assert_eq!(input_res.payload["runtime"], "capsule");
    assert_eq!(input_res.payload["bytes"].as_u64(), Some(text.len() as u64));

    // Poll `pty.screen` until the echoed marker shows up.
    let screen_deadline = Instant::now() + Duration::from_secs(10);
    let final_screen = loop {
        let id = next_id;
        next_id += 1;
        let screen_req = serde_json::json!({ "workspace_id": workspace_id });
        let res = call(&mut conn, id, op::PTY_SCREEN, screen_req).await;
        assert!(res.payload.get("error").is_none(), "pty.screen failed: {:?}", res.payload);
        let lines: Vec<String> = res.payload["lines"]
            .as_array()
            .expect("lines array")
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect();
        if lines.iter().any(|l| l.contains("sot-lu6c-marker")) {
            break res.payload;
        }
        assert!(
            Instant::now() < screen_deadline,
            "timed out waiting for the echoed marker to appear on screen; last lines: {lines:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert!(final_screen["cols"].as_u64().unwrap_or(0) > 0, "cols must be the capsule's real geometry");
    assert!(final_screen["rows"].as_u64().unwrap_or(0) > 0, "rows must be the capsule's real geometry");
    assert!(final_screen["cursor"].is_object(), "cursor must be Some for a healthy row: {final_screen:?}");

    // The daemon's own headless client must have left the lane CLEAN:
    // a fresh `FeAttachClient` from the test itself reaches its own
    // checkpoint (proving the row is not wedged), and a real keystroke
    // from it is accepted WITHOUT any contention artifact left behind —
    // proving the earlier `pty.screen` (a watcher) never took the pen
    // either.
    let state_dir = crate::state_dir_from_list(&mut conn, &mut next_id, &workspace_id).await;
    let (_woke, wake) = wake_flag_for_test();
    let mut test_client = sot_log::attach_client::client::FeAttachClient::<sot_log::lane::client::PlatformEndpoint>::attach(
        sot_log::lane::client::PlatformEndpoint::default(),
        sot_log::host::state_dir::state_dir_hash(&state_dir),
        80,
        24,
        "lu6c-test-post-check".to_string(),
        "lu6c-test-post-check".to_string(),
        None,
        wake,
    )
    .expect("attach a fresh FeAttachClient after the daemon's own headless ops");
    let checkpoint_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        test_client.pump();
        if test_client.is_checkpointed() {
            break;
        }
        assert!(!test_client.is_dead(), "post-check client died before a checkpoint: {}", test_client.status_line());
        assert!(Instant::now() < checkpoint_deadline, "post-check client never reached a checkpoint — the lane may be wedged");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    test_client.send_input(b"echo sot-lu6c-postcheck-marker\r\n");
    let took_pen_deadline = Instant::now() + Duration::from_secs(30);
    let got_marker = loop {
        test_client.pump();
        let (rows, cols) = test_client.screen().size();
        let mut text = String::new();
        for r in 0..rows {
            for c in 0..cols {
                if let Some(cell) = test_client.screen().cell(r, c) {
                    text.push_str(cell.contents());
                }
            }
        }
        if text.contains("sot-lu6c-postcheck-marker") {
            break true;
        }
        if Instant::now() >= took_pen_deadline {
            break false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(
        got_marker,
        "the pen was not free for a fresh client after the daemon's own pty.input/pty.screen ops (dead={}, status={})",
        test_client.is_dead(),
        test_client.status_line()
    );
    drop(test_client);

    env.kill_daemon_bounded().await;
}

/// Codex review round, 2026-09-14: `write_and_enter` must never refuse a
/// write just because the screen already has visible text on it — both
/// claude's and codex's TUIs (codex's own status line reads like
/// "model-x high · ~/project") always draw persistent chrome, and an
/// EARLIER busy check keyed on exactly this screen shape was deleted for
/// that reason (see `write_and_enter`'s own doc). Sets a custom PS1
/// mimicking that status line, confirms it is visible, then sends a
/// SECOND pty.input against that same non-blank screen and asserts it
/// still delivers (`enter: sent`) and actually runs.
/// Unix only: the status line is set through `PS1` in a POSIX shell; the
/// Windows capsule shell is cmd.exe, which has no such prompt variable.
#[cfg(unix)]
#[tokio::test]
async fn capsule_pty_input_lands_despite_a_nonblank_status_line_on_screen() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found next to sotd[.exe] — build it first");

    let env = Env::new("stln");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "stln-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();

    let list_deadline = Instant::now() + BOUND.max(Duration::from_secs(90));
    loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, &workspace_id) {
            if row["phase"].as_str() == Some("ready") {
                break;
            }
        }
        assert!(Instant::now() < list_deadline, "timed out waiting for the new capsule workspace to reach \"ready\"");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    let status_line = "model-x high \u{b7} ~/project$ ";
    let ps1_cmd = format!("PS1='{status_line}'");
    let ps1_req = serde_json::json!({
        "workspace_id": workspace_id,
        "data_b64": STANDARD.encode(ps1_cmd.as_bytes()),
        "enter": true,
    });
    let ps1_res = call(&mut conn, next_id, op::PTY_INPUT, ps1_req).await;
    next_id += 1;
    assert!(ps1_res.payload.get("error").is_none(), "setting PS1 failed: {:?}", ps1_res.payload);

    // Poll until the status line is actually visible — the precondition
    // this test exists to exercise.
    let prompt_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let id = next_id;
        next_id += 1;
        let screen_req = serde_json::json!({ "workspace_id": workspace_id });
        let res = call(&mut conn, id, op::PTY_SCREEN, screen_req).await;
        let lines: Vec<String> = res.payload["lines"]
            .as_array()
            .expect("lines array")
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect();
        if lines.iter().any(|l| l.contains(status_line.trim_end())) {
            break;
        }
        assert!(Instant::now() < prompt_deadline, "the status-line prompt never became visible; last lines: {lines:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // The real proof: a SECOND write against a screen that is now
    // non-blank (the status line itself) must still land and submit.
    let marker = "echo sot-lu6c-statusline-marker";
    let input_req = serde_json::json!({
        "workspace_id": workspace_id,
        "data_b64": STANDARD.encode(marker.as_bytes()),
        "enter": true,
    });
    let input_res = call(&mut conn, next_id, op::PTY_INPUT, input_req).await;
    next_id += 1;
    assert!(input_res.payload.get("error").is_none(), "pty.input failed: {:?}", input_res.payload);
    assert_eq!(input_res.payload["ok"], true);
    assert_eq!(
        input_res.payload["enter"], "sent",
        "Enter must be sent even though the bottom row was non-blank before this write: {:?}",
        input_res.payload
    );

    let screen_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let id = next_id;
        next_id += 1;
        let screen_req = serde_json::json!({ "workspace_id": workspace_id });
        let res = call(&mut conn, id, op::PTY_SCREEN, screen_req).await;
        let lines: Vec<String> = res.payload["lines"]
            .as_array()
            .expect("lines array")
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect();
        if lines.iter().any(|l| l.contains("sot-lu6c-statusline-marker")) {
            break;
        }
        assert!(Instant::now() < screen_deadline, "the marker command never ran; last lines: {lines:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    env.kill_daemon_bounded().await;
}

/// Round-5 (Opus review) ruling 1's core: once the text lands,
/// `write_and_enter` answers `Ok` with `enter: not_sent` when the take
/// epoch changes before the Enter write, never an `Err` -- a stolen pen
/// must never look like "not delivered, retype me." A SECOND, real
/// headless `FeAttachClient` (a different controller, pre-checkpointed so
/// its own send fires near-instantly) steals the pen while the daemon's
/// own write is still in its post-text pacing wait.
#[tokio::test]
async fn capsule_pty_input_enter_not_sent_on_a_pen_steal_between_text_and_enter() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found next to sotd[.exe] — build it first");

    let env = Env::new("steal");
    // `SOT_TEST_PACING_HOLD`: `write_and_enter`'s own pacing wait always
    // runs its full 3s bound instead of breaking early on a quiet
    // screen — deterministic, not a ticker job racing terminal output
    // batching (that raced 1 run in ~7: the pacing wait sometimes broke
    // "quiet" and sent Enter before this test's steal even fired,
    // proven by an instrumented timeline, not a guess).
    env.spawn_sotd_with_env(&[("SOT_TEST_PACING_HOLD", "1")]);
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "steal-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();

    let list_deadline = Instant::now() + BOUND.max(Duration::from_secs(90));
    loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, &workspace_id) {
            if row["phase"].as_str() == Some("ready") {
                break;
            }
        }
        assert!(Instant::now() < list_deadline, "timed out waiting for the new capsule workspace to reach \"ready\"");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let state_dir = state_dir_from_list(&mut conn, &mut next_id, &workspace_id).await;

    let mut stealer = sot_log::attach_client::client::FeAttachClient::<sot_log::lane::client::PlatformEndpoint>::attach_headless(
        sot_log::lane::client::PlatformEndpoint::default(),
        sot_log::host::state_dir::state_dir_hash(&state_dir),
        "lu7-stealer".to_string(),
    )
    .expect("attach the stealer client");
    let checkpoint_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        stealer.pump();
        if stealer.is_checkpointed() {
            break;
        }
        assert!(Instant::now() < checkpoint_deadline, "stealer never checkpointed: {}", stealer.status_line());
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;

    // No ticker job needed: `SOT_TEST_PACING_HOLD` (set on `env.spawn_sotd_with_env`
    // above) makes write_and_enter's own pacing wait run its full 3s bound
    // unconditionally, so the steal below gets that whole window regardless
    // of whether the screen happens to hold still.
    let marker = "echo sot-lu7-steal-marker";
    let input_req = serde_json::json!({
        "workspace_id": workspace_id,
        "data_b64": STANDARD.encode(marker.as_bytes()),
        "enter": true,
    });

    let (input_res, _) = tokio::join!(
        call(&mut conn, next_id, op::PTY_INPUT, input_req),
        async {
            // `SOT_TEST_PACING_HOLD` runs pacing to its full 3s bound, so
            // any delay well short of that lands before Enter is attempted.
            tokio::time::sleep(Duration::from_millis(800)).await;
            stealer.send_input(b"x");
            for _ in 0..50 {
                stealer.pump();
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    );

    assert!(
        input_res.payload.get("error").is_none(),
        "pty.input must still answer Ok once the text landed, never Err: {:?}",
        input_res.payload
    );
    assert_eq!(input_res.payload["ok"], true);
    assert_eq!(
        input_res.payload["enter"], "not_sent",
        "a pen steal between the text write and the Enter write must report enter:not_sent: {:?}",
        input_res.payload
    );

    drop(stealer);
    env.kill_daemon_bounded().await;
}

/// Local copy of `rust/log/tests/fe_client/`'s own `wake_flag` helper (a separate test
/// binary; not worth a shared dependency for four lines).
fn wake_flag_for_test() -> (std::sync::Arc<std::sync::atomic::AtomicBool>, Box<dyn Fn() + Send + 'static>) {
    let woke = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let woke2 = std::sync::Arc::clone(&woke);
    (woke, Box::new(move || woke2.store(true, std::sync::atomic::Ordering::Relaxed)))
}
