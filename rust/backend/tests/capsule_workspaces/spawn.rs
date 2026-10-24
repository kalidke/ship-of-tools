//! Boot adoption, fence contention, an early-dying capsule and an unlaunchable agent.

use super::*;

/// Field finding (a Windows FE box, 2026-09): the daemon boot resume-scan
/// (`resume_all`) used to skip straight to spawning `--resume` for every
/// capsule row with a fence file, with NO probe of whether that
/// row's supervisor was already alive. A capsule supervisor is spawned
/// DETACHED (ADR 0042) and survives its daemon by design, so an FE
/// relaunch that reboots the LOCAL daemon left every existing supervisor
/// running — and the rebooted daemon's `resume_all` then raced a brand
/// new `--resume` leg straight into the still-live `supervisor.lock`
/// fence. `sot-capsule supervise` failed that fence acquisition FAST
/// (`sot_capsule::supervisor::fence::lock_supervisor`, `rust/capsule/src/supervisor/`) and
/// (round-1 of this fix) exited `EXIT_TERMINAL` (69) within a couple
/// hundred ms; the daemon's watchdog treated 69 as unconditionally
/// terminal (rule F — never re-diagnosed) and marked the row
/// `capsule_terminal`, so `workspace.list` reported the row PERMANENTLY
/// terminal even though the OLD supervisor — the one actually running
/// the FE's attached session — never stopped. Round 2 additionally gave
/// fence contention its own exit code (`EXIT_CONTENDED`, 70, distinct
/// from terminal) for the narrower race a pre-spawn probe alone cannot
/// close (the old lane going quiet before its fence actually releases)
/// — this test's own scenario never reaches that path at all, since
/// `resume_all`'s probe here finds the lane still answering and adopts
/// it directly, spawning nothing.
///
/// Unlike this file's own `..._adopt_and_destroy` test above (which
/// deliberately STOPS the supervisor authority before restarting the
/// daemon, proving `sot-capsule`'s own leg-adoption on a genuinely dead
/// lane), this test leaves the supervisor authority ALIVE across the
/// daemon restart — the field bug's exact precondition. It proves the
/// fix: the reboot must ADOPT the still-answering lane (no second leg
/// spawned, same leg epoch, and the row's reported phase stays whatever
/// the live supervisor actually reports) rather than ever reading
/// "terminal" for a workspace nothing has failed.
#[tokio::test]
async fn capsule_workspace_boot_adopts_a_still_alive_supervisor_without_spawning_a_second_one() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-capsule --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("bas");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "bas-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(
        create_res.payload.get("error").is_none(),
        "workspace.create failed: {:?}",
        create_res.payload
    );
    let workspace_id = create_res.payload["workspace_id"]
        .as_str()
        .expect("workspace_id")
        .to_string();

    let list_deadline = Instant::now() + BOUND.max(Duration::from_secs(90));
    let state_dir = loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({}))
            .await
            .payload;
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

    let run_before = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || {
            sot_capsule::attach_client::supervisor_client::query_status(&dir)
                .expect("query_status before daemon restart")
                .0
                .run
        }
    })
    .await
    .unwrap()
    .expect("a ready capsule has a run");

    // The key difference from the create/list/attach/adopt/destroy
    // test's own adoption proof (which stops the authority first): here
    // it is deliberately left ALIVE across the restart — the field
    // bug's exact precondition — reproduced via the SAME
    // restart_daemon_and_prove_adoption helper that test uses (round-2
    // fold: this test's own "never terminal, same run" regression
    // proof is now that shared helper's `Alive` arm; nothing past this
    // point needs `conn2`/`next_id2`, so both are discarded).
    let (_conn2, _next_id2) = restart_daemon_and_prove_adoption(
        &env,
        conn,
        &workspace_id,
        &state_dir,
        &state_dir_path,
        run_before,
        AuthorityAtRestart::Alive,
    )
    .await;

    // Best-effort stop of the still-detached supervisor (see the
    // default-workspace test's own comment above) — this test never
    // spawned a second leg to worry about, only the one adopted one.
    let _ = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_capsule::attach_client::supervisor_client::stop(&dir)
    })
    .await;

    env.kill_daemon_bounded().await;
}

