//! Lease tests: the close lifecycle's leases and the shutdown, end to end.

use super::*;

// Used only by explicit-close variants that keep the production shutdown bound.
const PRODUCTION_CLOSE_REPLY_WITHIN: Duration = sot_protocol::ops::lease::CLOSE_ACK_WAIT;
const PRODUCTION_CLOSE_EXIT_WITHIN: Duration =
    Duration::from_secs(sot_protocol::ops::lease::SHUTDOWN_BOUND.as_secs() + 10);

#[tokio::test]
async fn last_one_out_two_windows() {
    const RUNNING_ROW_EXIT_WITHIN: Duration = Duration::from_secs(130);
    let _serial = SERIAL.lock().await;
    let env = Env::new("lease1");
    let mut daemon = Daemon::start(&env, &[]).await;
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (id, row_state) = create_row(&env, &mut conn, &mut next_id, "last-one").await;
    drop(conn);
    let (mut a, lease_a) = Window::open(&env.socket_path).await;
    let (mut b, lease_b) = Window::open(&env.socket_path).await;
    assert_eq!(lease_a["outcome"], "granted", "{lease_a:?}");
    assert_eq!(lease_b["outcome"], "granted", "{lease_b:?}");

    a.eof().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(daemon.still_up(), "the daemon shut down while a window still held a lease: {}", daemon.said());
    assert!(row_toml(&env, "last-one").exists(), "a row ended while a window still held a lease");

    b.eof().await;
    assert_eq!(
        daemon.exit_within(RUNNING_ROW_EXIT_WITHIN).await,
        Some(0),
        "running target {id} ({row_state:?}), phase=daemon-exit after last-window EOF: {}",
        daemon.said()
    );
    assert!(!env.socket_path.exists(), "the socket outlived the daemon");
    assert!(!row_toml(&env, "last-one").exists(), "the row's toml outlived the close");
    #[cfg(target_os = "linux")]
    assert!(!capsules_left(&env), "a sot-capsule for this state root outlived the close");
    assert!(held_record(&env).is_none(), "held.json outlived a clean close: {:?}", held_record(&env));
}

#[tokio::test]
async fn lease_end_any_way_departs() {
    let _serial = SERIAL.lock().await;
    for how in ["close", "half", "kill"] {
        let env = Env::new(&format!("dep{how}"));
        let mut daemon = Daemon::start(&env, &[]).await;
        let (mut w, lease) = Window::open(&env.socket_path).await;
        assert_eq!(lease["outcome"], "granted", "{how}: {lease:?}");
        match how {
            "close" => {
                let ack = w.ask("close", PRODUCTION_CLOSE_REPLY_WITHIN).await;
                assert_eq!(ack["not_ended"], 0, "{how}: {ack:?}");
            }
            "half" => {
                w.send("half").await;
                w.eof().await;
            }
            _ => w.child.kill().await.expect("SIGKILL the window"),
        }
        assert_eq!(
            daemon.exit_within(if how == "close" { PRODUCTION_CLOSE_EXIT_WITHIN } else { EXIT_WITHIN }).await,
            Some(0),
            "{how}: the lease's end did not shut down: {}",
            daemon.said()
        );
    }
}

#[tokio::test]
async fn keep_is_open_ended() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("keep");
    let mut daemon = Daemon::start(&env, &[("SOT_TEST_HANDOVER_BOUND_MS", "1000")]).await;
    let (mut w, _) = Window::open(&env.socket_path).await;
    let ack = w.ask("keep", BOUND).await;
    assert_eq!(ack["not_ended"], 0, "{ack:?}");
    w.eof().await;
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(daemon.still_up(), "a keep was not open-ended: {}", daemon.said());
    assert!(held_record(&env).is_none(), "a keep left a record: {:?}", held_record(&env));
    let (_w2, lease) = Window::open(&env.socket_path).await;
    assert_eq!(lease["outcome"], "granted", "{lease:?}");
}

