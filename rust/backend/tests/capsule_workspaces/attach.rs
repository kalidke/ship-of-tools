//! Start on attach, recovery by reset, and attach after a run end.

use super::*;

/// 2026-09-04 amendment (owner ruling): the daemon's own default/home
/// row is now an INERT ANCHOR when it carries no agent (`agent ==
/// "none"`) — the workspace it falls back to and the way to browse this
/// machine's files, never a session. This supersedes the old
/// `capsule_default_workspace_starts_its_supervisor_on_first_attach`
/// (which this test replaces): before this amendment, `pty.open`'s
/// start-on-attach spawned a supervisor for this row unconditionally
/// (the v0.6.0-rc.2 field fix below); now it must NOT, specifically
/// because its agent is "none" and it is the daemon's default. Proves
/// the inverse of the old claim: `pty.open` still answers
/// `attach_direct` (the SAME response every capsule row gets, never a
/// special error — see `rows/ops/pty.rs`'s own `pty.open` handler), but
/// nothing is ever spawned behind it — no state dir, no lane, the row's
/// own phase never leaves "stopped".
/// `capsule_created_workspace_starts_on_attach_and_recovers_via_reset_after_end`
/// (below) is where the "start-on-attach actually spawns something"
/// proof now lives, on an ordinary row.
///
/// (v0.6.0-rc.2 field finding, for context: the daemon's own default/home
/// workspace is registered with `runtime: "capsule"` at startup, but
/// `workspace.create` was the ONLY path that ever spawned a capsule's
/// supervisor — this row was never created through it, so it never got
/// one, and selecting it in the frontend parked it on an empty pane
/// forever. Start-on-attach closed that gap for every capsule row
/// generally; this amendment carves the DEFAULT-with-no-agent row back
/// out of it specifically.)
///
/// Windows-only (ADR 0043 decision 22): the default row's own STEADY
/// STATE is `runtime = "capsule"` only on Windows — the Linux platform
/// default stays "tmux" until the bridge, so this scenario (a capsule
/// DEFAULT row) is not a real day-to-day Linux configuration yet.
#[tokio::test]
#[cfg(windows)]
async fn capsule_default_workspace_with_no_agent_is_never_started_on_attach() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("dna");
    // Pre-write the default row's own toml as the INERT-ANCHOR agent,
    // "none" — the exact shape a fresh-boot default row seeds today on
    // EVERY host (`rows/anchor.rs`'s 2026-09-04 amendment). Pre-writing it here
    // keeps this test's precondition explicit and independent of that
    // default ever changing again.
    env.seed_default_capsule_toml("none");
    env.spawn_sotd();
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
    assert_eq!(default_row["runtime"], "capsule", "default row: {default_row:?}");
    assert_eq!(default_row["agent"], "none", "default row: {default_row:?}");
    let default_workspace_id = default_row["workspace_id"].as_str().expect("workspace_id").to_string();
    let default_target = default_row["session_name"].as_str().expect("session_name").to_string();

    // Same path arithmetic `rows::spawn::state_root::state_dir_for` uses:
    // `<LOCALAPPDATA>\sot\workspaces\<workspace_id>` — `env.state_root` IS
    // the LOCALAPPDATA value this daemon was launched with (see
    // `Env::spawn_sotd`).
    let state_dir_path = env.state_root.join("sot").join("workspaces").join(&default_workspace_id);

    // Precondition: no state dir on disk at all, and `workspace.list`'s
    // own row already reads "stopped" from THIS first list call (rule B:
    // the startup resume-scan skips every row with no published voyage
    // pointer, so it never touched this one either).
    assert!(
        !state_dir_path.exists(),
        "the default capsule's state dir must not exist before this test's own attach: {state_dir_path:?}"
    );
    assert_eq!(
        default_row["phase"].as_str(),
        Some("stopped"),
        "the default row's phase must read \"stopped\" before its first attach: {default_row:?}"
    );

    // `target` MUST be the row's own `session_name` — a targetless
    // `pty.open` addresses the drawer's own special SoT LLM terminal
    // (`pty::DEFAULT_TMUX_TARGET` == "sot-llm"), never a workspace row;
    // `rows/registry.rs`'s `workspace_for_tmux(requested_target)` only resolves
    // to this row when `target` matches its `session_name`. This is
    // exactly what the frontend sends attaching a capsule row — though
    // in practice the frontend never sends it for THIS row at all
    // (2026-09-04's own frontend-side filter, tested separately in
    // `ui/nav/sessions_tree_tests.rs`); this is belt-and-suspenders coverage of the backend
    // guard alone.
    let pty_req = serde_json::json!({
        "cols": 80, "rows": 24, "user_switch": true, "target": default_target,
    });
    let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    next_id += 1;
    assert_eq!(pty_res.payload["code"], "attach_direct", "pty.open payload: {:?}", pty_res.payload);

    // Dwell across a window comfortably longer than every OTHER
    // start-on-attach proof in this file needs to first observe its own
    // state dir/lane — if the anchor rule regressed and a supervisor
    // silently started anyway, this window is generous enough to catch
    // it; asserted continuously throughout, never just once at the end.
    let never_started_deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < never_started_deadline {
        assert!(
            !state_dir_path.exists(),
            "the default row's agent-none anchor must never spawn a supervisor on attach: {state_dir_path:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        try_query_status(state_dir_path.clone()).await.is_none(),
        "the default row's agent-none anchor's lane must never answer after an attach attempt"
    );

    // `workspace.list` must still read "stopped" — never "starting" or
    // "ready".
    let payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    let row = find_row(&payload, &default_workspace_id).expect("default row still listed");
    assert_eq!(
        row["phase"].as_str(),
        Some("stopped"),
        "the default row's agent-none anchor must still read \"stopped\" after an attach attempt: {row:?}"
    );

    env.kill_daemon_bounded().await;
}

