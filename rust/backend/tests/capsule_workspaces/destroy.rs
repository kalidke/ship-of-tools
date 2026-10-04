//! Destroy after adoption, after a dead supervisor, a markerless leg and a missing state dir.

use super::*;

/// Decision 33: an ADOPTED row (`resume_all`'s boot scan found the
/// authority already alive and simply logged it — "a watchdog exists
/// only for a `Child` the daemon launched") gets no watchdog at all.
/// Destroying it must leave nothing behind that could respawn it: no
/// lingering `supervise` process sustained over a real window (not a
/// single point-in-time check), the fence freely acquirable again
/// afterward, and — the regression this specifically guards against —
/// `sotd.log` never carries the watchdog's own "treating as a crash"
/// text, which could only appear if some future change re-attached a
/// watchdog to an adopted leg (or the old `starting` claim's stale-
/// backoff window came back).
#[tokio::test]
#[cfg(target_os = "linux")]
async fn capsule_destroy_after_adoption_leaves_no_respawn() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("dan");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "dan-workspace",
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
                .expect("query_status before the daemon restart")
                .0
                .leg
        }
    })
    .await
    .unwrap()
    .expect("a ready capsule has a leg");

    // Adopt: kill only the DAEMON, leave the authority alive, reboot —
    // resume_all's own `None` arm now just logs and moves on (decision
    // 33: no watchdog for a row this daemon never itself spawned).
    let (mut conn2, next_id2) = restart_daemon_and_prove_adoption(
        &env,
        conn,
        &workspace_id,
        &state_dir,
        &state_dir_path,
        leg_before,
        AuthorityAtRestart::Alive,
    )
    .await;

    let destroy_req = serde_json::json!({ "workspace_id": workspace_id });
    // `next_id2` has no further use on this connection (mirrors the
    // create/list/destroy test's own convention) — no further increment.
    let destroy_res = call(&mut conn2, next_id2, op::WORKSPACE_DESTROY, destroy_req).await;
    assert!(destroy_res.payload.get("error").is_none(), "workspace.destroy failed: {:?}", destroy_res.payload);

    poll_until(
        || {
            let dir = state_dir_path.clone();
            async move { if try_query_status(dir).await.is_none() { Some(()) } else { None } }
        },
        BOUND,
        "the destroyed row's supervisor lane to go silent",
    )
    .await;

    let pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "supervise", &env.state_root);
    // 5s SUSTAINED silence (250ms loop) — the regression this closes is a
    // respawn some moments AFTER destroy, not merely "not respawned yet."
    let sustain_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < sustain_deadline {
        assert!(
            try_query_status(state_dir_path.clone()).await.is_none(),
            "a destroyed, adopted row answered a status query again — it respawned"
        );
        assert!(
            !any_process_matches(&pattern),
            "a destroyed, adopted row's supervisor process reappeared — it respawned"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // The fence is freely acquirable — no lingering authority holds it.
    let fence = sot_log::supervisor::journal::fence::lock_supervisor(&state_dir_path);
    assert!(fence.is_ok(), "the fence must be acquirable after a clean destroy — something still holds it");
    drop(fence);

    let log_path = env.state_root.join("sot").join("sotd.log");
    let log_contents = std::fs::read_to_string(&log_path)
        .unwrap_or_else(|e| panic!("could not read the daemon's own log {log_path:?}: {e}"));
    assert!(
        !log_contents.contains("treating as a crash"),
        "sotd.log has a \"treating as a crash\" line — no watchdog should ever have run for this adopted, then destroyed, row"
    );

    env.kill_daemon_bounded().await;
}

/// ADR 0043 decision 33's destroy proof, exercised end to end: a row
/// whose SUPERVISOR alone died (the leg survives headless) is still
/// destroyable. `destroy_capsule_workspace`'s own pre-step
/// (`capsule_workspace::resume_locked`, under the SAME row guard
/// `end_run` then runs under) re-establishes the authority first, so
/// `end_run` finds a real lane to ask — the fence/leg proof
/// (`leg_absent`) never needs to fire at all.
///
/// Codex review (2026-09-11): the authority is first ADOPTED across a
/// daemon restart (`restart_daemon_and_prove_adoption`,
/// `AuthorityAtRestart::Alive`) BEFORE it is killed — an authority this
/// daemon merely adopted at boot gets no watchdog at all (decision 33),
/// so the ONLY thing that can bring the row back for `end_run` to reach
/// is `destroy_capsule_workspace`'s own resume call below. Without this,
/// the row's ORIGINAL watchdog (installed by `workspace.create`) could
/// race to restart it on its own, and this test could pass even with
/// that resume call deleted. The restart itself proves the surviving
/// leg's identity is unchanged (`restart_daemon_and_prove_adoption`'s own
/// leg-epoch assertion) before the supervisor is ever killed.
///
/// Both the re-established `supervise` process and the `run` leg it ends
/// must be gone within a bound, and the row itself removed from
/// `workspace.list` — the ordinary confirmed-end removal, reached from a
/// row that looked dead going in.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn capsule_destroy_resumes_then_ends_a_leg_whose_supervisor_died() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("ddr");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "ddr-workspace",
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
                .expect("query_status before the daemon restart")
                .0
                .leg
        }
    })
    .await
    .unwrap()
    .expect("a ready capsule has a leg");

    // Adopt across a daemon restart FIRST -- the authority survives, this
    // daemon lifetime never spawned it, so no watchdog exists for it; the
    // ONLY thing left that can bring the row back is
    // `destroy_capsule_workspace`'s own resume call below.
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

    // `destroy_capsule_workspace`'s own resume pre-step re-establishes
    // the authority before `end_run` ever runs; a lane still settling
    // past `Starting` when `end_run` reaches it answers `Kept` with
    // "supervisor is starting; retry" — a legitimate retryable outcome
    // (`EndRunOutcome::Starting`'s own doc), never a failure. Retry
    // exactly that one reason; anything else fails the test at once.
    let destroy_deadline = Instant::now() + BOUND.max(Duration::from_secs(30));
    loop {
        let destroy_req = serde_json::json!({ "workspace_id": workspace_id });
        let destroy_res = call(&mut conn, next_id, op::WORKSPACE_DESTROY, destroy_req).await;
        next_id += 1;
        if destroy_res.payload.get("error").is_none() {
            break;
        }
        let detail = destroy_res.payload.get("error").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            detail.contains("starting"),
            "workspace.destroy failed for a reason other than a still-settling resume: {:?}",
            destroy_res.payload
        );
        assert!(
            Instant::now() < destroy_deadline,
            "workspace.destroy never got past \"starting; retry\": last reply {:?}",
            destroy_res.payload
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let supervise_pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "supervise", &env.state_root);
    let run_pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "run", &env.state_root);
    assert!(
        poll_until_no_process_matches(&supervise_pattern, Duration::from_secs(10)),
        "a supervisor process still matches {supervise_pattern:?} after destroy"
    );
    assert!(
        poll_until_no_process_matches(&run_pattern, Duration::from_secs(10)),
        "a leg process still matches {run_pattern:?} after destroy"
    );

    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id += 1;
    let _ = next_id;
    assert!(
        find_row(&list_payload, &workspace_id).is_none(),
        "the destroyed row must no longer be listed: {list_payload:?}"
    );

    env.kill_daemon_bounded().await;
}