#[tokio::test]
async fn relaunch_handover_expires_then_shuts_down() {
    let _serial = SERIAL.lock().await;
    for in_time in [false, true] {
        let env = Env::new(if in_time { "hand1" } else { "hand0" });
        let mut daemon = Daemon::start(&env, &[("SOT_TEST_HANDOVER_BOUND_MS", "3000")]).await;
        let (mut w, _) = Window::open(&env.socket_path).await;
        let ack = w.ask("handover", BOUND).await;
        assert_eq!(ack["not_ended"], 0, "{ack:?}");
        w.eof().await;
        assert!(held_record(&env).is_some_and(|r| r["handover_until_ms"].is_u64()), "the handover is not recorded");
        if in_time {
            let (_next, lease) = Window::open(&env.socket_path).await;
            assert_eq!(lease["outcome"], "granted", "{lease:?}");
            assert_eq!(
                daemon.exit_within(Duration::from_secs(6)).await,
                None,
                "a lease in time did not keep the daemon: {}",
                daemon.said()
            );
        } else {
            assert_eq!(
                daemon.exit_within(Duration::from_secs(20)).await,
                Some(0),
                "an expired handover did not shut down: {}",
                daemon.said()
            );
        }
    }
}

#[tokio::test]
async fn lane_connection_close_does_not_shut_down() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("lanes");
    let mut daemon = Daemon::start(&env, &[]).await;
    // No lease yet: a capture's data connection, a lane login and a
    // redial come and go.
    let (conn, _) = connect_and_hello(&env.socket_path).await;
    drop(conn);
    let f = Frame::req(1, op::LANE_CONNECT, serde_json::json!({ "target": "no-such-row", "lane": "supervisor" }));
    let mut lane = handoff(&env.socket_path, &f).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), codec::read_frame(&mut lane)).await;
    drop(lane);
    drop(try_connect(&env.socket_path).await.expect("redial"));
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(daemon.still_up(), "a non-lease connection's end shut the daemon down: {}", daemon.said());

    // With a lease held, the same comings and goings never depart it.
    let (mut w, _) = Window::open(&env.socket_path).await;
    let (conn, _) = connect_and_hello(&env.socket_path).await;
    drop(conn);
    drop(try_connect(&env.socket_path).await.expect("redial"));
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(daemon.still_up(), "a non-lease connection's end shut the daemon down: {}", daemon.said());
    w.eof().await;
    assert_eq!(daemon.exit_within(EXIT_WITHIN).await, Some(0), "the lease was no longer held: {}", daemon.said());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn refused_end_reaches_the_window() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("refused");
    let mut daemon = Daemon::start(&env, &[("SOT_TEST_SHUTDOWN_BOUND_MS", "15000")]).await;
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (_id, state_dir) = create_row(&env, &mut conn, &mut next_id, "refused").await;
    drop(conn);
    let fence = slow_row(&state_dir).await;
    let (mut w, _) = Window::open(&env.socket_path).await;
    let ack = w.ask("close", EXIT_WITHIN).await;
    assert_eq!(ack["not_ended"], 1, "the close ack carries the refused end: {ack:?}");
    let rec = held_record(&env).expect("the record carries the count");
    assert_eq!(rec["not_ended"], 1, "{rec:?}");
    assert_eq!(rec["closing"], false, "{rec:?}");
    assert!(row_toml(&env, "refused").exists(), "a row that was not ended lost its registration");
    let ack = w.ask("ack 1", BOUND).await;
    assert!(ack.get("error").is_none(), "{ack:?}");
    assert_eq!(daemon.exit_within(EXIT_WITHIN).await, Some(0), "{}", daemon.said());
    drop(fence);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn create_during_shutdown_refused() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("createsd");
    let mut daemon = Daemon::start(&env, &[("SOT_TEST_SHUTDOWN_BOUND_MS", "15000")]).await;
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (_id, state_dir) = create_row(&env, &mut conn, &mut next_id, "slow").await;
    let fence = slow_row(&state_dir).await;
    let (mut w, _) = Window::open(&env.socket_path).await;
    w.send("close").await;
    // The slow row holds the shutdown in step 3; a create on a connection
    // accepted before it is refused.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let req = serde_json::json!({
        "label": "too-late",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let res = call(&mut conn, next_id, op::WORKSPACE_CREATE, req).await;
    assert!(res.payload.get("error").is_some(), "a create during the shutdown was not refused: {:?}", res.payload);
    assert!(try_connect(&env.socket_path).await.is_none(), "the dying daemon still accepted a connection");
    assert_eq!(daemon.exit_within(EXIT_WITHIN).await, Some(0), "{}", daemon.said());
    assert!(!row_toml(&env, "too-late").exists(), "the refused create left a row");
    drop(fence);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn shutdown_ends_a_child_that_left_the_agents_process_group() {
    let _serial = SERIAL.lock().await;
    use support::{arm_scope_guard, assert_scope_empties, cgroup_rel, user_manager_available_for_test};
    if let Err(e) = user_manager_available_for_test() {
        if std::env::var("SOT_TEST_REQUIRE_USER_MANAGER").as_deref() == Ok("1") {
            panic!("SOT_TEST_REQUIRE_USER_MANAGER=1 but no user manager is reachable: {e}");
        }
        eprintln!("SKIPPED: no user manager: {e}");
        return;
    }
    let env = Env::new("lesc");
    let (dir, pidfile) = env.seed_fake_claude_with_escapee();
    let path = format!("{}:{}", dir.display(), std::env::var("PATH").unwrap_or_default());
    let mut daemon = Daemon::start(&env, &[("PATH", &path)]).await;
    let (mut w, _) = Window::open(&env.socket_path).await;
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let req = serde_json::json!({
        "label": "esc-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
        "agent": "claude",
    });
    let res = call(&mut conn, next_id, op::WORKSPACE_CREATE, req).await;
    next_id += 1;
    assert!(res.payload.get("error").is_none(), "workspace.create failed: {:?}", res.payload);
    let id = res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let deadline = Instant::now() + Duration::from_secs(90);
    let state_dir = loop {
        let payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        next_id += 1;
        if let Some(row) = find_row(&payload, &id) {
            if let (Some(sd), Some("ready")) = (row["state_dir"].as_str(), row["phase"].as_str()) {
                break PathBuf::from(sd);
            }
        }
        assert!(Instant::now() < deadline, "the claude row never reached ready");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let dir = state_dir.clone();
    let (_status, process) =
        tokio::task::spawn_blocking(move || sot_log::attach_client::supervisor_client::query_status(&dir))
            .await
            .unwrap()
            .expect("query_status on a ready row");
    let scope = cgroup_rel(process.pid());
    drop(process);
    let _guard = arm_scope_guard(&scope, &state_dir);
    let deadline = Instant::now() + Duration::from_secs(10);
    let escapee: u32 = loop {
        if let Some(e) = std::fs::read_to_string(&pidfile).ok().and_then(|t| t.trim().parse().ok()) {
            break e;
        }
        assert!(Instant::now() < deadline, "the escapee never wrote {pidfile:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(cgroup_rel(escapee), scope, "the escapee is not in the row's scope");
    drop(conn);

    let ack = w.ask("close", PRODUCTION_CLOSE_REPLY_WITHIN).await;
    assert_eq!(ack["not_ended"], 0, "{ack:?}");
    assert_eq!(daemon.exit_within(PRODUCTION_CLOSE_EXIT_WITHIN).await, Some(0), "{}", daemon.said());
    assert_scope_empties(&scope, Duration::from_secs(5)).await;
}

/// Checks the exact inert-anchor/seeded-row target shape and the real durable-record readers.
fn assert_no_run_target_set(payload: &serde_json::Value, root: &Path) {
    use sot_log::supervisor::journal::{self, pointer};
    let targets = payload["workspaces"].as_array().expect("workspace.list target set");
    assert_eq!(targets.len(), 2, "never-run close has an extra shutdown target: {payload}");
    let anchor = targets.iter().find(|row| row["is_default"] == true).expect("inert anchor");
    assert_eq!(anchor["agent"], "none", "anchor has a runnable agent: {anchor}");
    assert_eq!(anchor["autostart_claude"], false, "anchor autostarts: {anchor}");
    let never = targets.iter().find(|row| row["workspace_id"] == "ws-never-0001").expect("seeded never-run row");
    assert_eq!(never["slug"], "never-run", "seeded row did not round-trip: {never}");
    assert_eq!(never["is_default"], false);
    for row in targets {
        assert_eq!(row["runtime"], "capsule", "unexpected target runtime: {row}");
        let dir = Path::new(row["state_dir"].as_str().expect("target state_dir"));
        assert!(
            matches!(pointer::validate(dir), pointer::PointerState::NotFound),
            "never-run target has a voyage pointer: {row}"
        );
        assert!(
            journal::active_operations(dir).expect("read target journal").is_empty(),
            "never-run target has a run operation: {row}"
        );
        assert!(
            matches!(std::fs::metadata(dir), Err(error) if error.kind() == std::io::ErrorKind::NotFound),
            "never-run target has a state directory or unreadable records: {row}"
        );
    }
    assert!(
        matches!(pointer::validate(root), pointer::PointerState::NotFound),
        "drawer adds a running shutdown target"
    );
    assert!(journal::active_operations(root).expect("read drawer journal").is_empty(), "drawer has a run operation");
}

/// Only workspace.list is sent: inspecting the fixture cannot start a row.
async fn assert_never_run_targets(env: &Env) {
    let (mut conn, next_id) = connect_and_hello(&env.socket_path).await;
    let payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    assert_no_run_target_set(&payload, &state_dir(env));
}

/// This audit reads the fixture and durable files; it starts no daemon and opens no socket.
#[test]
fn never_run_fixture_has_only_no_run_targets() {
    let source = sot_log::test_scan::without_test_modules(include_str!("lease.rs"));
    let body: String = source
        .lines()
        .skip_while(|line| !line.starts_with("async fn never_run_rows_count_as_ended("))
        .take_while(|line| *line != "}")
        .map(|line| format!("{line}\n"))
        .collect();
    assert!(!body.is_empty(), "never-run fixture absent");
    assert!(
        !body.contains("create_row") && !body.contains("WORKSPACE_CREATE"),
        "never-run fixture issued workspace.create"
    );
    assert!(!body.contains("\"ran\""), "never-run fixture targets a running row");
    let audit = body.find("assert_never_run_targets(&env).await;").expect("target audit absent");
    assert!(audit < body.find("w.ask(\"close\"").expect("close absent"), "target audit ran after close");
    let check: String = source
        .lines()
        .skip_while(|line| !line.starts_with("async fn assert_never_run_targets("))
        .take_while(|line| *line != "}")
        .map(|line| format!("{line}\n"))
        .collect();
    assert!(check.contains("op::WORKSPACE_LIST"), "target audit does not read the actual roster");
    assert!(!check.contains("create_row") && !check.contains("WORKSPACE_CREATE"), "target audit starts a row");
    let dir = tempfile::tempdir().unwrap();
    let anchor = dir.path().join("anchor");
    let never = dir.path().join("never");
    let payload = serde_json::json!({"workspaces": [
        {"workspace_id": "anchor", "is_default": true, "agent": "none", "autostart_claude": false, "runtime": "capsule", "state_dir": anchor},
        {"workspace_id": "ws-never-0001", "slug": "never-run", "is_default": false, "runtime": "capsule", "state_dir": never}
    ]});
    assert_no_run_target_set(&payload, dir.path());
    std::fs::create_dir(&never).unwrap();
    sot_log::supervisor::journal::pointer::publish(&never, "00000000-0000-0000-0000-000000000001").unwrap();
    assert!(
        std::panic::catch_unwind(|| assert_no_run_target_set(&payload, dir.path())).is_err(),
        "actual run pointer escaped target audit"
    );
}

/// A row with no run record (never started, or the anchor, which runs
/// nothing) has nothing to end: a close counts it ended without the
/// orphan proof. A row that ran and whose end cannot be proven still
/// counts not ended (`refused_end_reaches_the_window`).
#[tokio::test]
async fn never_run_rows_count_as_ended() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("neverrun");
    let never_root = env._tmp.path().join("never-project");
    std::fs::create_dir_all(&never_root).expect("mkdir the never-started row's project");
    env.seed_capsule_toml("ws-never-0001", "never-run", &never_root, "claude");
    let mut daemon = Daemon::start(&env, &[("SOT_TEST_SHUTDOWN_BOUND_MS", "15000")]).await;
    let (mut w, _) = Window::open(&env.socket_path).await;
    assert_never_run_targets(&env).await;
    let ack = w.ask("close", EXIT_WITHIN).await;
    assert_eq!(daemon.exit_within(EXIT_WITHIN).await, Some(0), "{}", daemon.said());
    let said = daemon.said();
    assert_eq!(ack["not_ended"], 0, "the anchor and a never-started row were counted not ended: {ack:?}: {said}");
    assert!(!said.contains("not ended"), "a row with no run record was warned about: {said}");
    assert!(held_record(&env).is_none(), "{:?}", held_record(&env));
    assert!(!row_toml(&env, "never-run").exists(), "the never-started row was not forgotten");
}

/// The user's latest intent wins: a `fe.leaving{close}` after a keep on
/// the same lease is that window's close, so the last one shuts down.
#[tokio::test]
async fn close_after_keep_on_one_lease_shuts_down() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("keepx");
    let mut daemon = Daemon::start(&env, &[]).await;
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (_id, _sd) = create_row(&env, &mut conn, &mut next_id, "keep-then-x").await;
    drop(conn);
    let (mut w, _) = Window::open(&env.socket_path).await;
    let ack = w.ask("keep", BOUND).await;
    assert_eq!(ack["not_ended"], 0, "{ack:?}");
    let ack = w.ask("close", PRODUCTION_CLOSE_REPLY_WITHIN).await;
    assert_eq!(
        daemon.exit_within(PRODUCTION_CLOSE_EXIT_WITHIN).await,
        Some(0),
        "a close after a keep on the same lease did not shut down: {}",
        daemon.said()
    );
    assert_eq!(ack["not_ended"], 0, "{ack:?}");
    assert!(!row_toml(&env, "keep-then-x").exists(), "the close after a keep left the row");
}

/// A keep followed by EOF stays a keep: the daemon and its rows run on.
#[tokio::test]
async fn keep_then_eof_stays_up() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("keepeof");
    let mut daemon = Daemon::start(&env, &[]).await;
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (id, _sd) = create_row(&env, &mut conn, &mut next_id, "kept").await;
    let (mut w, _) = Window::open(&env.socket_path).await;
    let ack = w.ask("keep", BOUND).await;
    assert_eq!(ack["not_ended"], 0, "{ack:?}");
    w.eof().await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(daemon.still_up(), "a keep then EOF shut the daemon down: {}", daemon.said());
    let payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    let row = find_row(&payload, &id).expect("the kept row is still registered");
    assert_eq!(row["phase"], "ready", "the kept row is not running: {row:?}");
}