/// Round-2 Codex finding: the pre-spawn probe alone (the boot-adopts
/// test above) cannot close the narrower race where the OLD lane has
/// already gone quiet but its fence has not yet released (`sot-capsule
/// supervise` drops its lane BEFORE releasing `supervisor.lock` — up to
/// `transport::TEARDOWN_AGGREGATE_DEADLINE`, 20s). A spawn that starts
/// into that window must exit `EXIT_CONTENDED` (70), never
/// `EXIT_TERMINAL` (69), and the daemon's watchdog must re-probe for
/// adoption rather than immediately marking the row `capsule_terminal`.
///
/// This test creates the contention DIRECTLY — no timing race needed —
/// using a "fake lock holder": `sot_capsule::supervisor::fence::lock_supervisor` is
/// `pub`, so this test pre-holds `supervisor.lock` at a workspace's
/// state dir from THIS TEST PROCESS itself, a real cross-process kernel
/// lock that `supervise_inner` acquires as its very FIRST act — BEFORE
/// it ever consults `--start` vs `--resume` (`rust/capsule/src/supervisor/`)
/// — so the contention this proves is identical whichever mode the next
/// spawn uses.
///
/// 2026-09-04 amendment: no longer the DEFAULT workspace. Before this
/// amendment, the default row's fixed, known-ahead-of-boot identity gave
/// this test a state dir that was fence-free before its own first
/// attach — the one way to pre-fence a workspace before its FIRST spawn,
/// since a created workspace's `workspace_id` (hence its state dir) is
/// only known AFTER `workspace.create` returns, and that handler spawns
/// synchronously as part of creation itself, too late to pre-fence. The
/// default row is now the inert anchor when it has no agent (see
/// `capsule_default_workspace_with_no_agent_is_never_started_on_attach`)
/// and can no longer be used this way. This test instead creates an
/// ordinary row, lets it reach Ready once normally, STOPS its authority
/// (the leg, and the run's own end record, both survive — ADR
/// 0041 Lifecycle), THEN pre-fences its now-EXISTING state dir: the next
/// attach needs a RESUME spawn (fence file left, lane dead) rather
/// than a fresh Start, but the fence check above runs before mode is
/// ever consulted either way, so this is the exact same contention this
/// test always proved, just reached via `--resume` instead of `--start`.
#[tokio::test]
async fn capsule_supervisor_spawn_survives_fence_contention_without_marking_terminal() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-capsule --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("cnt");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "cnt-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(
        create_res.payload.get("error").is_none(),
        "workspace.create failed: {:?}",
        create_res.payload
    );
    let workspace_id = create_res.payload["workspace_id"]
        .as_str()
        .expect("workspace_id")
        .to_string();
    let target = create_res.payload["session_name"]
        .as_str()
        .expect("session_name")
        .to_string();

    let list_deadline = Instant::now() + BOUND.max(Duration::from_secs(90));
    let state_dir = loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({}))
            .await
            .payload;
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

    // Stop the supervisor authority over its own lane — its capsule leg
    // and the run's own end record both survive (ADR 0041
    // Lifecycle) — so the NEXT attach needs a RESUME spawn, straight
    // into the fence this test is about to pre-hold.
    tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_capsule::attach_client::supervisor_client::stop(&dir).expect("stop the supervisor authority")
    })
    .await
    .unwrap();
    poll_until(
        || {
            let dir = state_dir_path.clone();
            async move { if try_query_status(dir).await.is_none() { Some(()) } else { None } }
        },
        BOUND,
        "the stopped supervisor's own lane to go silent",
    )
    .await;
    // Wait for the daemon's observer too -- a stale "ready" here would skip
    // the spawn attempt this test exists to prove.
    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "unreachable", BOUND).await;

    // The fake lock holder itself: held for this test's whole remaining
    // body, released only at the very end. The state dir already exists
    // (the earlier real spawn created it) — no `create_dir_all` needed.
    let fake_lock = sot_capsule::supervisor::fence::lock_supervisor(&state_dir_path)
        .expect("pre-hold the fence from the test process");

    let log_from = env.daemon_log_len();
    let pty_req = serde_json::json!({
        "cols": 80, "rows": 24, "user_switch": true, "target": target,
    });
    let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    next_id += 1;
    assert_eq!(
        pty_res.payload["code"], "attach_direct",
        "pty.open payload: {:?}",
        pty_res.payload
    );

    // The contended start ends in one observed fact: on Unix the durable parent forks nothing, and the attach, after
    // waiting on the claimed fence (25 passes of 200 ms, `ADMISSION_PENDING_PASSES`), reports the row's authority
    // pending; on Windows the supervisor is created directly and exits contended (70), which its watchdog logs. The row
    // must NEVER read "terminal" (this test's own regression proof) until then, nor for three observer rounds (2 s
    // each) after it, by when every path that could latch it has run.
    let pending_deadline = Instant::now() + BOUND;
    let mut pending_at: Option<Instant> = None;
    loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({}))
            .await
            .payload;
        let row = find_row(&payload, &workspace_id).expect("the contended row stays listed");
        assert_ne!(
            row["phase"].as_str(),
            Some("terminal"),
            "a spawn that lost a contended fence must never be marked terminal -- row: {row:?}"
        );
        let pending = row["activation_error"].as_str().is_some_and(|e| e.contains("authority is pending"))
            || env.daemon_log_since(log_from).contains("exited contended (70)");
        if pending && pending_at.is_none() {
            pending_at = Some(Instant::now());
        }
        if pending_at.is_some_and(|t| t.elapsed() >= Duration::from_secs(6)) {
            break;
        }
        assert!(
            Instant::now() < pending_deadline,
            "the contended start never ended (no pending authority, no contended exit) -- row: {row:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    drop(fake_lock);
    env.kill_daemon_bounded().await;
}

