//! Platform behaviour: Env drop, state-root qualification, job breakaway, user service, process group, scope guard, degrade.

use super::*;

/// One `MgmtRequest::Status` round trip against the LEG's own real
/// voyage socket (`sot_log::lane::socket_unix::connect_voyage_socket`, ADR
/// 0043 decision 8 steps 1-3: connect + same-user auth, the ordinary
/// step-5-client-facing constructor) — the WIRE value the degrade test
/// proves against (Codex SHOULD-FIX: `/proc/<pid>/cmdline` text does not
/// prove propagation onto the wire; restoring the deleted Unix survival
/// clamp would still leave a cmdline-only check green). Distinct from
/// [`try_query_status`]'s own `sot_log::attach_client::supervisor_client::query_status`:
/// that is the SUPERVISOR's own status (`SupervisorReply::StatusOk`,
/// which carries no `survival` field at all) — `survival` lives only on
/// `MgmtReply::StatusOk`, the LEG's own mgmt lane. The SOM0 mgmt lane
/// has no hello frame (unlike the supervisor/attach lanes) — `connect_
/// voyage_socket`'s own same-user auth already happened before this
/// function ever gets a `SocketClient` back, so the very first frame
/// sent here is the request itself. Bounded like every other wire round
/// trip in this file ([`call`]'s own `BOUND`) — the blocking body runs
/// on its own thread via `spawn_blocking`, abandoned (not cancelled) on
/// timeout, exactly [`drain_stderr_bounded`]'s own "leak, never hang"
/// tradeoff.
#[cfg(target_os = "linux")]
async fn leg_survival(voyage_id: &str) -> sot_log::lane::wire::Survival {
    let voyage_id_owned = voyage_id.to_string();
    let voyage_id_for_body = voyage_id_owned.clone();
    tokio::time::timeout(
        BOUND,
        tokio::task::spawn_blocking(move || -> sot_log::lane::wire::Survival {
            let client = sot_log::lane::socket_unix::connect_voyage_socket(&voyage_id_for_body)
                .unwrap_or_else(|e| panic!("connect_voyage_socket({voyage_id_for_body}): {e}"));
            let body = sot_log::lane::wire::encode_mgmt_request(&sot_log::lane::wire::MgmtRequest::Status)
                .expect("MgmtRequest::Status has no fields; encoding cannot fail");
            client.write_all(&body).expect("write MgmtRequest::Status");
            let mut splitter = sot_log::lane::wire::FrameSplitter::new();
            let mut buf = [0u8; 512];
            loop {
                let n = client.read(&mut buf).expect("read mgmt reply");
                assert!(n > 0, "voyage socket closed before answering status");
                let (frames, err) = splitter.feed(&buf[..n]);
                assert!(err.is_none(), "mgmt lane wire error: {err:?}");
                for frame in frames {
                    if let sot_log::lane::wire::DecodedFrame::MgmtReply(sot_log::lane::wire::MgmtReply::StatusOk {
                        survival,
                        ..
                    }) = frame
                    {
                        return survival;
                    }
                }
            }
        }),
    )
    .await
    .unwrap_or_else(|_| panic!("leg mgmt status for {voyage_id_owned} did not answer within {BOUND:?}"))
    .expect("leg_survival's own blocking task panicked")
}