#[tokio::test]
async fn malformed_lease_line_does_not_depart() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("malformed");
    let mut daemon = Daemon::start(&env, &[]).await;
    let (mut w, lease) = Window::open(&env.socket_path).await;
    assert_eq!(lease["outcome"], "granted", "{lease:?}");
    for cmd in [
        "garbage",
        r#"raw {"v":2,"id":7,"kind":"res","op":"fe.leaving","payload":{"intent":"close"}}"#,
        r#"raw {"v":2,"id":8,"kind":"req","op":"fe.nonsense","payload":{}}"#,
        r#"raw {"v":2,"id":9,"kind":"req","op":"fe.leaving","payload":{"intent":"later"}}"#,
        r#"raw {"v":2,"id":10,"kind":"req","op":"fe.notice_seen","payload":{"not_ended":"one"}}"#,
        "overcap",
    ] {
        let r = w.ask(cmd, BOUND).await;
        assert!(r.get("error").is_some(), "{cmd}: a malformed lease line is answered with an error: {r:?}");
    }
    // The window is the only lease, so a departure would have been a shutdown.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(daemon.still_up(), "a malformed lease line departed the lease: {}", daemon.said());
    w.eof().await;
    assert_eq!(daemon.exit_within(EXIT_WITHIN).await, Some(0), "the lease was no longer held: {}", daemon.said());
}