/// The gap this test proves closed: a capsule row whose agent argv can
/// never launch was UNENDABLE from the UI. `sot-capsule supervise`'s own
/// anti-flap bound (`FLAP_THRESHOLD` == 3, `respawn_or_terminal` in
/// `rust/capsule/src/supervisor/`) trips within milliseconds of a real
/// `CreateProcess` failure and enters sticky `Lifecycle::Terminal`,
/// self-exiting `TERMINAL_EXIT_GRACE` (2s) later with no external `stop`
/// ever required -- so by the time a `workspace.list` poll (or a user)
/// ever observes phase "terminal", the authority process has almost
/// always ALREADY exited. Before this PR, `rows::run::end_run::end_run`'s
/// wrapper only ever handled a LIVE lane answering `phase: Terminal`
/// (sending it `stop` and reporting a confirmed end); a lane that had
/// already gone fully silent by the time `workspace.destroy` reached it
/// surfaced as "supervisor lane unreachable" and was reported `Kept`
/// forever -- the row could never actually be destroyed. On Windows this
/// test scrubs `claude` from the daemon's PATH to force the failure
/// deterministically (no new fixture machinery). On
/// The supervisor-epoch rule's addition (b), end to end: a capsule
/// that dies inside its OWN bootstrap -- before it ever binds its lane,
/// so no `status` is ever served and no identity ever reaches this
/// daemon -- still latches the row `terminal` rather than leaving it
/// `stopped` to re-spawn the same instant failure on every attach. This
/// class is not hypothetical: a capsule dying at bootstrap is exactly
/// how the state-root failure on a shared home presented, and `terminal`
/// on the row was the signal the operator read.
///
/// The trigger is the supervisor's test seam `SOT_TEST_SUPERVISOR_BOOT_FAILS`, named
/// on the daemon's environment (which reaches the supervisor): it fails
/// right after the supervisor makes its state folder, BEFORE the
/// authority fence and the lane bind, and the supervisor returns
/// `EXIT_TERMINAL` having answered nothing. A seam rather than a
/// permission bit on purpose -- `chmod` proves nothing when the suite
/// happens to run as root.
///
/// Note what CANNOT explain a pass here: with no answered status there
/// is no watch record naming the supervisor, so the mark is the
/// watchdog's own, under the row's guard, once nothing answers. This
/// test is the guard on it.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_capsule_that_dies_before_its_lane_answers_still_latches_the_row_terminal() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-capsule --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("tub");
    // A REAL (if unlaunchable) agent, so the default row is not the
    // inert anchor -- the same precondition the unlaunchable-agent test
    // below establishes, and for the same reason. The agent itself never
    // runs here: its supervisor dies first.
    env.seed_default_capsule_toml("claude");
    let fake_claude_dir = env.seed_fake_unlaunchable_claude();
    env.spawn_sotd_with_path_and_env(&fake_claude_dir, &[("SOT_TEST_SUPERVISOR_BOOT_FAILS", "1")]);
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

    let pty_req = serde_json::json!({
        "cols": 80, "rows": 24, "user_switch": true, "target": default_target,
    });
    let _ = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    next_id += 1;

    let terminal_deadline = Instant::now() + BOUND;
    loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, &default_workspace_id) {
            if row["phase"].as_str() == Some("terminal") {
                break;
            }
            assert!(
                Instant::now() < terminal_deadline,
                "a capsule that died before answering its lane left the row at {:?}, never \"terminal\"",
                row["phase"]
            );
        }
        assert!(Instant::now() < terminal_deadline, "timed out waiting for the row to latch \"terminal\"");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Linux a genuinely absent `claude` does NOT reproduce the same
