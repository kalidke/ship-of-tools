//! Reaching a row through the daemon: directly, an old daemon, a lane-only drop, a never-started row,
//! and through a stub ssh child.

use super::*;

// -----------------------------------------------------------------------
// (i) The attach client reaches a supervisor through a daemon in the
// middle.
// -----------------------------------------------------------------------

#[tokio::test]
async fn fe_client_reaches_a_capsule_row_through_the_daemon() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found next to sotd — build it first (cargo build -p sot-log --bin sot-capsule)");

    let env = Env::new("lb1");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (_workspace_id, target) = create_ready_capsule_row(&env, &mut conn, &mut next_id, "lb1-workspace").await;

    let relay = Relay::start(env.socket_path.clone()).await;
    let (_woke, wake) = wake_flag_for_test();
    let mut client = FeAttachClient::<DaemonLaneEndpoint>::attach(
        daemon_lane_endpoint(&relay),
        target,
        80,
        24,
        "test-fe".to_string(),
        "test-fe".to_string(),
        None,
        wake,
    )
    .expect("attach through the daemon bridge");

    let deadline = Instant::now() + Duration::from_secs(30);
    while !client.is_checkpointed() {
        client.pump();
        assert!(!client.is_dead(), "died before a checkpoint through the bridge: {}", client.status_line());
        assert!(Instant::now() < deadline, "never checkpointed through the bridge");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    client.send_input(b"echo hi\r");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        client.pump();
        if let Some(outcome) = client.last_input_outcome() {
            assert_eq!(outcome, InputOutcome::Recorded, "the first input through the bridge must record");
            break;
        }
        assert!(!client.is_dead(), "died before its input outcome: {}", client.status_line());
        assert!(Instant::now() < deadline, "input outcome never arrived through the bridge");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(client.recorded_bytes(), 8, "recorded_bytes must equal the sent payload's own length");
    assert!(
        client.notice().is_some_and(|n| n.starts_with("attached to leg")),
        "notice() must name the leg, got {:?}",
        client.notice()
    );

    client.request_quit("lane bridge test quit");
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut exited = false;
    while Instant::now() < deadline {
        client.pump();
        if client.should_exit() {
            exited = true;
            break;
        }
        assert_ne!(
            client.quit_message(),
            Some("ending the session did not complete \u{2014} outcome unknown"),
            "the quit dispatcher timed out instead of observing record_closed"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(exited, "quit dispatcher never reached record_verified through the bridge (status={})", client.status_line());

    drop(client);
    env.kill_daemon_bounded().await;
}

// -----------------------------------------------------------------------
// (ii) An old daemon (no lane bridge) is a terminal "no bridge" — never a
// second dial attempt.
// -----------------------------------------------------------------------

#[tokio::test]
async fn an_old_daemon_is_a_terminal_no_bridge() {
    let dir = tempfile::Builder::new().prefix("sot-old-daemon-").tempdir_in("/tmp").expect("fake daemon folder");
    let path = dir.path().join("daemon.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let dials = Arc::new(AtomicUsize::new(0));
    let dials2 = Arc::clone(&dials);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            dials2.fetch_add(1, Ordering::SeqCst);
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                // The generic "unknown op" shape a daemon predating the
                // lane bridge answers — matches `sot-protocol`'s own
                // `lane_client.rs` unit test `an_unknown_op_reply_is_no_
                // bridge` exactly.
                let res = Frame::res(1, op::LANE_CONNECT, serde_json::json!({ "error": "unknown op: lane.connect" }));
                let mut line = serde_json::to_vec(&res).unwrap();
                line.push(b'\n');
                let _ = stream.write_all(&line);
                std::thread::sleep(Duration::from_millis(300));
            });
        }
    });

    let (_woke, wake) = wake_flag_for_test();
    let mut client = FeAttachClient::<DaemonLaneEndpoint>::attach(
        DaemonLaneEndpoint { dial: LaneDial::Local(path), token: None },
        "row-old-daemon".to_string(),
        80,
        24,
        "test-fe".to_string(),
        "test-fe".to_string(),
        None,
        wake,
    )
    .expect("attach starts even against a bridge-less daemon");

    let deadline = Instant::now() + Duration::from_secs(10);
    while !client.is_dead() {
        client.pump();
        assert!(Instant::now() < deadline, "client never went terminal against a no-bridge daemon");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(client.status_line().contains("no bridge"), "status must name the missing bridge, got {:?}", client.status_line());

    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(dials.load(Ordering::SeqCst), 1, "a Refused{{no_bridge}} reply must be terminal — never a second dial");
}

