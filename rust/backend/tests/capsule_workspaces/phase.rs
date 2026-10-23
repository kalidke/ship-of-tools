//! The lane-refusal fixture; foreign and unreachable phases; the observer after a supervisor kill.

use super::*;

/// A minimal row-socket FIXTURE standing in for a row of another row number
/// (ADR 0030 §8 decision 31c) — binds the EXACT unix socket path
/// `phase_of`'s own `query_status` will dial for `state_dir`
/// (`sot_log::row::row_socket_path`, the same one this
/// process's own `SOT_RUNTIME_DIR` resolves it to), accepts ONE
/// connection, and writes back `reply_bytes` verbatim before closing.
/// Same-user peer credentials (the SID/`SO_PEERCRED` steps) pass for
/// free: this fixture runs as the test's own process, so the kernel
/// reports it as the caller's own user regardless of what this function's
/// code does — no second build, no real supervisor, and no compile step
/// are needed to prove either half of the "answered but ___" split;
/// only the one reply a real peer would send. Two callers below use this
/// with two different `reply_bytes`: a hello reply declaring another row
/// number proves "foreign"; anything else well-formed-but-wrong proves
/// "unreachable" stays unreachable.
#[cfg(target_os = "linux")]
fn spawn_lane_refusal_fixture(state_dir: &Path, reply_bytes: Vec<u8>) -> std::thread::JoinHandle<()> {
    let h = sot_platform::host::state_dir::state_dir_hash(state_dir);
    let path = sot_log::row::row_socket_path(&h).expect("row socket path");
    let _ = std::fs::remove_file(&path);
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind the fixture supervisor socket");
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            use std::io::Write;
            let _ = stream.write_all(&reply_bytes);
            // Hold the connection open briefly so the client's own read
            // has time to land before this fixture (and its listener)
            // drop -- a one-shot fixture, not a persistent server.
            std::thread::sleep(Duration::from_millis(500));
        }
    })
}

/// ADR 0030 §8 decision 31c (cross-referenced as ADR 0043 decision 31;
/// the gate itself is superseded by ADR 0045 decision 7 -- `proto`, not
/// build): `phase_of` reports `"foreign"` for a capsule row whose
/// row answered with another row number (typed as
/// `sot_log::Error::VersionSkew`, never a text match). Reproduces the
/// field incident's OBSERVABLE shape (a row an operator finds already
/// held by a foreign lane, not one this daemon started that way -- this
/// daemon would never spawn one itself) using
/// [`spawn_lane_refusal_fixture`] in place of a real foreign peer.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn phase_reports_foreign_for_a_version_skew_refusal() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("foreign");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let (workspace_id, state_dir_path) =
        create_ready_workspace_then_stop_its_supervisor(&env, &mut conn, &mut next_id, "foreign-workspace").await;

    // Bind the fixture where the (now-stopped) real supervisor was, and reply
    // with a hello that declares a newer row number: the one reply a real
    // row of another row number would send.
    let hello = sot_protocol::Frame::res(
        1,
        sot_log::row::OP_HELLO,
        serde_json::json!({"pid": 1, "created": 1, "phase": "ready", "run": "", "row": sot_log::row::ROW_PROTOCOL + 1}),
    );
    let mut reply = serde_json::to_vec(&hello).expect("a hello reply serializes");
    reply.push(b'\n');
    let fixture = spawn_lane_refusal_fixture(&state_dir_path, reply);

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "foreign", BOUND.max(Duration::from_secs(60))).await;

    // Teardown: the surviving leg (the platform shell the ORIGINAL real
    // supervisor spawned) is caught by `Env`'s own leg sweep, anchored on
    // `env`'s own `state_root` — the fixture thread holds no leg of its
    // own and exits on its own once its one connection closes.
    let _ = fixture.join();
    env.kill_daemon_bounded().await;
}