/// scenario: `agent_argv`'s own resolution step (`resolve_claude`)
/// refuses at the DAEMON level instead, before `sot-capsule` is ever
/// spawned at all -- so this test's Linux leg instead resolves to a fake,
/// deliberately-broken `claude` stub (`Env::seed_fake_unlaunchable_claude`)
/// that IS resolvable+executable but fails every time it actually runs,
/// reaching the SAME anti-flap/Terminal path through `sot-capsule`'s own
/// internal retry logic. Either way: seed the default row's own toml
/// with `agent = "claude"` before boot -- a REAL (if unlaunchable) agent,
/// so this row is NOT the 2026-09-04 inert-anchor amendment's concern
/// (that only ever applies to `agent == "none"`; see
/// `capsule_default_workspace_with_no_agent_is_never_started_on_attach`)
/// -- attach it once to trigger start-on-attach, and prove (1) the row
/// reaches phase "terminal" within a bound rather than cycling
/// Starting -> Terminal forever, and (2) `workspace.destroy` on it then
/// succeeds and the supervisor's own lane goes silent.
#[tokio::test]
async fn capsule_row_with_an_unlaunchable_agent_reaches_terminal_and_is_destroyable() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-capsule --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("ult");
    // Pre-write the default row's own toml with `agent = "claude"` BEFORE
    // boot -- `rows/anchor.rs`'s fresh-boot seed already picks this agent on
    // Windows, but pre-writing it here makes the precondition explicit
    // and independent of that default ever changing.
    env.seed_default_capsule_toml("claude");
    // Windows: the daemon's PATH is scrubbed of every directory holding a
    // `claude`, so the literal argv `agent_argv` hands `sot-capsule` fails
    // to spawn every time, on a box with the agent installed as on one
    // without -- never assumed absent.
    #[cfg(windows)]
    {
        let scrubbed = path_without(&["claude.exe", "claude.cmd", "claude"]);
        env.spawn_sotd_with_env(&[("PATH", scrubbed.to_str().expect("scrubbed PATH is not UTF-8")), SHORT_BACKOFF]);
    }
    // Linux: `agent_argv`'s own resolution step means a genuinely absent
    // `claude` would refuse at the DAEMON level instead (never reaching
    // `sot-capsule`'s own anti-flap/Terminal logic this test exercises)
    // -- a fake, deliberately-broken but resolvable `claude` reproduces
    // the same "unlaunchable agent" scenario portably (see
    // `Env::seed_fake_unlaunchable_claude`'s own doc).
    #[cfg(target_os = "linux")]
    let fake_claude_dir = env.seed_fake_unlaunchable_claude();
    #[cfg(target_os = "linux")]
    env.spawn_sotd_with_path_and_env(&fake_claude_dir, &[SHORT_BACKOFF]);
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
    let default_workspace_id = default_row["workspace_id"].as_str().expect("workspace_id").to_string();
    let default_target = default_row["session_name"].as_str().expect("session_name").to_string();
    let state_dir_path = env.state_root.join("sot").join("workspaces").join(&default_workspace_id);

    // Trigger start-on-attach: the capsule's producer (`claude`) will
    // fail to spawn every time this daemon retries it.
    let pty_req = serde_json::json!({
        "cols": 80, "rows": 24, "user_switch": true, "target": default_target,
    });
    let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    next_id += 1;
    assert_eq!(pty_res.payload["code"], "attach_direct", "pty.open payload: {:?}", pty_res.payload);

    // (1) The row reaches phase "terminal" within a bound -- never
    // cycling Starting -> Terminal -> Starting forever, and never resting
    // at "ended_no_respawn" on the way (an agent that cannot start is not
    // an agent that was asked to end). The bound is the daemon watchdog's
    // restart backoffs (shortened to 200 ms by `SHORT_BACKOFF`) and settles
    // plus the suite's own `BOUND`: whichever capsule ends the row, the one
    // restart budget is the longest it can take.
    let terminal_deadline = Instant::now() + BUDGET_WAIT;
    loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, &default_workspace_id) {
            assert_ne!(
                row["phase"].as_str(),
                Some("ended_no_respawn"),
                "an agent that cannot start must never rest as ended_no_respawn: {row:?}"
            );
            if row["phase"].as_str() == Some("terminal") {
                break;
            }
        }
        assert!(
            Instant::now() < terminal_deadline,
            "timed out waiting for the unlaunchable-agent capsule row to reach phase \"terminal\""
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // The row ended `terminal` because the watchdog's restart budget ran out, not because the supervisor gave up on its
    // own: the budget's own line is in the daemon's log.
    let log = env.daemon_log_since(0);
    assert!(log.contains("restart budget exhausted"), "the daemon's log must hold the watchdog's budget line; got:\n{log}");

    // LU5a (ADR 0043 decision 25, Linux leg): the supervisor's own stderr
    // is the daemon's own log -- by the time this row is terminal, its
    // supervisor has necessarily written at least one diagnostic line
    // (the anti-flap/watchdog trail this whole test exercises), prefixed
    // with THIS workspace's own id so concurrent rows sharing the one
    // daemon log stay distinguishable.
    #[cfg(target_os = "linux")]
    {
        let log_path = env.state_root.join("sot").join("sotd.log");
        let log_contents = std::fs::read_to_string(&log_path)
            .unwrap_or_else(|e| panic!("could not read the daemon's own log {log_path:?}: {e}"));
        let wanted = format!("sot-capsule supervise[{default_workspace_id}]");
        assert!(
            log_contents.contains(&wanted),
            "expected {log_path:?} to contain a line with {wanted:?}; got:\n{log_contents}"
        );
    }

    // (2) `workspace.destroy` on a terminal row now succeeds (never the
    // typed `capsule_end_not_reached` error this gap used to produce
    // forever) -- the default row's own branch: kept (never deleted),
    // but its run is confirmed ended.
    let destroy_req = serde_json::json!({ "workspace_id": default_workspace_id });
    // `next_id` has no further use on this connection (mirrors the
    // create/list/destroy test above) -- no further increment.
    let destroy_res = call(&mut conn, next_id, op::WORKSPACE_DESTROY, destroy_req).await;
    assert!(
        destroy_res.payload.get("error").is_none(),
        "workspace.destroy on a terminal capsule row must succeed: {:?}",
        destroy_res.payload
    );
    assert!(
        destroy_res.payload.get("kept").and_then(|v| v.as_str()).is_some(),
        "default row destroy must report kept: {:?}",
        destroy_res.payload
    );

    // The supervisor's own lane is silent -- no resident `sot-capsule.exe`
    // leaked behind a row the UI now reports gone.
    poll_until(
        || {
            let dir = state_dir_path.clone();
            async move { if try_query_status(dir).await.is_none() { Some(()) } else { None } }
        },
        BOUND,
        "the terminal row's supervisor lane to be silent after workspace.destroy",
    )
    .await;

    env.kill_daemon_bounded().await;
}

