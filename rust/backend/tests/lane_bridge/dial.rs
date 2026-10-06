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
// (ii) An old daemon is a terminal refusal — never a second dial attempt: one
// that predates the lane bridge answers `lane.connect` with the generic "unknown
// op", and one that predates protocol 3 refuses the dial's hello.
// -----------------------------------------------------------------------

/// A fake daemon that answers the first bytes of every connection with `reply`; returns the status line the
/// attach client went terminal with and how many connections it opened.
async fn attach_to_a_daemon_that_answers(reply: Frame) -> (String, usize) {
    let dir = tempfile::Builder::new().prefix("sot-old-daemon-").permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700)).tempdir_in("/tmp").expect("fake daemon folder");
    let path = dir.path().join("daemon.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let dials = Arc::new(AtomicUsize::new(0));
    let dials2 = Arc::clone(&dials);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            dials2.fetch_add(1, Ordering::SeqCst);
            let reply = reply.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let mut line = serde_json::to_vec(&reply).unwrap();
                line.push(b'\n');
                let _ = stream.write_all(&line);
                std::thread::sleep(Duration::from_millis(300));
            });
        }
    });

    let (_woke, wake) = wake_flag_for_test();
    let mut client = FeAttachClient::<DaemonLaneEndpoint>::attach(
        DaemonLaneEndpoint::new(LaneDial::Local(path), None),
        "row-old-daemon".to_string(),
        80,
        24,
        "test-fe".to_string(),
        "test-fe".to_string(),
        None,
        wake,
    )
    .expect("attach starts even against an old daemon");

    let deadline = Instant::now() + Duration::from_secs(10);
    while !client.is_dead() {
        client.pump();
        assert!(Instant::now() < deadline, "client never went terminal against an old daemon");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    (client.status_line().to_string(), dials.load(Ordering::SeqCst))
}

#[tokio::test]
async fn a_daemon_without_the_bridge_is_a_terminal_no_bridge() {
    // The generic "unknown op" shape a daemon predating the lane bridge answers — matches `sot-protocol`'s own
    // `lane_client.rs` unit test `an_unknown_op_reply_is_no_bridge` exactly.
    let reply = Frame::res(1, op::LANE_CONNECT, serde_json::json!({ "error": "unknown op: lane.connect" }));
    let (status, dials) = attach_to_a_daemon_that_answers(reply).await;
    assert!(status.contains("no bridge"), "status must name the missing bridge, got {status:?}");
    assert_eq!(dials, 1, "a Refused{{no_bridge}} reply must be terminal — never a second dial");
}