// -----------------------------------------------------------------------
// (iii) A lane-only drop is resumed by the client's own next dial.
// -----------------------------------------------------------------------

#[tokio::test]
async fn a_lane_only_drop_is_resumed_by_the_next_dial() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found — build it first");

    let env = Env::new("lb3");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (_workspace_id, target) = create_ready_capsule_row(&env, &mut conn, &mut next_id, "lb3-workspace").await;

    let relay = Relay::start(env.socket_path.clone()).await;
    let (_woke, wake) = wake_flag_for_test();
    let mut client = FeAttachClient::<DaemonLaneEndpoint>::attach(
        daemon_lane_endpoint(&relay),
        target,
        80,
        24,
        "test-fe".to_string(),
        "test-fe".to_string(),
        None,
        wake,
    )
    .expect("attach");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !client.is_checkpointed() {
        client.pump();
        assert!(!client.is_dead(), "died before a checkpoint: {}", client.status_line());
        assert!(Instant::now() < deadline, "never checkpointed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let notice_deadline = Instant::now() + Duration::from_secs(5);
    while client.notice().is_none() {
        client.pump();
        assert!(Instant::now() < notice_deadline, "notice() never named a leg before the drop");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let leg_notice = client.notice().map(|s| s.to_string());

    let pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "supervise", &env.state_root);
    kill_supervisor_only(&env.state_root);

    let resumed_deadline = Instant::now() + Duration::from_secs(60);
    loop {
        client.pump();
        assert!(!client.is_dead(), "a lane-only drop must never go terminal: {}", client.status_line());
        if count_matching_processes(&pattern).unwrap_or(0) >= 1 {
            break;
        }
        assert!(Instant::now() < resumed_deadline, "the daemon never resumed the dropped row");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(count_matching_processes(&pattern).unwrap_or(99), 1, "ensure_started must spawn exactly one new supervise process");

    // Functional recovery, not merely a resumed process: a fresh input
    // through the SAME client lands, and the leg identity is unchanged.
    client.send_input(b"echo lb3-resumed\r");
    let input_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        client.pump();
        assert!(!client.is_dead(), "died while resuming: {}", client.status_line());
        if screen_text(&client).contains("lb3-resumed") {
            break;
        }
        assert!(Instant::now() < input_deadline, "resumed client's own input never echoed");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(client.notice().map(|s| s.to_string()), leg_notice, "notice() must still name the SAME leg after a lane-only drop");
    assert!(!client.is_dead(), "the client must never have gone terminal across the drop");

    drop(client);
    relay.cut();
    env.kill_daemon_bounded().await;
}

// -----------------------------------------------------------------------
// (iii-b) R4c: the bridge's `Reconnect` intent permits only Resume, never a never-started row's first start.
// -----------------------------------------------------------------------

#[tokio::test]
async fn a_never_started_row_is_not_started_by_a_bridge_dial() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found — build it first");

    let env = Env::new("lb3b");
    // Pre-seeded, never touched by `workspace.create` — its supervisor
    // has never been spawned by anything (`workspace.create` always
    // spawns synchronously, so this precondition can't come from it).
    env.seed_capsule_toml("ws-lb3b-preseeded", "lb3b-preseeded", &env.workspace_project_root, "none");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id += 1;
    let row = find_row(&list_payload, "ws-lb3b-preseeded").expect("the pre-seeded row is registered");
    assert_eq!(row["phase"].as_str(), Some("stopped"), "row must be genuinely never-started: {row:?}");
    let target = row["session_name"].as_str().expect("session_name").to_string();

    let pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "supervise", &env.state_root);
    assert_eq!(count_matching_processes(&pattern).unwrap_or(99), 0, "no supervise process must exist before the dial");

    let relay = Relay::start(env.socket_path.clone()).await;
    let (_woke, wake) = wake_flag_for_test();
    let mut client = FeAttachClient::<DaemonLaneEndpoint>::attach(
        daemon_lane_endpoint(&relay),
        target,
        80,
        24,
        "test-fe".to_string(),
        "test-fe".to_string(),
        None,
        wake,
    )
    .expect("attach");

    // R4c: `Reconnect` permits only `StartMode::Resume` — a dial on an
    // unpublished pointer starts nothing and answers absent. Proven over
    // a bounded window, not by waiting for the client to go terminal
    // (main's own behavior too: that needs the full client-side
    // HEALTH_WINDOW, unrelated to this ruling): across ~10s, the bridge
    // keeps answering absent (never attaches, never goes dead on its
    // own), the row's phase never moves off "stopped", and no supervise
    // process for it ever exists.
    let window_deadline = Instant::now() + Duration::from_secs(10);
    let mut saw_absent_answer = false;
    loop {
        client.pump();
        assert!(
            !client.is_dead(),
            "a never-started row's bridge dial must not go terminal on its own: {}",
            client.status_line()
        );
        if client.status_line().contains("not answering") {
            saw_absent_answer = true;
        }
        assert_eq!(
            count_matching_processes(&pattern).unwrap_or(99),
            0,
            "a bridge dial must never start a row's first-ever run"
        );
        let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        next_id += 1;
        let row = find_row(&list_payload, "ws-lb3b-preseeded").expect("the pre-seeded row is registered");
        assert_eq!(
            row["phase"].as_str(),
            Some("stopped"),
            "the row must stay stopped while the bridge keeps answering absent: {row:?}"
        );
        if Instant::now() >= window_deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(saw_absent_answer, "the bridge must have answered absent at least once: {}", client.status_line());

    drop(client);
    relay.cut();
    env.kill_daemon_bounded().await;
}

// -----------------------------------------------------------------------
// C3 as amended (isolation-plan.md §3, dev/output/c3-second-connection-
// amendment.md §7): `LaneDial::Ssh` reaches the SAME daemon-in-the-middle
// as `LaneDial::Local` above, through a spawned child instead of an
// already-open socket. A stub `ssh` first on `PATH` stands in for the
// real binary — it ignores every option/command argv `ssh_bridge::argv`
// builds and instead relays its stdin/stdout to the harness's own
// `Relay`, which is the "far end" both real ssh and a real daemon would
// otherwise be.
// -----------------------------------------------------------------------

/// Writes an executable `ssh` (no extension: this is the Linux-only half
/// of this file, `#![cfg(target_os = "linux")]` at the top) into a fresh
/// temp dir that relays stdin/stdout to the Unix socket at `socket` via `nc -U`
/// (`comm-relay.sh`'s own `nc -U` path is the shell twin). The caller
/// prepends the returned dir to `$PATH`.
fn stub_ssh_relaying_to(dir: &Path, socket: &Path) {
    let script = format!("#!/bin/sh\nexec nc -U {}\n", socket.display());
    let path = dir.join("ssh");
    std::fs::write(&path, script).expect("write stub ssh");
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&path, perms).expect("chmod stub ssh");
}