/// The restart budget is a count, not a rate: an agent that cannot start ends the row `terminal` after five restarts of
/// supervisors that never reached `ready`, however long each restart's settle takes. Each settle here takes four seconds
/// longer (`SOT_TEST_SETTLE_DELAY_MS`), which a budget measured in a 60 s window cannot survive: the backoffs alone are
/// 56 s.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn an_unlaunchable_agent_ends_the_row_terminal_after_five_restarts_however_slow_the_settles() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("ulslow");
    env.seed_default_capsule_toml("claude");
    let fake_claude_dir = env.seed_fake_unlaunchable_claude();
    env.spawn_sotd_with_path_and_env(&fake_claude_dir, &[("SOT_TEST_SETTLE_DELAY_MS", "3000"), SHORT_BACKOFF]);
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

    // The short backoffs, six settles of 3 s (plus their own 2 s) and the suite's own bound.
    let terminal_deadline = Instant::now() + Duration::from_secs(6 * 5 + 5) + BOUND;
    loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, &default_workspace_id) {
            if row["phase"].as_str() == Some("terminal") {
                break;
            }
        }
        assert!(Instant::now() < terminal_deadline, "the row never reached terminal: the budget is not a count");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let log = env.daemon_log_since(0);
    assert!(log.contains("restart budget exhausted"), "the daemon's log must hold the watchdog's budget line; got:\n{log}");

    env.kill_daemon_bounded().await;
}