#[tokio::test]
async fn a_daemon_on_an_older_protocol_is_a_terminal_refusal() {
    let payload = serde_json::json!({ "error": "protocol mismatch: update the older side", "code": "protocol_mismatch" });
    let (status, dials) = attach_to_a_daemon_that_answers(Frame::res(1, op::HELLO, payload)).await;
    assert!(status.contains("protocol_mismatch"), "status must name the daemon's refusal, got {status:?}");
    assert_eq!(dials, 1, "a refused hello must be terminal — never a second dial");
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

// An explicitly spawned SSH stand-in carries the endpoint's piped bytes to the private Relay; no test changes PATH or SHELL.

/// Writes the executable that the caller explicitly spawns to relay its pipes to the test-owned socket.
fn stub_ssh_relaying_to(dir: &Path, socket: &Path) {
    let script = format!(r#"#!/usr/bin/env python3
import os, socket, sys, threading
peer = socket.socket(socket.AF_UNIX)
peer.connect({socket:?})
def upload():
    while True:
        data = os.read(0, 65536)
        if not data:
            peer.shutdown(socket.SHUT_WR)
            return
        peer.sendall(data)
threading.Thread(target=upload, daemon=True).start()
while True:
    data = peer.recv(65536)
    if not data:
        break
    sys.stdout.buffer.write(data)
    sys.stdout.buffer.flush()
"#, socket = socket.to_string_lossy());
    sot_log::test_exec::write_executable(&dir.join("ssh"), script);
}

fn stub_endpoint(path: PathBuf) -> DaemonLaneEndpoint {
    let recipe = sot_protocol::topology::ssh_bridge::SshRecipe::new("teststub", None).expect("plain host name");
    DaemonLaneEndpoint::new(LaneDial::Ssh(recipe, Default::default()), None).with_test_ssh_spawner(Arc::new(move |recipe, gate| {
        let command = gate.command(recipe)?;
        std::process::Command::new(&path).args(command.get_args())
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
            .spawn().map_err(sot_protocol::topology::ssh_bridge::SpawnError::Io)
    }))
}

/// The dying twin: exits nonzero before ever touching a socket, with one
/// stderr line — the diagnosis `BridgedClient` surfaces in place of a
/// generic broken-pipe message (this module's own doc; C3 as amended §6).
fn stub_ssh_dying_with(dir: &Path, stderr_line: &str) {
    let script = format!("#!/bin/sh\necho '{stderr_line}' >&2\nexit 255\n");
    let path = dir.join("ssh");
    sot_log::test_exec::write_executable(&path, script);
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

    let endpoint = stub_endpoint(stub_dir.path().join("ssh"));
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

    let endpoint = stub_endpoint(stub_dir.path().join("ssh"));
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

/// Both children record their start; the hello is read first and the lane.connect request determines the role. The supervisor observes the spare before answering, regardless of which child started first.
fn ordering_fixture(dir: &Path) -> PathBuf {
    let path = dir.join("ordering-ssh");
    let script = format!(r#"#!/usr/bin/env python3
import json, os, pathlib, sys, time
root = pathlib.Path({root:?})
(root / ('start-' + str(os.getpid()))).write_text('started\n')
hello = json.loads(sys.stdin.readline())
request = json.loads(sys.stdin.readline())
assert hello['op'] == 'hello' and hello['payload']['role'] == 'handoff'
assert request['op'] == 'lane.connect'
if request['payload']['lane'] == 'supervisor':
    (root / 'supervisor-pid').write_text(str(os.getpid()))
    deadline = time.monotonic() + 1.5
    while len(list(root.glob('start-*'))) < 2 and time.monotonic() < deadline:
        time.sleep(0.005)
    if len(list(root.glob('start-*'))) >= 2:
        (root / 'ordered').write_text('spare before supervisor answer\n')
    for frame, payload in [(hello, {{'ok': True}}), (request, {{'error': 'ordering fixture refused', 'code': 'unknown_workspace'}})]:
        print(json.dumps({{'v': frame['v'], 'id': frame['id'], 'kind': 'res', 'op': frame['op'], 'payload': payload}}), flush=True)
else:
    sys.stdin.buffer.read()
"#, root = dir.to_string_lossy());
    sot_log::test_exec::write_executable(&path, script);
    path
}

#[test]
fn the_spare_login_starts_before_the_supervisor_handshake_completes() {
    use sot_log::lane::client::Endpoint;
    let dir = tempfile::tempdir().unwrap();
    let endpoint = stub_endpoint(ordering_fixture(dir.path()));
    let result = endpoint.connect_supervisor_unchallenged("row-ordering");
    assert!(dir.path().join("ordered").is_file(), "the spare login must start before the supervisor handshake completes");
    assert!(matches!(result, Err(sot_log::lane::transport::TransportError::Refused { .. })), "ordering fixture returned {:?}", result.err());
    drop(endpoint);
}

#[test]
fn a_failed_supervisor_handshake_drops_the_spare() {
    use sot_log::lane::client::Endpoint;
    let dir = tempfile::tempdir().unwrap();
    let endpoint = stub_endpoint(ordering_fixture(dir.path()));
    let result = endpoint.connect_supervisor_unchallenged("row-abandoned");
    assert!(dir.path().join("ordered").is_file(), "both owned children started before refusal");
    assert!(matches!(result, Err(sot_log::lane::transport::TransportError::Refused { .. })));
    let supervisor = std::fs::read_to_string(dir.path().join("supervisor-pid")).unwrap();
    let mut spares = 0;
    for entry in std::fs::read_dir(dir.path()).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(pid) = name.strip_prefix("start-").filter(|pid| *pid != supervisor) {
            spares += 1;
            assert!(!Path::new(&format!("/proc/{pid}/stat")).exists(), "a failed supervisor handshake must reap its spare while the endpoint is alive");
        }
    }
    assert_eq!(spares, 1, "one owned spare was observed");
    drop(endpoint);
}

/// Observe the private daemon's admission close before the first voyage consumes its spare.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_expired_spare_uses_a_fresh_voyage_login() {
    use sot_log::lane::client::Endpoint;
    let _serial = SERIAL.lock().await;
    let env = Env::new("lb-expired-spare");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (_, target) = create_ready_capsule_row(&env, &mut conn, &mut next_id, "lb-expired-row").await;
    let relay = Relay::start(env.socket_path.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    stub_ssh_relaying_to(dir.path(), &relay.path);
    let pids = Arc::new(Mutex::new(Vec::new()));
    let observed = pids.clone();
    let path = dir.path().join("ssh");
    let recipe = sot_protocol::topology::ssh_bridge::SshRecipe::new("teststub", None).unwrap();
    let endpoint = DaemonLaneEndpoint::new(LaneDial::Ssh(recipe, Default::default()), None).with_test_ssh_spawner(Arc::new(move |recipe, gate| {
        let command = gate.command(recipe)?;
        let child = std::process::Command::new(&path).args(command.get_args())
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
            .spawn().map_err(sot_protocol::topology::ssh_bridge::SpawnError::Io)?;
        observed.lock().unwrap().push(child.id());
        Ok(child)
    }));
    let supervisor = endpoint.connect_supervisor_unchallenged(&target).unwrap();
    assert_eq!(pids.lock().unwrap().len(), 2, "the initial pair started");
    let spare = pids.lock().unwrap()[1];
    let stat = format!("/proc/{spare}/stat");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let text = std::fs::read_to_string(&stat).expect("the retained child stays unreaped until consumption");
        if text.rsplit_once(") ").unwrap().1.starts_with('Z') { break; }
        assert!(Instant::now() < deadline, "the private daemon never closed the idle bridge at admission expiry");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    drop(supervisor);
    let mut client = FeAttachClient::attach(endpoint, target, 80, 24, "expiry-fe".into(), "expiry-fe".into(), None, Box::new(|| {})).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !client.is_checkpointed() {
        client.pump();
        assert!(!client.is_dead(), "expired-spare fallback died: {}", client.status_line());
        assert!(Instant::now() < deadline, "expired-spare fallback never received its checkpoint");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(pids.lock().unwrap().len(), 4, "retry supervisor plus one fresh voyage login");
    assert!(!Path::new(&stat).exists(), "the expired owned child was reaped before fallback");
    client.shutdown(Duration::from_secs(5));
    env.kill_daemon_bounded().await;
}

/// A fixed spare-entry delay and bounded rendezvous; entry order never establishes a lane role.
fn rendezvous_endpoint(dir: &Path, delay: u64, reversed: bool) -> DaemonLaneEndpoint {
    let path = dir.join("rendezvous.py");
    let script = format!(r#"import json, os, pathlib, sys, time
root = pathlib.Path({root:?})
slot = int(sys.argv[1])
reverse = {reverse}
deadline = time.monotonic() + 30
if slot == 1:
    time.sleep({delay})
wait_for = 'start-1' if reverse and slot == 0 else ('start-0' if not reverse and slot == 1 else None)
while wait_for and not (root / wait_for).exists():
    assert time.monotonic() < deadline, 'fixture entry rendezvous timed out'
    time.sleep(0.005)
(root / ('start-' + str(slot))).write_text(str(os.getpid()))
hello = json.loads(sys.stdin.readline())
request = json.loads(sys.stdin.readline())
assert hello['kind'] == 'req' and hello['op'] == 'hello' and hello['payload']['role'] == 'handoff'
assert request['kind'] == 'req' and request['op'] == 'lane.connect'
role = request['payload']['lane']
if role == 'supervisor':
    while not (root / 'start-0').exists() or not (root / 'start-1').exists():
        assert time.monotonic() < deadline, 'fixture spare rendezvous timed out'
        time.sleep(0.005)
    (root / 'ordered').write_text('supervisor observed spare before answering')
    print('Permission denied (fixture).', file=sys.stderr, flush=True)
    sys.exit(255)
sys.stdin.buffer.read()
"#, root=dir.to_string_lossy(), reverse=if reversed { "True" } else { "False" });
    sot_log::test_exec::write_executable(&path, script);
    let slots = Arc::new(AtomicUsize::new(0));
    let recipe = sot_protocol::topology::ssh_bridge::SshRecipe::new("teststub", None).unwrap();
    DaemonLaneEndpoint::new(LaneDial::Ssh(recipe, Default::default()), None).with_test_ssh_spawner(Arc::new(move |recipe, gate| {
        let command = gate.command(recipe)?;
        std::process::Command::new("python3").arg("-u").arg(&path).arg(slots.fetch_add(1, Ordering::SeqCst).to_string()).args(command.get_args())
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
            .spawn().map_err(sot_protocol::topology::ssh_bridge::SpawnError::Io)
    }))
}

#[test]
fn the_slow_fixture_uses_its_own_handshake_bound() {
    use sot_log::lane::client::Endpoint;
    let dir = tempfile::tempdir().unwrap();
    let endpoint = rendezvous_endpoint(dir.path(), 3, false).with_test_handshake_bound(Duration::from_secs(60));
    let result = endpoint.connect_supervisor_unchallenged("row-slow-fixture");
    let error = result.err().expect("the fixture deliberately refuses the login").to_string();
    assert!(error.contains("Permission denied"), "the slow fixture must reach its intended Permission denied result: {error}");
    assert!(dir.path().join("ordered").exists(), "the delayed spare arrived before the fixture answered");
}

#[test]
fn fixture_bounds_are_local_to_the_endpoint() {
    use sot_log::lane::client::Endpoint;
    let configured = tempfile::tempdir().unwrap();
    let slow = rendezvous_endpoint(configured.path(), 3, false).with_test_handshake_bound(Duration::from_secs(60));
    let ordinary = tempfile::tempdir().unwrap();
    let default = rendezvous_endpoint(ordinary.path(), 3, false);
    let started = Instant::now();
    let error = default.connect_supervisor_unchallenged("row-default").err().unwrap().to_string();
    assert!(error.contains("handshake timed out"), "an unconfigured endpoint retains CONNECT_BOUND: {error}");
    assert!(started.elapsed() >= sot_log::lane::transport::CONNECT_BOUND);
    assert!(!ordinary.path().join("ordered").exists(), "the ordinary endpoint stopped before the three-second spare entry");
    let error = slow.connect_supervisor_unchallenged("row-configured").err().unwrap().to_string();
    assert!(error.contains("Permission denied"), "only the configured endpoint waits for its slow peer: {error}");
}