/// ADR 0043 decision 33's destroy proof, the markerless-death case: BOTH
/// the supervisor AND the leg die with no end marker on disk. Destroy
/// still succeeds — the resumed authority re-executes the leg within the
/// same voyage (the supervisor's own recovery rule, exercised by
/// `capsule_resume_reexecutes_a_leg_that_ended_without_a_marker`) and the
/// subsequent `end_run` ends THAT leg — the stated policy, not a gap
/// this proof leaves open. Nothing survives, and the row is removed.
///
/// Codex review (2026-09-11): the authority is first ADOPTED across a
/// daemon restart (`restart_daemon_and_prove_adoption`,
/// `AuthorityAtRestart::Alive`) BEFORE it (and its leg) are killed, so no
/// watchdog exists for this row and the ONLY thing that can re-execute
/// the leg and then end it is `destroy_capsule_workspace`'s own resume
/// call — the row's ORIGINAL watchdog (installed by `workspace.create`)
/// could otherwise race to recover it first, and this test could pass
/// even with that resume call deleted. The "no end marker" precondition
/// this test's own name claims is checked directly
/// (`sot_log::store::verify::leg_carries_run_end_marker`) right after both
/// SIGKILLs, rather than only inferred from the recovery behaviour
/// afterward — destroy's own success proves nothing about markerlessness
/// on its own; an adopted-but-cleanly-ended leg would also let destroy
/// succeed.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn capsule_destroy_after_a_markerless_leg_death_leaves_nothing() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("dml");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "dml-workspace",
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
                .expect("query_status before the daemon restart");
            (
                report.leg.expect("a ready capsule has a leg"),
                report.voyage.expect("a ready capsule has a voyage"),
            )
        }
    })
    .await
    .unwrap();

    // Adopt across a daemon restart FIRST -- the authority survives, this
    // daemon lifetime never spawned it, so no watchdog exists for it; the
    // ONLY thing left that can re-execute and then end the leg is
    // `destroy_capsule_workspace`'s own resume call below.
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

    // The "markerless" precondition this test is named for, checked
    // directly rather than only inferred from the recovery behaviour
    // afterward.
    let seg_dir = sot_log::supervisor::voyage_root_path(&state_dir_path, &voyage_before).join("seg");
    let carries_marker = tokio::task::spawn_blocking({
        let seg_dir = seg_dir.clone();
        let voyage = voyage_before.clone();
        move || sot_log::store::verify::leg_carries_run_end_marker(&seg_dir, &voyage, leg_before)
    })
    .await
    .unwrap()
    .expect("leg_carries_run_end_marker must read the SIGKILLed leg's own segment cleanly");
    assert!(
        !carries_marker,
        "the killed leg carries an end marker — this is not the markerless-death precondition this test claims"
    );

    // Re-executing the leg from scratch (no survivor to adopt) is
    // slower than a plain adoption — `destroy_capsule_workspace`'s
    // resume pre-step may still be settling past `Starting` when
    // `end_run` first reaches it (`EndRunOutcome::Starting`'s own doc:
    // retryable, never a failure). Retry exactly that one reason; any
    // other failure fails the test at once.
    let destroy_deadline = Instant::now() + BOUND.max(Duration::from_secs(30));
    loop {
        let destroy_req = serde_json::json!({ "workspace_id": workspace_id });
        let destroy_res = call(&mut conn, next_id, op::WORKSPACE_DESTROY, destroy_req).await;
        next_id += 1;
        if destroy_res.payload.get("error").is_none() {
            break;
        }
        let detail = destroy_res.payload.get("error").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            detail.contains("starting"),
            "workspace.destroy failed for a reason other than a still-settling resume: {:?}",
            destroy_res.payload
        );
        assert!(
            Instant::now() < destroy_deadline,
            "workspace.destroy never got past \"starting; retry\": last reply {:?}",
            destroy_res.payload
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let supervise_pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "supervise", &env.state_root);
    let run_pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "run", &env.state_root);
    assert!(
        poll_until_no_process_matches(&supervise_pattern, Duration::from_secs(10)),
        "a supervisor process still matches {supervise_pattern:?} after destroy"
    );
    assert!(
        poll_until_no_process_matches(&run_pattern, Duration::from_secs(10)),
        "a leg process still matches {run_pattern:?} after destroy"
    );

    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id += 1;
    let _ = next_id;
    assert!(
        find_row(&list_payload, &workspace_id).is_none(),
        "the destroyed row must no longer be listed: {list_payload:?}"
    );

    env.kill_daemon_bounded().await;
}