/// Never reaching `ready` is what the budget counts, not how long a supervisor lived: each supervisor here is held in
/// `starting` for three seconds (`SOT_TEST_RECOVERY_DELAY_MS`) before its agent, which cannot start, ends it, so it lives
/// seconds, longer than any agent that cannot start, and never reaches `ready`. The row still ends `terminal` after five such
/// restarts.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_slow_failing_supervisor_that_never_reaches_ready_still_counts_toward_terminal() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("ulslowstart");
    env.seed_default_capsule_toml("claude");
    let fake_claude_dir = env.seed_fake_unlaunchable_claude();
    env.spawn_sotd_with_path_and_env(&fake_claude_dir, &[("SOT_TEST_RECOVERY_DELAY_MS", "3000"), SHORT_BACKOFF]);
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

    // The short backoffs, six starts held for 3 s, their settles (2 s each) and the suite's own bound.
    let terminal_deadline = Instant::now() + Duration::from_secs(6 * 3 + 6 * 2 + 5) + BOUND;
    loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, &default_workspace_id) {
            if row["phase"].as_str() == Some("terminal") {
                break;
            }
        }
        assert!(Instant::now() < terminal_deadline, "the row never reached terminal: a supervisor that lives long without reaching ready was not counted");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let log = env.daemon_log_since(0);
    assert!(log.contains("restart budget exhausted"), "the daemon's log must hold the watchdog's budget line; got:\n{log}");

    env.kill_daemon_bounded().await;
}

