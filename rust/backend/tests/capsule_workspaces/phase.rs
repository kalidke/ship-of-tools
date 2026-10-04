//! The lane-refusal fixture; foreign and unreachable phases; the observer after a supervisor kill.

use super::*;

/// A minimal lane-refusal FIXTURE standing in for a supervisor of another
/// build (ADR 0030 §8 decision 31c) — binds the EXACT unix socket path
/// `phase_of`'s own `query_status` will dial for `state_dir`
/// (`sot_log::socket_unix::supervisor_socket_path`, the same one this
/// process's own `SOT_RUNTIME_DIR` resolves it to), accepts ONE
/// connection, and writes back `reply_bytes` verbatim before closing.
/// Same-user peer credentials (the SID/`SO_PEERCRED` steps) pass for
/// free: this fixture runs as the test's own process, so the kernel
/// reports it as the caller's own user regardless of what this function's
/// code does — no second build, no real supervisor, and no compile step
/// are needed to prove either half of the "answered but ___" split;
/// only the one reply a real peer would send. Two callers below use this
/// with two different `reply_bytes`: an actual `Refused { VersionSkew }`
/// encoding proves "foreign"; anything else well-formed-but-wrong proves
/// "unreachable" stays unreachable.
#[cfg(target_os = "linux")]
fn spawn_lane_refusal_fixture(state_dir: &Path, reply_bytes: Vec<u8>) -> std::thread::JoinHandle<()> {
    let h = sot_log::state_dir::state_dir_hash(state_dir);
    let path = sot_log::socket_unix::supervisor_socket_path(&h).expect("supervisor socket path");
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
/// supervisor lane answered but refused this daemon's protocol (typed as
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

    // Bind the fixture where the (now-stopped) real supervisor was, and
    // reply with the ACTUAL wire encoding of `Refused { VersionSkew }` —
    // the one reply a real supervisor of another build would send.
    let reply = sot_log::wire::encode_supervisor_reply(&sot_log::wire::SupervisorReply::Refused {
        reason: sot_log::wire::SupervisorRefusedReason::VersionSkew,
    })
    .expect("Refused encodes unconditionally");
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

    let _ = fixture.join();
    env.kill_daemon_bounded().await;
}

/// Kills only the supervisor authority (leg survives); adopted first so no
/// watchdog exists, proving the observer alone carries ready -> unreachable -> ready.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn capsule_observer_reports_unreachable_after_a_bare_supervisor_kill_then_pty_open_recovers_it() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("obskill");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "obskill-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let target = create_res.payload["session_name"].as_str().expect("session_name").to_string();

    let list_deadline = Instant::now() + BOUND.max(Duration::from_secs(90));
    let state_dir = loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, &workspace_id) {
            if let (Some(sd), Some("ready")) = (row["state_dir"].as_str(), row["phase"].as_str()) {
                break sd.to_string();
            }
        }
        assert!(Instant::now() < list_deadline, "timed out waiting for the new capsule workspace to reach \"ready\"");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let state_dir_path = PathBuf::from(&state_dir);

    let leg_before = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::supervisor_client::query_status(&dir).expect("query_status before killing the supervisor").0.leg
    })
    .await
    .unwrap()
    .expect("a ready capsule has a leg");

    // Adopts first, stripping this lifetime's own watchdog.
    let (mut conn, mut next_id) = restart_daemon_and_prove_adoption(
        &env,
        conn,
        &workspace_id,
        &state_dir,
        &state_dir_path,
        leg_before,
        AuthorityAtRestart::Alive,
    )
    .await;

    kill_supervisor_only(&env.state_root);

    // No watchdog exists (adopted, not spawned); killing just the authority
    // never marks the row terminal along the way.
    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "unreachable", BOUND.max(Duration::from_secs(30))).await;

    // ADR 0046 decision 2: workspace.list reads pure memory -- even with the
    // row genuinely unreachable, back-to-back calls keep reading it from
    // memory.
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
    // The burst must not itself provoke a new lane probe; +2 slack covers
    // the observer's own background cadence landing during the window.
    let unreachable_lines_after = count_log_occurrences(&log_path_for_list_check, "supervisor lane unreachable");
    assert!(
        unreachable_lines_after <= unreachable_lines_before + 2,
        "workspace.list must never itself probe the lane: {unreachable_lines_before} -> {unreachable_lines_after} \
         occurrences of the phase_of debug line across a 10-call burst"
    );

    // Resumes via `ensure_started`, answering attach_direct regardless of phase.
    let pty_req = serde_json::json!({ "cols": 80, "rows": 24, "user_switch": true, "target": target });
    let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    next_id += 1;
    assert_eq!(pty_res.payload["code"], "attach_direct", "pty.open payload: {:?}", pty_res.payload);

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND.max(Duration::from_secs(30))).await;

    // Recovery is a fresh leg (ADR 0043 decision 33 spawns anew, never resurrects).
    let leg_after = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::supervisor_client::query_status(&dir).expect("query_status after recovery").0.leg
    })
    .await
    .unwrap();
    assert!(leg_after.is_some(), "the recovered row must have a real leg");

    env.kill_daemon_bounded().await;
}