/// ADR 0043 decision 33's destroy proof, the missing-state-dir case
/// (refined): a row that was cleanly ended and stopped, then had its
/// whole state dir removed out from under it (an operator `rm -rf`, or
/// an external volume issue) — never a licence to recreate anything, but
/// no longer an automatic keep either. `end_run`'s `!state_dir.is_dir()`
/// check no longer reports `state_dir_missing` unconditionally: with the
/// directory gone, neither the supervisor fence nor this voyage's writer
/// lock can exist anywhere (both live under `state_dir`), so the only
/// remaining question is whether a supervisor still answers this row's
/// lane (addressed independently of the directory, by the hash of the
/// canonical state-dir path). Here the authority was already stopped and
/// its lane confirmed silent BEFORE the directory was removed, so
/// `query_status`'s connect returns decision 27's own "no listener at
/// all" shape and `destroy_capsule_workspace` proves the row `Orphaned`
/// — a CONFIRMED end (`orphan_removed`), same as a normal `end_run`
/// success, and the row is actually removed. The directory is still
/// never recreated at any point.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn capsule_destroy_on_a_missing_state_dir_with_no_listener_removes_the_row() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("dsm");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "dsm-workspace",
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

    // End the run and stop the authority cleanly (`sot_log::
    // supervisor_client` directly, mirroring this file's own doc on why:
    // proving the record is closed before ever touching the directory).
    let voyage = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || {
            sot_log::attach_client::supervisor_client::query_status(&dir)
                .expect("query_status before ending the run")
                .0
                .voyage
                .expect("a ready capsule has a voyage")
        }
    })
    .await
    .unwrap();
    tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::attach_client::supervisor_client::end_run(&dir, &voyage, "test: state_dir_missing proof")
    })
    .await
    .unwrap()
    .expect("end_run must succeed on a ready row");
    tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::attach_client::supervisor_client::stop(&dir)
    })
    .await
    .unwrap()
    .expect("stop the ended authority");
    poll_until(
        || {
            let dir = state_dir_path.clone();
            async move { if try_query_status(dir).await.is_none() { Some(()) } else { None } }
        },
        BOUND,
        "the stopped supervisor's own lane to go silent",
    )
    .await;

    std::fs::remove_dir_all(&state_dir_path)
        .unwrap_or_else(|e| panic!("rm -rf the state dir {state_dir_path:?}: {e}"));
    assert!(!state_dir_path.exists(), "the state dir must actually be gone before destroy is asked");

    let destroy_req = serde_json::json!({ "workspace_id": workspace_id });
    let destroy_res = call(&mut conn, next_id, op::WORKSPACE_DESTROY, destroy_req).await;
    next_id += 1;
    // A NON-default row's proven-orphan destroy takes the ordinary
    // "actually removed" wire shape (`WorkspaceDestroyRes`'s own doc:
    // `kept: None` means the row above was really removed) -- the
    // distinct `orphan_removed` outcome this fix adds is visible in the
    // daemon's own log (`destroy_capsule_workspace`'s
    // `tracing::info!(..., %outcome, ...)`), not on this wire shape,
    // which is identical to any other confirmed end (`RecordVerified`,
    // `Unheld`, ...). Only the DEFAULT row's own kept-not-removed
    // response carries the outcome text (`default_row_end_response`).
    assert!(
        destroy_res.payload.get("error").is_none(),
        "a proven orphan is a CONFIRMED end, not an error: {:?}", destroy_res.payload
    );
    assert_eq!(
        destroy_res.payload.get("toml_removed").and_then(|v| v.as_bool()),
        Some(true),
        "a proven orphan's toml must actually be removed: {:?}", destroy_res.payload
    );
    assert!(
        !state_dir_path.exists(),
        "destroy on a missing state dir must never recreate it: {state_dir_path:?}"
    );

    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id += 1;
    let _ = next_id;
    assert!(
        find_row(&list_payload, &workspace_id).is_none(),
        "a proven orphan is actually removed, not merely kept: {list_payload:?}"
    );

    env.kill_daemon_bounded().await;
}
