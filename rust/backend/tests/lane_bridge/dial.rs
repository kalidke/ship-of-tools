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
    let mut client = FeAttachClient::attach(
        daemon_lane_endpoint(&relay),
        target,
        80,
        24,
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
    assert!(!client.run().is_empty(), "the attach names the row's run");

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
    let mut client = FeAttachClient::attach(
        DaemonLaneEndpoint::new(LaneDial::Local(path), None),
        "row-old-daemon".to_string(),
        80,
        24,
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
    assert!(status.contains("no_bridge"), "status must name the missing bridge, got {status:?}");
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
    let mut client = FeAttachClient::attach(
        daemon_lane_endpoint(&relay),
        target,
        80,
        24,
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
    let run_before = client.run();
    assert!(!run_before.is_empty(), "the attach names the row's run before the drop");

    kill_supervisor(&env.state_root);

    let resumed_deadline = Instant::now() + Duration::from_secs(60);
    loop {
        client.pump();
        assert!(!client.is_dead(), "a lane-only drop must never go terminal: {}", client.status_line());
        if !own_supervisor_pids(&env.state_root).is_empty() {
            break;
        }
        assert!(Instant::now() < resumed_deadline, "the daemon never resumed the dropped row");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(own_supervisor_pids(&env.state_root).len(), 1, "ensure_started must spawn exactly one new supervise process");

    // Functional recovery, not merely a resumed process: a fresh input
    // through the SAME client lands, and the leg identity is unchanged.
    // The agent died with its supervisor, so the resume is a new run: an input typed before the client has re-attached to
    // it is never resent (it is reported delivery unknown), so the input is typed again until the new run echoes it.
    let input_deadline = Instant::now() + Duration::from_secs(60);
    let mut last_typed: Option<Instant> = None;
    loop {
        client.pump();
        assert!(!client.is_dead(), "died while resuming: {}", client.status_line());
        if screen_text(&client).contains("lb3-resumed") {
            break;
        }
        if client.status_line() == "attached" && last_typed.is_none_or(|at| at.elapsed() >= Duration::from_secs(3)) {
            client.send_input(b"echo lb3-resumed\r");
            last_typed = Some(Instant::now());
        }
        assert!(Instant::now() < input_deadline, "resumed client's own input never echoed");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // The agent died with its supervisor, so the resume is a new run, and the same client is attached to it.
    assert_ne!(client.run(), run_before, "the resumed row's attach names its new run, got {:?}", client.run());
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

    assert_eq!(own_supervisor_pids(&env.state_root).len(), 0, "no supervise process must exist before the dial");

    let relay = Relay::start(env.socket_path.clone()).await;
    let (_woke, wake) = wake_flag_for_test();
    // The window starts before attach, which starts the client's own health clock.
    let window_deadline = Instant::now() + Duration::from_secs(10);
    let mut client = FeAttachClient::attach(
        daemon_lane_endpoint(&relay),
        target,
        80,
        24,
        wake,
    )
    .expect("attach");

    // R4c: `Reconnect` permits only `StartMode::Resume` — a dial on an
    // unpublished pointer starts nothing and answers absent. Proven over
    // a bounded window, not by waiting for the client to go terminal
    // (main's own behavior too: that needs the full client-side
    // HEALTH_WINDOW, unrelated to this rule): across ~10s, the bridge
    // keeps answering absent (never attaches, never goes dead on its
    // own), the row's phase never moves off "stopped", and no supervise
    // process for it ever exists.
    let mut saw_absent_answer = false;
    loop {
        client.pump();
        assert!(
            !client.is_dead(),
            "a never-started row's bridge dial must not go terminal on its own: {}",
            client.status_line()
        );
        if client.status_line().contains("redialing") {
            saw_absent_answer = true;
        }
        assert_eq!(
            own_supervisor_pids(&env.state_root).len(),
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

// Explicitly spawned SSH stand-ins exercise the real endpoint; parent PATH and SHELL preservation is observed through setup, execution and teardown.

/// Writes the executable that the caller explicitly spawns to relay its pipes to the test-owned socket.

// Parent values are compared as Option<OsString>; diagnostics never format either value.
struct ParentSshEnvironment {
    values: [Option<std::ffi::OsString>; 2],
    failures: std::sync::Mutex<Vec<String>>,
}
impl ParentSshEnvironment {
    fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self { values: [std::env::var_os("PATH"), std::env::var_os("SHELL")], failures: Default::default() })
    }
    fn observe(&self, phase: &str) {
        for (key, before) in ["PATH", "SHELL"].into_iter().zip(&self.values) {
            if std::env::var_os(key) != *before { self.failures.lock().unwrap().push(format!("{key} at {phase} in SSH fixture")); }
        }
    }
    fn finish(&self) {
        self.observe("after teardown");
        let failures = self.failures.lock().unwrap();
        assert!(failures.is_empty(), "ssh_fixtures_preserve_parent_path_and_shell: {}", failures.join("; "));
    }
}

fn stub_ssh_relaying_to(dir: &Path, socket: &Path, guard: &ParentSshEnvironment) {
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
    sot_platform::test_exec::write_executable(&dir.join("ssh"), script);
    guard.observe("after preparing relay");
}

fn stub_endpoint(path: PathBuf, guard: Arc<ParentSshEnvironment>) -> DaemonLaneEndpoint {
    guard.observe("after preparing endpoint");
    let recipe = sot_topology::ssh_bridge::SshRecipe::new("teststub", None).expect("plain host name");
    DaemonLaneEndpoint::new(LaneDial::Ssh(recipe, Default::default()), None).with_test_ssh_spawner(Arc::new(move |command| {
        guard.observe("at spawn");
        let spawned = std::process::Command::new(&path).args(command.get_args())
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
            .spawn();
        guard.observe("child active");
        spawned
    }))
}

/// Exits before connecting, with one stderr line; the lane status keeps the transport error and adds this diagnosis once.
fn stub_ssh_dying_with(dir: &Path, stderr_line: &str, guard: &ParentSshEnvironment) {
    let script = format!("#!/bin/sh\necho '{stderr_line}' >&2\nexit 255\n");
    let path = dir.join("ssh");
    sot_platform::test_exec::write_executable(&path, script);
    guard.observe("after preparing dying fixture");
}

#[tokio::test]
async fn fe_client_reaches_a_capsule_row_through_a_stub_ssh_child() {
    let _serial = SERIAL.lock().await;
    let guard = ParentSshEnvironment::new();
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found next to sotd — build it first (cargo build -p sot-log --bin sot-capsule)");

    let env = Env::new("lb-ssh1");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (_workspace_id, target) = create_ready_capsule_row(&env, &mut conn, &mut next_id, "lb-ssh1-workspace").await;

    let relay = Relay::start(env.socket_path.clone()).await;
    let stub_dir = tempfile::Builder::new().prefix("sot-stub-ssh-").tempdir().expect("tempdir");
    stub_ssh_relaying_to(stub_dir.path(), &relay.path, &guard);

    let endpoint = stub_endpoint(stub_dir.path().join("ssh"), guard.clone());
    let (_woke, wake) = wake_flag_for_test();
    let mut client = FeAttachClient::attach(
        endpoint,
        target,
        80,
        24,
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
    // The peer report is the DAEMON's: a live checkpoint only reaches this far once the bridge's split-identity proof
    // (`DaemonLaneEndpoint::challenge` against the daemon's own `LaneConnectRes` report) has succeeded over this exact
    // stub-ssh connection.
    assert!(!client.run().is_empty(), "the attach names the row's run");
    guard.observe("after result");

    drop(client);
    env.kill_daemon_bounded().await;
    guard.finish();
}

#[tokio::test]
async fn a_stub_ssh_that_dies_first_puts_its_stderr_line_in_the_lane_status() {
    let _serial = SERIAL.lock().await;
    let guard = ParentSshEnvironment::new();
    let stub_dir = tempfile::Builder::new().prefix("sot-stub-ssh-dying-").tempdir().expect("tempdir");
    stub_ssh_dying_with(stub_dir.path(), "Permission denied (publickey).", &guard);

    let endpoint = stub_endpoint(stub_dir.path().join("ssh"), guard.clone());
    let (_woke, wake) = wake_flag_for_test();
    // `attach()`'s only `Err` is a failed OS thread spawn (`attach_inner`,
    // `sot-log/src/attach_client/client.rs`) — the dial itself runs on the worker
    // thread it spawns, and `Unreachable` (what a dying child classifies
    // as) retries rather than failing this call outright (ADR 0045
    // decision 4: "Unreachable must retry, never go terminal on its
    // own"). The observable this test proves is the status TEXT a
    // retrying attempt already carries, not a terminal/dead client.
    let mut client = FeAttachClient::attach(
        endpoint,
        "row-does-not-matter-the-child-dies-first".to_string(),
        80,
        24,
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
    guard.observe("after result");
    client.shutdown(Duration::from_secs(5)); drop(client);
    guard.finish();
}

// -----------------------------------------------------------------------
// (iv) The row number is the one gate: a row that writes an older wire integer in every envelope attaches through the
// bridge, which pipes bytes and judges neither.
// -----------------------------------------------------------------------

#[tokio::test]
async fn a_row_that_writes_an_older_wire_integer_attaches_through_the_bridge() {
    use sot_protocol::codec::{read_frame_blocking_blob, write_frame_blocking_blob};
    let _serial = SERIAL.lock().await;
    let env = Env::new("lb10");
    // A row that was never started, whose socket this test serves itself.
    env.seed_capsule_toml("ws-lb10-standin", "lb10-standin", &env.workspace_project_root, "none");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id += 1;
    let _ = next_id;
    let row = find_row(&list_payload, "ws-lb10-standin").expect("the seeded row is registered");
    let target = row["session_name"].as_str().expect("session_name").to_string();
    let state_dir = env.state_root.join("sot").join("workspaces").join("ws-lb10-standin");
    let path = sot_log::row::socket_path_in(env._runtime_tmp.path(), &sot_platform::host::state_dir::state_dir_hash(&state_dir))
        .expect("the row socket path fits");
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind the stand-in row");
    let older = sot_protocol::PROTOCOL_VERSION - 1;
    std::thread::spawn(move || {
        // The daemon's own dial connects and authenticates first, and says nothing; the client's connection is the next.
        while let Ok((stream, _)) = listener.accept() {
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut out = stream;
            while let Ok((frame, _)) = read_frame_blocking_blob(&mut reader) {
                let mut reply = match frame.op.as_str() {
                    "row.hello" => {
                        let (pid, created) = sot_log::row::self_identity().expect("own identity");
                        sot_protocol::Frame::res(
                            frame.id,
                            "row.hello",
                            serde_json::json!({"pid": pid, "created": created, "phase": "ready", "run": "run-old-v", "row": sot_log::row::ROW_PROTOCOL}),
                        )
                    }
                    "row.attach" => sot_protocol::Frame::res(frame.id, "row.attach", serde_json::json!({"run": "run-old-v"})),
                    _ => sot_protocol::Frame::res(frame.id, &frame.op, serde_json::json!({})),
                };
                reply.v = older;
                if write_frame_blocking_blob(&mut out, &reply, None).is_err() {
                    break;
                }
                if frame.op == "row.attach" {
                    let mut parser = vt100_ctt::Parser::new(24, 80, 0);
                    parser.process(b"older wire, same row number");
                    let checkpoint = parser.screen().checkpoint().expect("a checkpoint");
                    let mut evt = sot_protocol::Frame::evt(
                        "row.snapshot",
                        serde_json::json!({"last": true, "blob": {"len": checkpoint.len(), "mime": "application/octet-stream"}}),
                    );
                    evt.v = older;
                    if write_frame_blocking_blob(&mut out, &evt, Some(&checkpoint)).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let relay = Relay::start(env.socket_path.clone()).await;
    let (_woke, wake) = wake_flag_for_test();
    let mut client = FeAttachClient::attach(daemon_lane_endpoint(&relay), target, 80, 24, wake).expect("attach");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !client.is_checkpointed() {
        client.pump();
        assert!(!client.is_dead(), "a row on an older wire integer was refused through the bridge: {}", client.status_line());
        assert!(Instant::now() < deadline, "never checkpointed through the bridge; status {:?}", client.status_line());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(client.screen().contents().contains("older wire, same row number"), "{}", client.screen().contents());
    assert_eq!(client.run(), "run-old-v");
    drop(client);
    relay.cut();
    env.kill_daemon_bounded().await;
}
