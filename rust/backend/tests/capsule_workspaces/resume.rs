//! Headless input resumes a row; resume never resets; re-exec; a stale attach during backoff.

use super::*;

/// Decision 33: a headless op (`pty.input`) resumes a row whose
/// supervisor died between two ops, in place, under the row's own guard —
/// `resume_if_absent` in place of a bare `phase_of` read. The surviving
/// leg (a SIGKILL of the authority alone never touches it, ADR 0041
/// Lifecycle) is ADOPTED by the resume, not replaced: the leg epoch is
/// unchanged.
///
/// Codex review (2026-09-11): the authority is first ADOPTED across a
/// daemon restart (`restart_daemon_and_prove_adoption`,
/// `AuthorityAtRestart::Alive`) BEFORE it is killed — an authority this
/// daemon merely adopted at boot gets no watchdog at all (decision 33),
/// so the ONLY thing that can bring the row back is whatever
/// `pty.input` itself does. Without this, the row's ORIGINAL watchdog
/// (installed by `workspace.create`) would race to restart it on its
/// own, and this test could pass even with `resume_if_absent` deleted.
#[tokio::test]
#[cfg(target_os = "linux")]
#[allow(clippy::too_many_lines, reason = "one test scenario: headless input resumes a row whose supervisor died")]
async fn capsule_headless_input_resumes_a_row_whose_supervisor_died() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("hir");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "hir-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();

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
        assert!(Instant::now() < list_deadline, "timed out waiting for the new capsule workspace to reach \"ready\"");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let state_dir_path = PathBuf::from(&state_dir);

    let leg_before = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || {
            sot_log::attach_client::supervisor_client::query_status(&dir)
                .expect("query_status before killing the supervisor")
                .0
                .leg
        }
    })
    .await
    .unwrap()
    .expect("a ready capsule has a leg");

    // Adopt across a daemon restart FIRST -- the authority survives, this
    // daemon lifetime never spawned it, so no watchdog exists for it.
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

    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    let text = "echo sot-l1a-resume-marker";
    let input_deadline = Instant::now() + BOUND.max(Duration::from_secs(60));
    loop {
        let input_req = serde_json::json!({
            "workspace_id": workspace_id,
            "data_b64": STANDARD.encode(text.as_bytes()),
            "enter": true,
            "origin": "l1a-resume-test",
        });
        let input_res = call(&mut conn, next_id, op::PTY_INPUT, input_req).await;
        next_id += 1;
        if input_res.payload.get("error").is_none() && input_res.payload["ok"] == true {
            break;
        }
        assert!(
            Instant::now() < input_deadline,
            "pty.input never succeeded after the supervisor died: last reply {:?}",
            input_res.payload
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND.max(Duration::from_secs(30))).await;

    let leg_after = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || {
            sot_log::attach_client::supervisor_client::query_status(&dir)
                .expect("query_status after the resume")
                .0
                .leg
        }
    })
    .await
    .unwrap();
    assert_eq!(
        leg_after,
        Some(leg_before),
        "the leg epoch changed — the surviving leg was not adopted by the resume"
    );

    let _ = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::attach_client::supervisor_client::stop(&dir)
    })
    .await;

    env.kill_daemon_bounded().await;
}

/// Decision 33: resume-only intent — `resume_if_absent` never sends
/// `reset`. A row already `EndedNoRespawn` whose authority then dies
/// (SIGKILL, no graceful `stop`) must, on the next headless op, come back
/// reporting its OWN ended phase — never resurrected to "ready," and its
/// durable voyage pointer must be byte-identical (only `reset` — attach's
/// own retirement path, unchanged by this lane — ever rewrites it).
///
/// Codex review (2026-09-11): the authority is first ADOPTED across a
/// daemon restart (own inline restart, not
/// `restart_daemon_and_prove_adoption` — that helper polls for "ready",
/// which an `EndedNoRespawn` row never reaches) BEFORE it is killed, so
/// no watchdog exists for it and the ONLY thing that can answer the
/// headless op afterward is `resume_if_absent` itself.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn capsule_resume_never_resets_an_ended_row() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("nre");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "nre-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();

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
        assert!(Instant::now() < list_deadline, "timed out waiting for the new capsule workspace to reach \"ready\"");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let state_dir_path = PathBuf::from(&state_dir);

    let (original_status, _process) = sot_log::attach_client::supervisor_client::query_status(&state_dir_path)
        .expect("query_status before ending the run");
    let voyage = original_status.voyage.expect("a ready capsule has a voyage");

    sot_log::attach_client::supervisor_client::end_run(&state_dir_path, &voyage, "test end").expect("end_run over the lane");

    poll_until(
        || {
            let dir = state_dir_path.clone();
            async move {
                let report = try_query_status(dir).await?;
                (report.phase == sot_log::lane::wire::SupervisorPhase::EndedNoRespawn).then_some(())
            }
        },
        BOUND,
        "the ended row's authority to settle into EndedNoRespawn",
    )
    .await;

    let pointer_path = sot_log::supervisor::journal::pointer::pointer_path(&state_dir_path);
    let pointer_before = std::fs::read(&pointer_path).expect("read the pointer before killing the authority");

    // Adopt across a daemon restart FIRST -- the resting authority
    // survives (ADR 0041 Lifecycle: `EndedNoRespawn` persists until an
    // explicit `stop`), this daemon lifetime never spawned it, so no
    // watchdog exists for it.
    env.kill_daemon_bounded().await;
    drop(conn);
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    kill_supervisor_only(&env.state_root);

    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    let settle_deadline = Instant::now() + BOUND;
    loop {
        let input_req = serde_json::json!({
            "workspace_id": workspace_id,
            "data_b64": STANDARD.encode(b"echo should-never-run"),
            "enter": true,
            "origin": "l1a-ended-test",
        });
        let input_res = call(&mut conn, next_id, op::PTY_INPUT, input_req).await;
        next_id += 1;
        assert_ne!(
            input_res.payload["phase"].as_str(),
            Some("ready"),
            "resume must never bring an ended row to ready: {:?}",
            input_res.payload
        );
        if input_res.payload["phase"].as_str() == Some("ended_no_respawn") {
            assert_eq!(input_res.payload["code"], "capsule_not_ready", "{:?}", input_res.payload);
            break;
        }
        assert!(
            Instant::now() < settle_deadline,
            "pty.input never settled to ended_no_respawn: last reply {:?}",
            input_res.payload
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let pointer_after = std::fs::read(&pointer_path).expect("read the pointer after the resume attempt");
    assert_eq!(
        pointer_before, pointer_after,
        "resume must never touch the durable voyage pointer — only reset does, and resume never resets"
    );

    let _ = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::attach_client::supervisor_client::stop(&dir)
    })
    .await;

    env.kill_daemon_bounded().await;
}