/// LU4 review round 2, F4's own cleanup contract, proved directly here
/// rather than re-asserted in every test above (F4's own "whichever is
/// smaller" — one dedicated test beats touching all six bodies). Spawns
/// one real capsule row and waits for it to reach a leg worth cleaning up
/// (so the "empty after" assertions below are not vacuously true), then
/// drops `env` explicitly and proves `Env`'s own `Drop`:
///  - swept every process matching this env's own anchored leg pattern
///    ([`Env::leg_pgrep_pattern`], never the old unanchored substring
///    match a `tail -f` could false-match),
///  - killed this env's own ISOLATED tmux server (F3) rather than ever
///    touching the developer's real one (a different socket entirely —
///    this env's daemon never saw that path at all, `SOT_TMUX_SOCK`),
///  - and removed its own temp project/state/config dir AND its own
///    `SOT_RUNTIME_DIR` tempdir (no `/tmp/sotcw-*`/`/tmp/sotrt-*` left).
/// Linux only: `pkill`/`pgrep`/`tmux` are shelled out to directly.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn capsule_backend_test_env_drop_cleans_up_legs_and_daemon() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("cln");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "cln-workspace",
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
            assert_eq!(row["runtime"], "capsule", "row: {row:?}");
            if row["phase"].as_str() == Some("ready") {
                break;
            }
        }
        assert!(
            Instant::now() < list_deadline,
            "timed out waiting for workspace.list to report phase \"ready\" for the new capsule workspace"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Not vacuous: a real `sot-capsule supervise`/leg pair matching this
    // env's own anchored pattern is alive right now.
    let pattern = env.leg_pgrep_pattern();
    assert!(
        any_process_matches(&pattern),
        "expected a live sot-capsule leg matching {pattern:?} before the cleanup guard runs"
    );

    // The bounded, deliberate daemon teardown every other test in this
    // file ends its own run with — empties `Env`'s own daemon slot, so
    // `Drop`'s own step 1 below is a no-op, exactly like every other test.
    drop(conn);
    env.kill_daemon_bounded().await;

    // F4's actual claim: dropping `env` now sweeps this env's own legs
    // (anchored, so it can never touch anything else on the box) and
    // kills its OWN isolated tmux server (harmless whether or not a
    // session ever landed there), in that order, before its own temp
    // dirs vanish — capture what we need to verify BEFORE `env` (and the
    // fields these borrow from) are gone. Deletion (round 2 reviewer
    // note): no dedicated accessor for this same-module read — `_tmp`/
    // `_runtime_tmp` are plain private fields, directly readable here.
    let tmp_root = env._tmp.path().to_path_buf();
    let runtime_root = env._runtime_tmp.path().to_path_buf();
    drop(env);

    // G3: the same bounded, sweep-until-empty shape `Drop`'s own loop
    // uses, read-only here (see `poll_until_no_process_matches`'s doc).
    assert!(
        poll_until_no_process_matches(&pattern, Duration::from_secs(2)),
        "the cleanup guard must leave no process matching {pattern:?}"
    );
    assert!(
        !tmp_root.exists(),
        "the cleanup guard must remove the env's own temp project/state/config dir: {tmp_root:?}"
    );
    assert!(
        !runtime_root.exists(),
        "the cleanup guard must remove the env's own SOT_RUNTIME_DIR temp dir: {runtime_root:?}"
    );
}