/// A row socket that cannot be bound, and keeps failing, is a row that does not start: its supervisor exits 75 before
/// any agent starts and rows' watchdog counts each one against its one restart budget, which ends the row terminal.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn a_row_socket_that_cannot_be_bound_ends_the_row_terminal_through_the_budget() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first", sot_capsule_exe());

    let env = Env::new("rsb");
    env.seed_default_capsule_toml("claude");
    let fake_claude_dir = env.seed_fake_unlaunchable_claude();
    env.spawn_sotd_with_path_and_env(&fake_claude_dir, &[SHORT_BACKOFF]);
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
    let workspace_id = default_row["workspace_id"].as_str().expect("workspace_id").to_string();
    let target = default_row["session_name"].as_str().expect("session_name").to_string();
    // A folder where the row's socket belongs: every supervisor of this row fails to bind it.
    let state_dir = env.state_root.join("sot").join("workspaces").join(&workspace_id);
    let blocker = sot_capsule::row::socket_path_in(env._runtime_tmp.path(), &sot_platform::host::state_dir::state_dir_hash(&state_dir))
        .expect("the row socket path fits");
    std::fs::create_dir(&blocker).expect("block the row socket");

    let pty_req = serde_json::json!({"cols": 80, "rows": 24, "user_switch": true, "target": target});
    let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    next_id += 1;
    assert_eq!(pty_res.payload["code"], "attach_direct", "pty.open payload: {:?}", pty_res.payload);

    let terminal_deadline = Instant::now() + BUDGET_WAIT;
    loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, &workspace_id) {
            if row["phase"].as_str() == Some("terminal") {
                break;
            }
        }
        assert!(
            Instant::now() < terminal_deadline,
            "a row whose socket cannot be bound never reached \"terminal\" through the budget; the daemon's log:\n{}",
            env.daemon_log_since(0)
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let log = env.daemon_log_since(0);
    assert!(log.contains("restart budget exhausted"), "the budget, not the first failure, ended the row; the daemon's log:\n{log}");
    assert!(log.contains("could not bind the row socket"), "the supervisor said why:\n{log}");
    env.kill_daemon_bounded().await;
}

/// An agent that writes a line and exits within two seconds has not reached `ready` for having written: `ready` is the
/// agent having lived 2 s. Five such starts in a row spend the one restart budget as a count, and the row is `terminal`.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn an_agent_that_writes_and_exits_within_two_seconds_counts_as_never_ready() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("wex");
    env.seed_default_capsule_toml("claude");
    let fake_claude_dir = env.seed_fake_writing_claude();
    env.spawn_sotd_with_path_and_env(&fake_claude_dir, &[SHORT_BACKOFF]);
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
    let workspace_id = default_row["workspace_id"].as_str().expect("workspace_id").to_string();
    let target = default_row["session_name"].as_str().expect("session_name").to_string();
    let pty_req = serde_json::json!({"cols": 80, "rows": 24, "user_switch": true, "target": target});
    let _ = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    next_id += 1;

    let deadline = Instant::now() + BUDGET_WAIT;
    loop {
        let payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        next_id += 1;
        let row = find_row(&payload, &workspace_id).expect("the default row");
        if row["phase"] == "terminal" {
            break;
        }
        assert_ne!(row["phase"], "ended_no_respawn", "an agent that cannot run is not one that was asked to end: {row:?}");
        assert!(Instant::now() < deadline, "an agent that writes and exits never ended the row terminal: {row:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let log = env.daemon_log_since(0);
    assert!(log.contains("restart budget exhausted"), "the watchdog's budget line is missing; got:\n{log}");
    env.kill_daemon_bounded().await;
}