/// 2026-09-04 amendment: the default row's own "never touched by
/// `workspace.create`" precondition no longer proves `pty.open`'s
/// start-on-attach actually spawns anything — that row is now the inert
/// anchor by design (see
/// `capsule_default_workspace_with_no_agent_is_never_started_on_attach`,
/// above, which this test's predecessor was split into). This moves
/// that proof, plus the #182 items A.1/C end -> reattach -> new-voyage-
/// via-reset proof, onto an ORDINARY (non-default) capsule row instead —
/// pre-seeded via `Env::seed_capsule_toml` rather than
/// `workspace.create`, since `workspace.create`'s own handler spawns
/// synchronously as part of creation and so never leaves a row in the
/// "registered but never started" state start-on-attach needs to prove
/// anything at all. Seeded with the placeholder `agent = "none"` —
/// unchanged and intended: the inert-anchor rule is scoped to the
/// DEFAULT row specifically (ADR 0042's amendment), so an ordinary row
/// with no agent still runs the same `agent_argv("none")` == the
/// platform shell placeholder every other created-workspace test in
/// this file relies on.
///
/// The #182 proof itself can't route through `workspace.destroy` here
/// the way the old default-row test did — that op only KEEPS a row's
/// registry entry for the DEFAULT workspace
/// (`handle_workspace_destroy`'s own doc: "the default workspace's ROW
/// is never destroyed here"); on a NON-default row it actually REMOVES
/// the registry entry once the run is confirmed ended, which would
/// delete the very row this test needs to re-attach to. Ends the run
/// directly over the lane instead (`sot_log::attach_client::supervisor_client::end_run`,
/// never a follow-up `stop` — the authority is left RESIDENT in
/// `EndedNoRespawn` on purpose, so the re-attach below exercises the
/// real retirement arm against a genuinely resident authority, not an
/// already-stopped one), leaving the row fully registered; `pty.open`'s
/// start-on-attach (`ensure_started`) then retires it (ADR 0043
/// decision 33) and mints a new voyage via `reset` — the exact mechanic
/// this proves, regardless of which row it runs on.
#[tokio::test]
#[allow(clippy::too_many_lines, reason = "one test scenario: a created row starts on attach and recovers by reset after its end")]
async fn capsule_created_workspace_starts_on_attach_and_recovers_via_reset_after_end() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("car");
    // A project folder whose name starts with `t`: on Windows its path holds `\t`, which the row store's reader
    // decodes unless the seeded value is quoted as its writer quotes it.
    let project_root = env._tmp.path().join("t-project");
    std::fs::create_dir_all(&project_root).expect("mkdir the pre-seeded row's project");
    // Pre-seed an ORDINARY (non-default) capsule row — never touched by
    // `workspace.create`, so its supervisor has never been spawned.
    env.seed_capsule_toml(
        "ws-preseeded-extra",
        "extra",
        &project_root,
        "none",
    );
    // Real test-only barrier: activation blocks until this file exists,
    // turning the "reply before state dir exists" race into a certainty.
    let barrier = env._tmp.path().join("car-activation-barrier");
    env.spawn_sotd_with_env(&[("SOT_TEST_ACTIVATION_BARRIER", &barrier.to_string_lossy())]);
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id += 1;
    let row = find_row(&list_payload, "ws-preseeded-extra").expect("the pre-seeded row is registered");
    assert_eq!(row["runtime"], "capsule", "row: {row:?}");
    assert_eq!(
        row["project_root"].as_str(),
        Some(&*project_root.to_string_lossy()),
        "the daemon must read the pre-seeded row's project folder back unchanged: {row:?}"
    );
    let workspace_id = row["workspace_id"].as_str().expect("workspace_id").to_string();
    let target = row["session_name"].as_str().expect("session_name").to_string();

    // Same path arithmetic `rows::spawn::state_root::state_dir_for` uses.
    let state_dir_path = env.state_root.join("sot").join("workspaces").join(&workspace_id);

    // Rule H: prove the "never started" precondition BEFORE `pty.open` —
    // no state dir on disk at all, and `workspace.list`'s own row already
    // reads "stopped" from THIS first list call (rule B: the startup
    // resume-scan skips every row with no published voyage pointer, so
    // it never touched this one).
    assert!(
        !state_dir_path.exists(),
        "the pre-seeded row's state dir must not exist before its first attach: {state_dir_path:?}"
    );
    assert_eq!(
        row["phase"].as_str(),
        Some("stopped"),
        "the pre-seeded row's phase must read \"stopped\" before its first attach: {row:?}"
    );

    // pty.open must answer at once from phase, never awaiting the activation --
    // order-only proof since spawning a real sot-capsule is far slower.
    let pty_req = serde_json::json!({ "cols": 80, "rows": 24, "user_switch": true, "target": target });
    let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    next_id += 1;
    assert_eq!(pty_res.payload["code"], "attach_direct", "pty.open payload: {:?}", pty_res.payload);
    let expected_state_dir = state_dir_path.to_string_lossy().into_owned();
    assert_eq!(
        pty_res.payload["state_dir"].as_str(),
        Some(expected_state_dir.as_str()),
        "pty.open's attach_direct state_dir should be this row's own capsule state dir"
    );
    assert!(
        !barrier.exists(),
        "test bug: the barrier must not have been released yet"
    );
    assert!(
        !state_dir_path.is_dir(),
        "the reply must arrive while the activation is still held, before it has had any chance \
         to create the state dir"
    );

    std::fs::write(&barrier, b"go").expect("release the activation barrier");

    // Appears once released -- proof of a real spawn, not a stale path.
    let dir_deadline = Instant::now() + BOUND;
    while !state_dir_path.is_dir() {
        assert!(
            Instant::now() < dir_deadline,
            "timed out waiting for the pre-seeded row's state dir to appear: {state_dir_path:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // The lane answers — a real supervisor authority is listening, not
    // just an empty directory left behind by a partial spawn.
    poll_until(
        || {
            let dir = state_dir_path.clone();
            async move { try_query_status(dir).await }
        },
        BOUND,
        "the pre-seeded row's supervisor lane to answer a status query",
    )
    .await;

    // The row reaches Ready (leg spawned, ConPTY up, challenge proven)
    // before this test ends its run.
    poll_until(
        || {
            let dir = state_dir_path.clone();
            async move {
                let report = try_query_status(dir).await?;
                (report.phase == sot_log::lane::wire::SupervisorPhase::Ready).then_some(())
            }
        },
        BOUND,
        "the pre-seeded row's supervisor to reach phase Ready",
    )
    .await;

    // --- #182 items A.1/C, plus this test's own doc above: end the run,
    // leave the authority resting, prove attach recovers via `reset`
    // with a NEW voyage (not a flat refusal, not a resurrected old one) ---
    let (original_status, original_process) = sot_log::attach_client::supervisor_client::query_status(&state_dir_path)
        .expect("query_status before ending the run");
    let original_voyage = original_status
        .voyage
        .expect("a ready capsule has a voyage");
    let original_pid = original_process.pid();

    sot_log::attach_client::supervisor_client::end_run(&state_dir_path, &original_voyage, "test end")
        .expect("end_run over the lane");

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

    // Re-attach, repeatedly and bounded, until the row is genuinely
    // live again with a NEW voyage — item C, the claim this test
    // proves. Never assert a specific attach count or an intermediate
    // phase: `ensure_started`'s own inline settle loop after a resume
    // spawn usually catches a marker-only recovery's near-instant
    // `EndedNoRespawn` transition and resets it within the FIRST
    // re-attach, entirely inside that one `pty.open` round trip
    // (`reset` itself polls to completion before `ensure_started`
    // returns) — so `EndedNoRespawn` is often never independently
    // observable from here at all. A slower settle just needs one more
    // attach once it lands; repeated attaches are harmless either way
    // (a `Resetting`/already-live authority answers "already up",
    // nothing to do).
    let ready_deadline = Instant::now() + BOUND.max(Duration::from_secs(90));
    let new_voyage = loop {
        let reattach_req = serde_json::json!({
            "cols": 80, "rows": 24, "user_switch": true, "target": target,
        });
        let reattach_res = call(&mut conn, next_id, op::PTY_OPEN, reattach_req).await;
        next_id += 1;
        assert_eq!(
            reattach_res.payload["code"], "attach_direct",
            "re-attach after end: {:?}",
            reattach_res.payload
        );
        if let Some(report) = try_query_status(state_dir_path.clone()).await {
            if report.phase == sot_log::lane::wire::SupervisorPhase::Ready {
                break report.voyage.expect("a ready capsule has a voyage");
            }
        }
        assert!(
            Instant::now() < ready_deadline,
            "timed out waiting for the pre-seeded row to recover via reset after being ended"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_ne!(
        new_voyage, original_voyage,
        "reset must mint a NEW voyage, not resurrect the ended one"
    );

    // L3 (ADR 0043 decision 33's retirement clause): the OLD resident authority must actually
    // be RETIRED, never merely raced past by a `reset` that landed on it
    // directly — a distinct pid is the cross-platform half of that proof
    // (`ChallengedProcess::pid()` exists on both platforms).
    let (_new_status, new_process) =
        sot_log::attach_client::supervisor_client::query_status(&state_dir_path).expect("query_status after recovery");
    assert_ne!(
        new_process.pid(),
        original_pid,
        "the recovered authority must be a FRESH process, never the old resident one reset() landed on"
    );
    // Linux-only half: the old pid is gone (or no longer naming
    // `supervise`) from `/proc`, and exactly one `supervise` process
    // matches this row's own state dir now.
    #[cfg(target_os = "linux")]
    {
        let old_cmdline = std::fs::read_to_string(format!("/proc/{original_pid}/cmdline")).unwrap_or_default();
        assert!(
            !old_cmdline.contains("supervise"),
            "the old resident authority (pid {original_pid}) must be gone or no longer running `supervise`: {old_cmdline:?}"
        );
        let pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "supervise", &env.state_root);
        assert_eq!(
            count_matching_processes(&pattern).expect("pgrep"),
            1,
            "exactly one supervise process must match this row's state dir after recovery"
        );
    }

    // Rule H: the spawned supervisor's OWN leg is DETACHED (spawned by
    // the daemon, survives the daemon's own exit by design, ADR 0042
    // L1a), so killing the daemon below does NOT reap it and it would
    // otherwise leak past this test. There is no `std::process::Child`
    // for it here (the daemon owns the actual spawn), so this stops it
    // over its own lane instead — the same
    // `sot_log::attach_client::supervisor_client::stop` the create-test's own adoption
    // proof uses. Best-effort: the AUTHORITY is gone either way; its
    // detached leg survives on Linux and is swept by `Env`'s own `Drop`
    // (F4) once this test's own `env` goes out of scope below.
    let _ = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::attach_client::supervisor_client::stop(&dir)
    })
    .await;

    env.kill_daemon_bounded().await;
}