/// ADR 0043 decision 23 (LU5a): `workspace.create` with `"runtime":
/// "capsule"` is refused OUTRIGHT — before any row persists — when the
/// state root resolves onto a VOLATILE filesystem (tmpfs here; ramfs is
/// the same code path, untested for lack of an easy-to-mount ramfs in
/// CI). Linux only: tmpfs-as-state-root is a Linux-specific concern here,
/// and Windows keeps its existing, unrelated NTFS-only preflight.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn capsule_create_is_refused_on_an_unqualified_state_root() {
    if !Path::new("/dev/shm").is_dir() {
        eprintln!(
            "skipping capsule_create_is_refused_on_an_unqualified_state_root: /dev/shm is not mounted here"
        );
        return;
    }
    let _serial = SERIAL.lock().await;

    let env = Env::new_with_state_root_on_tmpfs("cur");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let label = "cur-workspace";
    let create_req = serde_json::json!({
        "label": label,
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert_eq!(
        create_res.payload["code"], "state_root_unqualified",
        "payload: {:?}", create_res.payload
    );
    let error_text = create_res.payload["error"].as_str().expect("error text");
    assert!(error_text.contains("XDG_STATE_HOME"), "{error_text}");
    assert!(error_text.contains("tmpfs"), "{error_text}");
    assert!(
        create_res.payload.get("workspace_id").is_none(),
        "a refused create must mint no workspace_id: {:?}", create_res.payload
    );

    // workspace.list: no row for this (never-created) workspace. Getting a
    // real answer back at all (rather than a dead connection) is itself the
    // proof the refusal above was per-request, never a daemon-wide wedge —
    // no separate create-on-the-same-daemon round trip is needed for that
    // (a "tmux" one no longer would even succeed here: ADR 0046 decision 5
    // refuses a NEW tmux row wherever the capsule runtime compiles, this
    // Linux leg included).
    let ws_slug = slug(label);
    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    let has_row = list_payload["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|w| w["slug"] == ws_slug);
    assert!(!has_row, "a refused create must not appear in workspace.list: {list_payload:?}");

    // No toml persisted for it either.
    let toml_path = env
        .app_config_dir()
        .join(format!("workspaces-{TEST_STATE_HOST}"))
        .join(format!("{ws_slug}.toml"));
    assert!(!toml_path.exists(), "a refused create must not persist a toml: {toml_path:?}");

    env.kill_daemon_bounded().await;
}

/// ADR 0043 decision 32 (revised): a daemon that finds itself inside a job
/// forbidding breakaway is never refused the launch over its own
/// containment — it retries the spawn without `CREATE_BREAKAWAY_FROM_JOB`,
/// logs once, and the row still reaches "ready" (`--survival degraded`).
/// Honest for a hosted CI runner too, which keeps its own processes inside
/// a job lacking `JOB_OBJECT_LIMIT_BREAKAWAY_OK` regardless of this test's
/// own explicit assignment below — the row reaching ready is asserted
/// unconditionally, and containment is then proven directly
/// (`IsProcessInJob` against the ONE job this test built, standing in for
/// a capsule's own leg job) rather than assumed from the create call
/// alone.
#[tokio::test]
#[cfg(windows)]
async fn create_from_inside_a_job_that_forbids_breakaway_still_reaches_ready() {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    let _serial = SERIAL.lock().await;
    let env = Env::new("breakaway-contained");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    // A job with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` only -- no
    // `JOB_OBJECT_LIMIT_BREAKAWAY_OK` -- standing in for a capsule's own
    // leg job that this `sotd` finds itself launched inside.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    assert!(!job.is_null(), "CreateJobObjectW: {}", std::io::Error::last_os_error());
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let ok = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    assert!(ok != 0, "SetInformationJobObject: {}", std::io::Error::last_os_error());
    let daemon_handle = {
        let daemon = env.daemon.borrow();
        daemon.as_ref().expect("daemon spawned").as_raw_handle() as HANDLE
    };
    let ok = unsafe { AssignProcessToJobObject(job, daemon_handle) };
    assert!(ok != 0, "AssignProcessToJobObject(sotd): {}", std::io::Error::last_os_error());

    let create_req = serde_json::json!({
        "label": "breakaway-contained-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(
        create_res.payload.get("error").is_none(),
        "a job that forbids breakaway must never refuse the launch: {:?}", create_res.payload
    );
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND).await;

    let state_dir = state_dir_from_list(&mut conn, &mut next_id, &workspace_id).await;
    let (_status, process) =
        tokio::task::spawn_blocking(move || sot_log::attach_client::supervisor_client::query_status(&state_dir))
            .await
            .unwrap()
            .expect("query_status after ready");
    let supervisor = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process.pid()) };
    assert!(!supervisor.is_null(), "OpenProcess({}): {}", process.pid(), std::io::Error::last_os_error());
    let mut in_job: i32 = 0;
    let ok = unsafe { IsProcessInJob(supervisor, job, &mut in_job) };
    assert!(ok != 0, "IsProcessInJob: {}", std::io::Error::last_os_error());
    assert_eq!(in_job, 1, "the contained supervisor must stay in the job it could not break away from");

    unsafe {
        CloseHandle(supervisor);
        CloseHandle(job);
    }
    env.kill_daemon_bounded().await;
}