#[tokio::test]
async fn lease_end_while_data_conn_busy() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("busy");
    let mut daemon = Daemon::start(&env, &[]).await;
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    create_row(&env, &mut conn, &mut next_id, "busy-row").await;
    let (mut w, lease) = Window::open(&env.socket_path).await;
    assert_eq!(lease["outcome"], "granted", "{lease:?}");
    // The data connection's handler is now mid-read of a frame.
    conn.write_all(b"{\"v\":2,\"id\":").await.expect("write the half frame");
    conn.flush().await.expect("flush the half frame");
    w.eof().await;
    assert_eq!(daemon.exit_within(EXIT_WITHIN).await, Some(0), "the lease's end did not decide: {}", daemon.said());
    assert!(!row_toml(&env, "busy-row").exists(), "the row outlived the close");
    let read = tokio::time::timeout(Duration::from_secs(5), codec::read_frame(&mut conn)).await;
    assert!(read.is_ok(), "the busy data connection was left open by the daemon's exit");
}

#[tokio::test]
async fn non_lease_fe_never_decides() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("nonlease");
    let mut daemon = Daemon::start(&env, &[]).await;
    let me = sot_log::identity::challenge::self_identity().expect("this process's identity");

    async fn refused(env: &Env, me: &sot_log::identity::challenge::ProcessIdentity) {
        let req = FeLeaseReq { boot: me.boot.clone(), pid: me.pid + 1, created: me.created, token: None };
        let f = Frame::req(1, op::FE_LEASE, serde_json::to_value(&req).unwrap());
        let mut c = handoff(&env.socket_path, &f).await;
        let (reply, _) = tokio::time::timeout(BOUND, codec::read_frame(&mut c))
            .await
            .expect("the refusal did not arrive")
            .expect("read the refusal");
        assert_eq!(reply.payload["outcome"], "foreign", "{:?}", reply.payload);
    }

    refused(&env, &me).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(daemon.still_up(), "a refused lease's end shut the daemon down: {}", daemon.said());
    let (mut w, lease) = Window::open(&env.socket_path).await;
    assert_eq!(lease["outcome"], "granted", "{lease:?}");
    refused(&env, &me).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(daemon.still_up(), "a refused lease's end departed the granted window: {}", daemon.said());
    w.eof().await;
    assert_eq!(daemon.exit_within(EXIT_WITHIN).await, Some(0), "the lease was no longer held: {}", daemon.said());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn fast_reopen_never_reaches_dying_daemon() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("reopen");
    let mut daemon = Daemon::start(&env, &[("SOT_TEST_SHUTDOWN_BOUND_MS", "15000")]).await;
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (_id, state_dir) = create_row(&env, &mut conn, &mut next_id, "slow").await;
    drop(conn);
    let fence = slow_row(&state_dir).await;
    // Accepted before the shutdown begins, and silent until after.
    let mut c = tokio::io::BufReader::new(try_connect(&env.socket_path).await.expect("connect"));
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (mut w, _) = Window::open(&env.socket_path).await;
    w.send("close").await;
    // The slow row holds the shutdown in step 3.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(try_connect(&env.socket_path).await.is_none(), "the dying daemon still accepted a connection");
    let me = sot_log::identity::challenge::self_identity().expect("this process's identity");
    let req = FeLeaseReq { boot: me.boot, pid: me.pid, created: me.created, token: None };
    let f = Frame::req(1, op::FE_LEASE, serde_json::to_value(&req).unwrap());
    handoff_on(&mut c, &f, &[]).await;
    let reply = tokio::time::timeout(BOUND, codec::read_frame(&mut c))
        .await
        .expect("no answer to the late lease")
        .unwrap_or_else(|e| {
            panic!("the earlier connection was not accepted before the shutdown began (test sync), not the defect: {e}")
        });
    assert_eq!(reply.0.payload["outcome"], "closing", "{:?}", reply.0.payload);
    assert_eq!(daemon.exit_within(EXIT_WITHIN).await, Some(0), "{}", daemon.said());
    drop(fence);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn closing_flag_spans_shutdown() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("closing");
    let mut daemon = Daemon::start(&env, &[("SOT_TEST_SHUTDOWN_BOUND_MS", "15000")]).await;
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (_id, slow_dir) = create_row(&env, &mut conn, &mut next_id, "slow").await;
    let quick_root = env._tmp.path().join("quick-root");
    std::fs::create_dir_all(&quick_root).expect("create the quick row's root");
    create_row_at(&mut conn, &mut next_id, "quick", &quick_root).await;
    drop(conn);
    let fence = slow_row(&slow_dir).await;
    let (mut w, _) = Window::open(&env.socket_path).await;
    w.send("close").await;
    // The quick row ended while the slow one still holds step 3, so the
    // final record (step 5) cannot have been written.
    let deadline = Instant::now() + EXIT_WITHIN;
    while row_toml(&env, "quick").exists() {
        assert!(Instant::now() < deadline, "the quick row was never ended: {}", daemon.said());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let rec = held_record(&env).expect("the record exists while the shutdown runs");
    assert_eq!(rec["closing"], true, "the record does not say closing mid-shutdown: {rec:?}");
    assert_eq!(daemon.exit_within(EXIT_WITHIN).await, Some(0), "{}", daemon.said());
    let rec = held_record(&env).expect("the final record");
    assert_eq!(rec["closing"], false, "{rec:?}");
    assert_eq!(rec["not_ended"], 1, "{rec:?}");
    drop(fence);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn shutdown_bound_is_end_to_end() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("endtoend");
    let mut daemon =
        Daemon::start(&env, &[("SOT_TEST_SHUTDOWN_BOUND_MS", "19000"), ("SOT_TEST_SPAWN_SETTLE_MS", "5000")]).await;
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (_id, row_dir) = create_row(&env, &mut conn, &mut next_id, "slow").await;
    drop(conn);
    let (mut w, _) = Window::open(&env.socket_path).await;
    let _fence = slow_row(&row_dir).await;
    // The watchdog restarts the killed supervisor 1 s later and holds that run start through its settle, 5 s here;
    // the new supervisor finds the fence held and logs it. The close lands about 0.3 s into the settle.
    let log_path = state_dir(&env).join("sotd.log");
    let until = Instant::now() + BOUND;
    while !std::fs::read_to_string(&log_path).unwrap_or_default().contains("authority fence already held") {
        assert!(Instant::now() < until, "the watchdog never restarted the slow row: {}", daemon.said());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let ack = w.ask("close", EXIT_WITHIN).await;
    // No `ack` is sent, so the closer's notice waits out its own bound; exit 0
    // (the backstop's is 1) proves the whole shutdown fit inside the bound.
    assert_eq!(daemon.exit_within(EXIT_WITHIN).await, Some(0), "{}", daemon.said());
    // Steps 2 and 3 share one rows deadline, decided + budget (9 s), which the hold ends before. The refused row, retried
    // 1 s apart, is let go between budget - 1 and budget + 0.4 (timer and log slack). A fresh step-3 deadline lets it go
    // at least held + budget - 1 in: past the upper bound only if held >= 1.4, which the last assert requires.
    let log = std::fs::read_to_string(&log_path).expect("the daemon's log");
    let since = |from: f64, to: f64| (to - from + 43_200.0).rem_euclid(86_400.0) - 43_200.0;
    let began = stamped(&log, "shutting down: ending this computer's sessions");
    let held = since(began, stamped(&log, "lane did not settle within the post-spawn deadline"));
    let rows = since(began, stamped(&log, "row not ended by the deadline"));
    let budget = 19.0 - sot_protocol::ops::lease::SHUTDOWN_TAIL.as_secs_f64();
    eprintln!("run start held {held:.3} s into the shutdown; row let go at {rows:.3} s");
    assert!(rows < budget + 0.4, "step 3 ran past the rows deadline it shares with step 2: {rows:.3} s");
    assert!(rows > budget - 2.0, "the refused row was not retried through the rows budget: {rows:.3} s");
    assert_eq!(ack["not_ended"], 1, "{ack:?}");
    assert!(held >= 1.4, "the close landed {held:.3} s before the settle ended; under 1.4 s no fresh step-3 deadline can show (test sync, not the defect)");
}