/// Decision 33, the stated policy: a leg that ended WITHOUT a marker
/// (both the authority AND its leg SIGKILLed — no graceful end, nothing
/// to adopt) is RE-EXECUTED by `--resume` within the SAME voyage — the
/// supervisor's own recovery rule, never a `reset`'s fresh one. Proven by
/// a strictly higher leg epoch (a genuinely new leg process) alongside an
/// unchanged voyage id.
///
/// Codex review (2026-09-11): the authority is first ADOPTED across a
/// daemon restart (`restart_daemon_and_prove_adoption`,
/// `AuthorityAtRestart::Alive`) BEFORE it (and its leg) are killed, so no
/// watchdog exists for this row and the ONLY thing that can re-execute
/// the leg afterward is `pty.input`'s own `resume_if_absent` call.
#[tokio::test]
#[cfg(target_os = "linux")]
#[allow(clippy::too_many_lines, reason = "one test scenario: resume re-executes a leg that ended without a marker")]
async fn capsule_resume_reexecutes_a_leg_that_ended_without_a_marker() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("rrl");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "rrl-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();

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
        assert!(Instant::now() < list_deadline, "timed out waiting for the new capsule workspace to reach \"ready\"");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let state_dir_path = PathBuf::from(&state_dir);

    let (leg_before, voyage_before) = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || {
            let (report, _process) = sot_log::attach_client::supervisor_client::query_status(&dir)
                .expect("query_status before killing supervisor and leg");
            (
                report.leg.expect("a ready capsule has a leg"),
                report.voyage.expect("a ready capsule has a voyage"),
            )
        }
    })
    .await
    .unwrap();

    // Adopt across a daemon restart FIRST -- the authority survives,
    // this daemon lifetime never spawned it, so no watchdog exists for
    // it.
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
    kill_leg_only(&env.state_root);

    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    let text = "echo sot-l1a-reexec-marker";
    let input_deadline = Instant::now() + BOUND.max(Duration::from_secs(60));
    loop {
        let input_req = serde_json::json!({
            "workspace_id": workspace_id,
            "data_b64": STANDARD.encode(text.as_bytes()),
            "enter": true,
            "origin": "l1a-reexec-test",
        });
        let input_res = call(&mut conn, next_id, op::PTY_INPUT, input_req).await;
        next_id += 1;
        if input_res.payload.get("error").is_none() && input_res.payload["ok"] == true {
            break;
        }
        assert!(
            Instant::now() < input_deadline,
            "pty.input never succeeded after the supervisor AND leg both died: last reply {:?}",
            input_res.payload
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND.max(Duration::from_secs(30))).await;

    let (leg_after, voyage_after) = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || {
            let (report, _process) = sot_log::attach_client::supervisor_client::query_status(&dir)
                .expect("query_status after the re-execution");
            (report.leg, report.voyage)
        }
    })
    .await
    .unwrap();
    assert!(
        leg_after.expect("the re-executed row has a leg") > leg_before,
        "the leg epoch must be STRICTLY higher — a fresh leg must have been re-executed, not merely adopted"
    );
    assert_eq!(
        voyage_after.as_deref(),
        Some(voyage_before.as_str()),
        "re-execution stays within the SAME voyage — never a reset's fresh one"
    );

    let _ = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::attach_client::supervisor_client::stop(&dir)
    })
    .await;

    env.kill_daemon_bounded().await;
}