/// ADR 0043 decision 32 (lane L2), test 1: the actual proof the Linux
/// escape works — a supervisor spawned under a REAL `systemd --user`
/// service (never the live `sotd`; a uniquely-named scratch unit this
/// test alone starts and stops) survives that unit being stopped, because
/// it left the unit's own cgroup for its own transient scope at spawn
/// time (`systemd-run --user --scope`). Skips loudly (never silently)
/// when this host has no reachable `systemd --user` manager at all (a
/// bare CI container, commonly) — the ONE test in this suite that needs a
/// real answer to that question; force it with
/// `SOT_TEST_REQUIRE_USER_MANAGER=1` wherever a real user manager is
/// expected to exist.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn capsule_supervisor_survives_a_real_user_service_stop() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );
    if let Err(e) = user_manager_available_for_test() {
        if std::env::var("SOT_TEST_REQUIRE_USER_MANAGER").as_deref() == Ok("1") {
            panic!("SOT_TEST_REQUIRE_USER_MANAGER=1 but no user manager is reachable: {e}");
        }
        eprintln!("SKIPPED: no user manager: {e}");
        return;
    }

    let env = Env::new("uss");
    let (unit, daemon_pid) = env.spawn_sotd_as_user_service();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "uss-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND).await;
    let state_dir = state_dir_from_list(&mut conn, &mut next_id, &workspace_id).await;

    let (status, process) = tokio::task::spawn_blocking({
        let dir = state_dir.clone();
        move || sot_log::attach_client::supervisor_client::query_status(&dir)
    })
    .await
    .unwrap()
    .expect("query_status before stopping the daemon's own unit");
    let leg_before = status.leg.expect("a leg epoch on a ready row");
    let pid = process.pid();
    drop(process);

    // The supervisor left the DAEMON's own unit's cgroup for its own
    // transient scope at spawn time (ADR 0043 decision 32) -- proven
    // BEFORE the stop, not merely inferred from surviving it.
    let cgroup = cgroup_rel(pid);
    let last_segment = cgroup.rsplit('/').next().unwrap_or("");
    assert!(
        last_segment.starts_with("sot-row-") && last_segment.ends_with(".scope"),
        "supervisor's own cgroup does not end in a sot-row-*.scope (still inside the daemon's own unit?): {cgroup:?}"
    );
    assert!(
        !cgroup.contains(&unit),
        "supervisor's own cgroup still names the daemon's own unit {unit:?}: {cgroup:?}"
    );

    stop_user_service(&unit, daemon_pid);
    env.forget_user_service();

    // 3 s SUSTAINED (never a single lucky sample): the supervisor's own
    // lane keeps answering and its leg keeps running for the WHOLE
    // window, proving survival actually crossed the unit stop rather than
    // merely outliving it by a race.
    let run_pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "run", &env.state_root);
    let sustain_deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let alive = try_query_status(state_dir.clone()).await.is_some() && any_process_matches(&run_pattern);
        assert!(alive, "supervisor lane or its leg went away within 3s of the daemon's own unit stopping");
        if Instant::now() >= sustain_deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    drop(conn);
    env.spawn_sotd();
    let (mut conn2, mut next_id2) = connect_and_hello(&env.socket_path).await;
    poll_for_phase(&mut conn2, &mut next_id2, &workspace_id, "ready", BOUND.max(Duration::from_secs(90))).await;

    let leg_after = tokio::task::spawn_blocking({
        let dir = state_dir.clone();
        move || sot_log::attach_client::supervisor_client::query_status(&dir).expect("query_status after restart").0.leg
    })
    .await
    .unwrap();
    assert_eq!(
        leg_after,
        Some(leg_before),
        "the leg epoch changed across the daemon restart -- a fresh leg was spawned, not adopted"
    );

    env.kill_daemon_bounded().await;
}