/// The dying twin: exits nonzero before ever touching a socket, with one
/// stderr line — the diagnosis `BridgedClient` surfaces in place of a
/// generic broken-pipe message (this module's own doc; C3 as amended §6).
fn stub_ssh_dying_with(dir: &Path, stderr_line: &str) {
    let script = format!("#!/bin/sh\necho '{stderr_line}' >&2\nexit 255\n");
    let path = dir.join("ssh");
    std::fs::write(&path, script).expect("write dying stub ssh");
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&path, perms).expect("chmod dying stub ssh");
}

/// Prepends `dir` to this PROCESS's `$PATH` — global, like the `SERIAL`
/// mutex above already accounts for (`SOT_RUNTIME_DIR`) — and returns a
/// guard that restores the exact original value on drop, so a later test
/// in this same binary never inherits a stub.
struct PathGuard(String);
impl PathGuard {
    fn prepend(dir: &Path) -> Self {
        let original = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{}", dir.display(), original);
        std::env::set_var("PATH", new_path);
        PathGuard(original)
    }
}
impl Drop for PathGuard {
    fn drop(&mut self) {
        std::env::set_var("PATH", &self.0);
    }
}

#[tokio::test]
async fn fe_client_reaches_a_capsule_row_through_a_stub_ssh_child() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found next to sotd — build it first (cargo build -p sot-log --bin sot-capsule)");

    let env = Env::new("lb-ssh1");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (_workspace_id, target) = create_ready_capsule_row(&env, &mut conn, &mut next_id, "lb-ssh1-workspace").await;

    let relay = Relay::start(env.socket_path.clone()).await;
    let stub_dir = tempfile::Builder::new().prefix("sot-stub-ssh-").tempdir().expect("tempdir");
    stub_ssh_relaying_to(stub_dir.path(), &relay.path);
    let _path_guard = PathGuard::prepend(stub_dir.path());

    let recipe = sot_protocol::topology::ssh_bridge::SshRecipe::new("teststub", None).expect("plain host name");
    let endpoint = DaemonLaneEndpoint { dial: LaneDial::Ssh(recipe, Default::default()), token: None };
    let (_woke, wake) = wake_flag_for_test();
    let mut client = FeAttachClient::<DaemonLaneEndpoint>::attach(
        endpoint,
        target,
        80,
        24,
        "test-fe".to_string(),
        "test-fe".to_string(),
        None,
        wake,
    )
    .expect("attach through the ssh-child bridge");

    let deadline = Instant::now() + Duration::from_secs(30);
    while !client.is_checkpointed() {
        client.pump();
        assert!(!client.is_dead(), "died before a checkpoint through the ssh-child bridge: {}", client.status_line());
        assert!(Instant::now() < deadline, "never checkpointed through the ssh-child bridge");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // The peer report is the DAEMON's (proven the same way the direct case
    // above proves it: a live checkpoint only reaches this far once the
    // bridge's split-identity proof — `DaemonLaneEndpoint::challenge`
    // against the daemon's own `LaneConnectRes` report — has already
    // succeeded over this exact stub-ssh connection).
    assert!(
        client.notice().is_some_and(|n| n.starts_with("attached to leg")),
        "notice() must name the leg, got {:?}",
        client.notice()
    );

    client.request_quit("lane bridge ssh-stub test quit");
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut exited = false;
    while Instant::now() < deadline {
        client.pump();
        if client.should_exit() {
            exited = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(exited, "quit dispatcher never reached record_verified through the ssh-child bridge (status={})", client.status_line());

    drop(client);
    env.kill_daemon_bounded().await;
}

#[tokio::test]
async fn a_stub_ssh_that_dies_first_puts_its_stderr_line_in_the_lane_status() {
    let _serial = SERIAL.lock().await;
    let stub_dir = tempfile::Builder::new().prefix("sot-stub-ssh-dying-").tempdir().expect("tempdir");
    stub_ssh_dying_with(stub_dir.path(), "Permission denied (publickey).");
    let _path_guard = PathGuard::prepend(stub_dir.path());

    let recipe = sot_protocol::topology::ssh_bridge::SshRecipe::new("teststub", None).expect("plain host name");
    let endpoint = DaemonLaneEndpoint { dial: LaneDial::Ssh(recipe, Default::default()), token: None };
    let (_woke, wake) = wake_flag_for_test();
    // `attach()`'s only `Err` is a failed OS thread spawn (`attach_inner`,
    // `sot-log/src/attach_client/client.rs`) — the dial itself runs on the worker
    // thread it spawns, and `Unreachable` (what a dying child classifies
    // as) retries rather than failing this call outright (ADR 0045
    // decision 4: "Unreachable must retry, never go terminal on its
    // own"). The observable this test proves is the status TEXT a
    // retrying attempt already carries, not a terminal/dead client.
    let mut client = FeAttachClient::<DaemonLaneEndpoint>::attach(
        endpoint,
        "row-does-not-matter-the-child-dies-first".to_string(),
        80,
        24,
        "test-fe".to_string(),
        "test-fe".to_string(),
        None,
        wake,
    )
    .expect("attach() itself only fails on a thread-spawn error");

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        client.pump();
        if client.status_line().contains("Permission denied") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the lane status text never carried the dying child's last stderr line, got: {}",
            client.status_line()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