/// Sibling of [`phase_reports_foreign_for_a_version_skew_refusal`]: a lane
/// that answers but with a MALFORMED/wrong-shape reply (never a
/// `Refused { VersionSkew }`) must stay `"unreachable"` — the exact
/// distinction ADR 0030 §8 decision 31c's typed check exists to draw
/// (Codex review: a broader `Foreign` classification, text-matched, would
/// have reported "foreign" here too).
#[tokio::test]
#[cfg(target_os = "linux")]
async fn phase_stays_unreachable_for_a_malformed_reply() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("malformed");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let (workspace_id, state_dir_path) =
        create_ready_workspace_then_stop_its_supervisor(&env, &mut conn, &mut next_id, "malformed-workspace").await;

    let fixture = spawn_lane_refusal_fixture(&state_dir_path, b"not a valid supervisor-lane frame".to_vec());

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "unreachable", BOUND.max(Duration::from_secs(60))).await;

    // ADR 0046 decision 2: workspace.list reads pure memory -- even with the row genuinely unreachable, back-to-back
    // calls keep reading it from memory. (Nothing restarts this row: its supervisor was stopped, which exits 0.)
    let log_path_for_list_check = env.state_root.join("sot").join("sotd.log");
    let unreachable_lines_before = count_log_occurrences(&log_path_for_list_check, "supervisor lane unreachable");
    for _ in 0..10 {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        assert_eq!(
            find_row(&payload, &workspace_id).and_then(|r| r["phase"].as_str().map(str::to_string)),
            Some("unreachable".to_string()),
            "the row must keep reading \"unreachable\" from memory across the burst"
        );
    }
    // The burst must not itself provoke a new lane probe; +2 slack covers the observer's own redial landing during the window.
    let unreachable_lines_after = count_log_occurrences(&log_path_for_list_check, "supervisor lane unreachable");
    assert!(
        unreachable_lines_after <= unreachable_lines_before + 2,
        "workspace.list must never itself probe the lane: {unreachable_lines_before} -> {unreachable_lines_after} \
         occurrences of the phase_of debug line across a 10-call burst"
    );

    let _ = fixture.join();
    env.kill_daemon_bounded().await;
}

/// An adopted supervisor that is killed is restarted by the daemon that adopted it and comes back `ready` under a new
/// pid; one that is stopped (exit 0, a stop is not a crash) stays down, with no supervisor process, for five seconds.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn a_killed_adopted_supervisor_is_restarted_and_a_stopped_one_is_not() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("killstop");
    let (mut conn, mut next_id, workspace_id, state_dir) =
        super::resume::ready_row_adopted_by_a_new_daemon(&env, &[], &[SHORT_BACKOFF], "killstop-workspace").await;
    let first = super::resume::supervisor_pid(&state_dir).await.expect("the adopted supervisor answers");

    kill_supervisor(&env.state_root);
    let deadline = Instant::now() + Duration::from_secs(45);
    let second = loop {
        let payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        next_id += 1;
        let ready = find_row(&payload, &workspace_id).is_some_and(|row| row["phase"] == "ready");
        if let Some(pid) = super::resume::supervisor_pid(&state_dir).await.filter(|pid| *pid != first) {
            if ready {
                break pid;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the killed adopted supervisor was not restarted: {:?}",
            find_row(&payload, &workspace_id)
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_ne!(first, second);

    tokio::task::spawn_blocking({
        let dir = state_dir.clone();
        move || sot_log::attach_client::supervisor_client::stop(&dir).expect("stop the supervisor")
    })
    .await
    .unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        assert!(own_supervisor_pids(&env.state_root).is_empty(), "a stopped supervisor was restarted");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    env.kill_daemon_bounded().await;
}

/// A supervisor the daemon adopted at boot (not a replacement the daemon launched) that is stopped stays down: the
/// stop exits 0, which nothing restarts, and the row reads `unreachable` because nothing answers. Five seconds with no
/// supervisor process.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn a_stopped_adopted_supervisor_is_not_restarted_and_reads_unreachable() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("stopadopted");
    let (mut conn, mut next_id, workspace_id, state_dir) =
        super::resume::ready_row_adopted_by_a_new_daemon(&env, &[], &[SHORT_BACKOFF], "stopadopted-workspace").await;
    assert!(super::resume::supervisor_pid(&state_dir).await.is_some(), "the adopted supervisor answers");

    stop_supervisor(&state_dir).await;
    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "unreachable", BOUND.max(Duration::from_secs(30))).await;
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        assert!(own_supervisor_pids(&env.state_root).is_empty(), "a stopped adopted supervisor was restarted");
        let payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        next_id += 1;
        assert_eq!(
            find_row(&payload, &workspace_id).map(|row| row["phase"].clone()),
            Some(serde_json::json!("unreachable")),
            "the stopped adopted row stopped reading unreachable"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    env.kill_daemon_bounded().await;
}