/// A4b: a destroy ends EVERY process the row started, including a
/// descendant that left the agent's process group with `setsid`, which
/// the leg's `killpg` cannot reach: the row's own systemd scope is its
/// kill domain. The fake `claude` starts such an escapee; after the
/// destroy the row's scope must read `populated 0` or be gone. Skips
/// loudly, like the user-service test above, where no user manager is
/// reachable (force with `SOT_TEST_REQUIRE_USER_MANAGER=1`).
#[tokio::test]
#[cfg(target_os = "linux")]
async fn destroy_ends_a_child_that_left_the_agents_process_group() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );
    if let Err(e) = user_manager_available_for_test() {
        if std::env::var("SOT_TEST_REQUIRE_USER_MANAGER").as_deref() == Ok("1") {
            panic!("SOT_TEST_REQUIRE_USER_MANAGER=1 but no user manager is reachable: {e}");
        }
        eprintln!("SKIPPED: no user manager: {e}");
        return;
    }

    let env = Env::new("esc");
    let (dir, pidfile) = env.seed_fake_claude_with_escapee();
    env.spawn_sotd_with_prepended_path(&dir);
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "esc-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
        "agent": "claude",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND).await;
    let state_dir = state_dir_from_list(&mut conn, &mut next_id, &workspace_id).await;
    let (_status, process) = tokio::task::spawn_blocking({
        let dir = state_dir.clone();
        move || sot_log::attach_client::supervisor_client::query_status(&dir)
    })
    .await
    .unwrap()
    .expect("query_status on a ready row");
    let pid = process.pid();
    drop(process);

    // The guard is also the isolation assertion: production's aim rule
    // accepts the scope only as this row's, never this test's own cgroup
    // or an ancestor of it.
    let scope = cgroup_rel(pid);
    let guard = arm_scope_guard(&scope, &state_dir);

    let deadline = Instant::now() + Duration::from_secs(10);
    let escapee: u32 = loop {
        if let Some(e) = std::fs::read_to_string(&pidfile).ok().and_then(|t| t.trim().parse().ok()) {
            break e;
        }
        assert!(Instant::now() < deadline, "the escapee never wrote {pidfile:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(cgroup_rel(escapee), scope, "the escapee is not in the row's scope");
    assert_eq!(
        unsafe { libc::getsid(escapee as i32) },
        escapee as i32,
        "the escapee did not leave the agent's session"
    );

    let destroy = call(&mut conn, next_id, op::WORKSPACE_DESTROY, serde_json::json!({ "workspace_id": workspace_id })).await;
    next_id += 1;
    assert!(destroy.payload.get("error").is_none(), "workspace.destroy failed: {:?}", destroy.payload);

    guard.assert_empties(Duration::from_secs(5)).await;

    // The daemon survived the kill of the row's scope.
    let list = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await;
    assert!(list.payload.get("error").is_none(), "workspace.list after the destroy: {:?}", list.payload);
    env.kill_daemon_bounded().await;
}

/// A4b: the test scope guard refuses every target production's aim rule
/// refuses — one table over one `#[path]` source — so a test's `Drop` can
/// never write `cgroup.kill` outside the scope of a row the test created.
#[test]
#[cfg(target_os = "linux")]
fn scope_guard_refuses_everything_the_aim_rule_refuses() {
    let state = tempfile::tempdir().expect("tempdir");
    let h = sot_log::host::state_dir::state_dir_hash(state.path());
    for (target, own, accepted) in row_scope_aim::aim_table(&h) {
        assert_eq!(row_scope_aim::aim(&target, &own, &h).is_ok(), accepted, "aim on {target:?} with own {own:?}");
        let armed = std::panic::catch_unwind(|| aim_scope_guard(&target, &own, state.path()));
        assert_eq!(armed.is_ok(), accepted, "arm_scope_guard's aim on {target:?} with own {own:?}");
    }
}

/// ADR 0043 decision 32 (lane L2), test 2: on a host that denies the
/// escape (a stubbed `systemd-run` standing in for "no reachable
/// `systemd --user` manager", so this runs deterministically regardless
/// of whether a REAL one exists here too), the row still reaches "ready"
/// — contained, degraded, but never refused — and the daemon reports
/// exactly why: the LEG's own mgmt status reports `survival: Degraded`
/// on the wire ([`leg_survival`] — Codex SHOULD-FIX: cmdline text proves
/// only what was typed on the command line, not what the process
/// actually configured or reported; restoring the deleted Unix survival
/// clamp would still leave a cmdline-only check green), and the daemon's
/// own log names the probe's stderr.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn capsule_launch_degrades_when_no_user_scope_is_available() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("deg");
    let stub_dir = env.seed_stub_systemd_run();
    env.spawn_sotd_with_prepended_path(&stub_dir);
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "deg-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(
        create_res.payload.get("error").is_none(),
        "a denied user scope must never refuse the launch: {:?}",
        create_res.payload
    );
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND).await;
    let state_dir = state_dir_from_list(&mut conn, &mut next_id, &workspace_id).await;

    let (status, process) = tokio::task::spawn_blocking({
        let dir = state_dir.clone();
        move || sot_log::attach_client::supervisor_client::query_status(&dir)
    })
    .await
    .unwrap()
    .expect("query_status on the degraded row");
    drop(process);
    let voyage_id = status.voyage.expect("a voyage id on a ready row");

    let survival = leg_survival(&voyage_id).await;
    assert_eq!(
        survival,
        sot_log::lane::wire::Survival::Degraded,
        "the leg's own mgmt status must report survival=degraded when the user scope is denied"
    );

    let log_path = env.state_root.join("sot").join("sotd.log");
    let log_contents = std::fs::read_to_string(&log_path)
        .unwrap_or_else(|e| panic!("could not read the daemon's own log {log_path:?}: {e}"));
    assert!(
        log_contents.contains("stub: no user manager"),
        "expected {log_path:?} to contain the stub systemd-run's own stderr; got:\n{log_contents}"
    );

    env.kill_daemon_bounded().await;
}
/// A Windows test's capsules end with its `Env`: capsules outlive a killed
/// daemon by design, so `Env`'s `Drop` sweeps its own, as the Linux leg
/// sweep does.
#[cfg(windows)]
#[tokio::test]
async fn env_drop_leaves_no_capsule_of_its_own() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("edl");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    // No agent requested: the row's producer is the platform shell, which
    // launches on every Windows host.
    let create_req = serde_json::json!({
        "label": "edl-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND).await;

    let state_root = env.state_root.clone();
    assert!(
        !own_capsule_pids(&state_root).is_empty(),
        "a ready capsule row must have capsule processes over {state_root:?}"
    );

    drop(env);

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let left = own_capsule_pids(&state_root);
        if left.is_empty() {
            break;
        }
        assert!(Instant::now() < deadline, "Env's Drop left capsule processes alive: {left:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