/// Decision 33: the watchdog's own Crash arm holds this row's guard from
/// BEFORE the restart-budget check THROUGH the backoff sleep and the
/// restart spawn itself — closing the exact window the OLD `starting`
/// claim left open (released the instant a leg exited, before any
/// backoff, so a stale attach landing mid-backoff was free to spawn a
/// second authority). A `pty.open` fired during backoff must simply WAIT
/// for the SAME guard rather than race a second spawn: across three
/// SIGKILL/backoff/respawn cycles, sampled continuously (not at a
/// handful of point-in-time checks that could straddle the one instant a
/// bug would show up), at most one `supervise` process may ever match —
/// and, since no second spawn ever races the first into the fence,
/// `EXIT_CONTENDED` (the "contended (70)" log line) must never appear.
#[tokio::test]
#[cfg(target_os = "linux")]
#[allow(clippy::too_many_lines, reason = "one test scenario: a stale attach during backoff spawns no second authority")]
async fn capsule_stale_attach_during_backoff_spawns_no_second_authority() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("sab");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "sab-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let target = create_res.payload["session_name"].as_str().expect("session_name").to_string();

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

    let pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "supervise", &env.state_root);
    let log_path = env.state_root.join("sot").join("sotd.log");
    let backoff_needle = "capsule supervisor watchdog: crashed, restarting with --resume";

    // Continuous background sampler — the claim under test is about
    // EVERY instant across the whole run below, not a handful of
    // point-in-time checks. A query failure FAILS the test (Codex
    // review, 2026-09-11) rather than silently counting as "zero
    // processes" — a false "at most one" proves nothing.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let max_seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let sampler_error = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let sampler = {
        let stop = stop.clone();
        let max_seen = max_seen.clone();
        let sampler_error = sampler_error.clone();
        let pattern = pattern.clone();
        tokio::spawn(async move {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let pattern = pattern.clone();
                let outcome = tokio::task::spawn_blocking(move || count_matching_processes(&pattern)).await;
                match outcome {
                    Ok(Ok(n)) => max_seen.fetch_max(n, std::sync::atomic::Ordering::Relaxed),
                    Ok(Err(e)) => {
                        *sampler_error.lock().unwrap() = Some(format!("pgrep failed: {e}"));
                        return;
                    }
                    Err(join_err) => {
                        *sampler_error.lock().unwrap() = Some(format!("sampler task panicked: {join_err}"));
                        return;
                    }
                };
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
    };

    for _cycle in 0..3 {
        let backoff_seen_before = count_log_occurrences(&log_path, backoff_needle);
        kill_supervisor_only(&env.state_root);

        // Synchronize on the watchdog's OWN backoff log line (Codex
        // review, 2026-09-11) — a NEW occurrence of the line the Crash
        // arm logs immediately before its backoff sleep — rather than a
        // fixed sleep, which proves nothing about whether the watchdog
        // has actually reached that point by the time this test fires
        // its own stale attach.
        let backoff_deadline = Instant::now() + BOUND;
        loop {
            if count_log_occurrences(&log_path, backoff_needle) > backoff_seen_before {
                break;
            }
            assert!(
                Instant::now() < backoff_deadline,
                "timed out waiting for the watchdog's own backoff log line to appear"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let pty_req = serde_json::json!({
            "cols": 80, "rows": 24, "user_switch": true, "target": target,
        });
        let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
        next_id += 1;
        assert_eq!(
            pty_res.payload["code"], "attach_direct",
            "a stale pty.open during backoff must still answer attach_direct once the guard frees up: {:?}",
            pty_res.payload
        );

        poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND.max(Duration::from_secs(45))).await;

        // A stale "ready" could let poll_for_phase above pass without proving
        // anything; wait for a real OS-level process count instead.
        let process_deadline = Instant::now() + BOUND;
        loop {
            if count_matching_processes(&pattern).unwrap_or(0) == 1 {
                break;
            }
            assert!(
                Instant::now() < process_deadline,
                "timed out waiting for a real supervise process to exist after cycle {_cycle}'s recovery"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = sampler.await;

    if let Some(e) = sampler_error.lock().unwrap().take() {
        panic!("process sampler failed: {e}");
    }
    assert!(
        max_seen.load(std::sync::atomic::Ordering::Relaxed) <= 1,
        "more than one supervise process matched at some sampled instant across the SIGKILL/backoff cycles"
    );

    let log_contents = std::fs::read_to_string(&log_path)
        .unwrap_or_else(|e| panic!("could not read the daemon's own log {log_path:?}: {e}"));
    assert!(
        !log_contents.contains("contended (70)"),
        "sotd.log has a contended (70) line — a second authority raced the first's own restart"
    );

    let state_dir = state_dir_from_list(&mut conn, &mut next_id, &workspace_id).await;
    let _ = tokio::task::spawn_blocking(move || sot_log::attach_client::supervisor_client::stop(&state_dir)).await;

    env.kill_daemon_bounded().await;
}