/// pty.open fires activation unconditionally even when the cached phase still
/// reads Ready; a deleted "skip if phase != Ready" guard would latch the row forever.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn capsule_attach_right_after_run_end_activates_despite_a_stale_cached_ready_phase() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("carc");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "carc-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let target = create_res.payload["session_name"].as_str().expect("session_name").to_string();
    let state_dir_path = env.state_root.join("sot").join("workspaces").join(&workspace_id);

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND).await;

    let (status, _process) =
        sot_log::attach_client::supervisor_client::query_status(&state_dir_path).expect("query_status before ending the run");
    let original_voyage = status.voyage.expect("a ready capsule has a voyage");
    sot_log::attach_client::supervisor_client::end_run(&state_dir_path, &original_voyage, "test end").expect("end_run over the lane");

    // end_run went straight to the lane, bypassing the daemon, so the phase
    // cell is still stale Ready; the guarded helper's fresh probe must decide.
    let pty_req = serde_json::json!({ "cols": 80, "rows": 24, "user_switch": true, "target": target });
    let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    next_id += 1;
    assert_eq!(pty_res.payload["code"], "attach_direct", "pty.open right after end_run: {:?}", pty_res.payload);

    // A NEW voyage is proof the guarded activation ran retire->resume->reset,
    // not a no-op against the stale cached phase.
    let ready_deadline = Instant::now() + BOUND.max(Duration::from_secs(90));
    let new_voyage = loop {
        if let Some(report) = try_query_status(state_dir_path.clone()).await {
            if report.phase == sot_log::lane::wire::SupervisorPhase::Ready {
                break report.voyage.expect("a ready capsule has a voyage");
            }
        }
        assert!(
            Instant::now() < ready_deadline,
            "timed out waiting for the row to recover to Ready despite a stale cached Ready phase at attach time"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_ne!(new_voyage, original_voyage, "reset must mint a NEW voyage, not resurrect the ended one");

    let _ = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::attach_client::supervisor_client::stop(&dir)
    })
    .await;
    env.kill_daemon_bounded().await;
}
