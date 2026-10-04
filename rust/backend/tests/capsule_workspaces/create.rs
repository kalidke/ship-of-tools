//! Create, list, attach refusal, adopt and destroy of a capsule row; a live label refused.

use super::*;

#[tokio::test]
async fn capsule_workspace_create_list_attach_refusal_adopt_and_destroy() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("cwl");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    // workspace.create — a SEPARATE project root from the daemon's own
    // default (finding 2). No autostart requested, so the capsule's own
    // producer is `agent_argv("none")` == the platform shell (ADR 0042
    // L1a's own fallback). `"runtime": "capsule"` is explicit — ADR 0043
    // decision 22, the field exists for exactly this.
    let create_req = serde_json::json!({
        "label": "cwl-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let target = create_res.payload["session_name"]
        .as_str()
        .expect("session_name (the pty.open addressing token)")
        .to_string();

    // workspace.list: runtime "capsule", a state_dir, and — polled — phase
    // reaching "ready" (the capsule's platform-shell leg coming up and
    // the supervisor's own lane answering `status`).
    let list_deadline = Instant::now() + BOUND.max(Duration::from_secs(90));
    let state_dir = loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, &workspace_id) {
            assert_eq!(row["runtime"], "capsule", "row: {row:?}");
            if let (Some(sd), Some("ready")) = (row["state_dir"].as_str(), row["phase"].as_str()) {
                break sd.to_string();
            }
        }
        assert!(
            Instant::now() < list_deadline,
            "timed out waiting for workspace.list to report phase \"ready\" for the new capsule workspace"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let state_dir_path = PathBuf::from(&state_dir);
    assert!(state_dir_path.is_dir(), "reported state_dir does not exist on disk: {state_dir}");

    // pty.open on a capsule workspace: refused, never proxied.
    let pty_req = serde_json::json!({ "cols": 80, "rows": 24, "target": target, "user_switch": true });
    let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    // `next_id` has no further use on this connection (it is dropped and
    // replaced after the daemon restart below), so no further increment.
    assert_eq!(pty_res.payload["code"], "attach_direct", "pty.open payload: {:?}", pty_res.payload);
    assert_eq!(
        pty_res.payload["state_dir"].as_str(),
        Some(state_dir.as_str()),
        "pty.open's attach_direct state_dir should match workspace.list's"
    );

    // --- Adoption proof (Codex review finding 13; folded round-2 into
    // the shared restart_daemon_and_prove_adoption helper below, which
    // also serves the boot-adopts-a-still-alive-supervisor test in this
    // same file) ---
    // Record the current leg epoch BEFORE stopping the supervisor
    // authority over its own lane (sot_log::attach_client::supervisor_client::stop) --
    // its capsule leg is deliberately outside the supervisor's own job
    // (ADR 0041 Lifecycle) and survives.
    let leg_before = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || {
            sot_log::attach_client::supervisor_client::query_status(&dir)
                .expect("query_status before stop")
                .0
                .leg
        }
    })
    .await
    .unwrap()
    .expect("a ready capsule has a leg");

    let (mut conn2, mut next_id2) = restart_daemon_and_prove_adoption(
        &env,
        conn,
        &workspace_id,
        &state_dir,
        &state_dir_path,
        leg_before,
        AuthorityAtRestart::Stopped,
    )
    .await;

    // --- Destroy proof (Codex review finding 13) ---
    // workspace.destroy: ends the run; independently confirm via the
    // lane itself (not just the daemon's own say-so) that the record
    // actually closed BEFORE asserting the row disappears from
    // workspace.list; the state dir is NOT deleted (the record persists
    // by design).
    let destroy_req = serde_json::json!({ "workspace_id": workspace_id });
    let destroy_res = call(&mut conn2, next_id2, op::WORKSPACE_DESTROY, destroy_req).await;
    next_id2 += 1;
    assert!(destroy_res.payload.get("error").is_none(), "workspace.destroy failed: {:?}", destroy_res.payload);

    // The old "poll for phase EndedNoRespawn" expectation is obsolete:
    // `end_run`'s own wrapper now sends the (post-#184) WAITING `stop`
    // once the end is confirmed, so `workspace.destroy`'s own response
    // doesn't land until the authority has already exited (or is in
    // the process of it) — the lane goes SILENT instead of resting in
    // EndedNoRespawn, and polling for that resting phase here raced a
    // window too narrow to reliably observe (CI's own field finding).
    // Leak proof: mirror the SAME "lane goes silent after stop" idiom
    // the adoption proof above uses (`sot-capsule supervise` otherwise
    // idles in `EndedNoRespawn` forever without a `stop` request — see
    // `supervisor/mod.rs`'s own exit-condition doc). Without this, the
    // field defect this closes reproduces exactly: one resident
    // `sot-capsule.exe` per destroy, holding `supervisor.lock` and the
    // exe, that nothing would ever reap.
    poll_until(
        || {
            let dir = state_dir_path.clone();
            async move { if try_query_status(dir).await.is_none() { Some(()) } else { None } }
        },
        BOUND,
        "the ended supervisor's own lane to go silent (workspace.destroy's end_run must also stop it)",
    )
    .await;

    let destroy_deadline = Instant::now() + BOUND;
    loop {
        let id = next_id2;
        next_id2 += 1;
        let payload = call(&mut conn2, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if find_row(&payload, &workspace_id).is_none() {
            break;
        }
        assert!(
            Instant::now() < destroy_deadline,
            "timed out waiting for the destroyed workspace to disappear from workspace.list"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        state_dir_path.is_dir(),
        "the capsule's state dir must survive workspace.destroy (the record persists by design): {state_dir}"
    );

    env.kill_daemon_bounded().await;
}

/// A same-slug `workspace.create` on a live row is refused (`label_in_use`)
/// and leaves the row's task, id and phase as they were (ADR 0036 update).
#[tokio::test]
async fn workspace_create_on_a_live_label_is_refused_and_leaves_the_row_untouched() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("cil");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create = |task: &str| {
        serde_json::json!({
            "label": "cil-workspace",
            "project_root": env.workspace_project_root.to_string_lossy(),
            "runtime": "capsule",
            "task": task,
        })
    };
    let first = call(&mut conn, next_id, op::WORKSPACE_CREATE, create("first")).await;
    next_id += 1;
    assert!(first.payload.get("error").is_none(), "first create failed: {:?}", first.payload);
    let workspace_id = first.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND.max(Duration::from_secs(90))).await;

    let second = call(&mut conn, next_id, op::WORKSPACE_CREATE, create("second")).await;
    next_id += 1;
    assert_eq!(second.payload["code"], "label_in_use", "second create: {:?}", second.payload);
    assert!(second.payload.get("workspace_id").is_none(), "refusal must not read as success: {:?}", second.payload);
    assert_eq!(second.payload["existing"]["workspace_id"], workspace_id.as_str());
    assert_eq!(second.payload["existing"]["phase"], "ready");

    let list = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await;
    next_id += 1;
    let row = find_row(&list.payload, &workspace_id).expect("row survives the refused create");
    assert_eq!(row["task"], "first", "row: {row:?}");
    assert_eq!(row["phase"], "ready", "row: {row:?}");

    let destroy = call(&mut conn, next_id, op::WORKSPACE_DESTROY, serde_json::json!({ "workspace_id": workspace_id })).await;
    assert!(destroy.payload.get("error").is_none(), "destroy failed: {:?}", destroy.payload);
}
