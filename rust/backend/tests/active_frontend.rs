#![cfg(any(windows, target_os = "linux"))]
//! "Active frontend" (2026-09-08 review rework): server-level regression
//! tests through the REAL wire protocol against a real `sotd`, mirroring
//! `switch_latency.rs`'s own real-process posture (no protocol doubles, no
//! mocked `handle_connection`). Gated off macOS like that file and
//! `capsule_workspaces.rs`: the daemon's default-row boot path shells out to
//! a real `tmux` server on Linux unless `SOT_TMUX_SOCK` isolates it, and
//! macOS CI runners don't ship `tmux` by default.
//!
//! Two properties the unit tests in `clients.rs`/`handlers.rs` can't reach
//! because they never go through `server.rs`'s real dispatch loop:
//!
//! 1. Ordinary navigation/typing ops (`tree.root`, `preview.get`,
//!    `workspace.activate{read:true}`, `pty.write`) — EVERY op an earlier
//!    design stamped presence from — leave a connection's activity
//!    untouched; only `fe.presence` does.
//! 2. Two connections sharing an `fe_handle` (a stale reconnect, or a
//!    genuine hostname collision) never both receive an untargeted
//!    `fe.command.send` — exactly one does, by connection identity.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use interprocess::local_socket::tokio::{prelude::*, Stream as LocalStream};
use interprocess::local_socket::GenericFilePath;
use sot_protocol::{codec, op, Frame, HelloReq, Kind};

/// Generous bound for a whole exchange — not a precision timing assertion,
/// just the "don't hang forever" backstop every bounded test needs.
const BOUND: Duration = Duration::from_secs(20);

/// How long to wait for a frame that must NOT arrive before concluding it
/// won't. Comfortably above scheduling jitter, comfortably below `BOUND`.
const ABSENCE_WAIT: Duration = Duration::from_millis(800);

/// One isolated `sotd`, rooted at a fresh temp project, with every path it
/// could touch OUTSIDE that tempdir (workspace-registry config, per-machine
/// state, its default row's tmux server) redirected there too — copied from
/// `switch_latency.rs`'s own `Env::spawn`, trimmed to what this file needs
/// (no slow-op knob).
struct Env {
    _tmp: tempfile::TempDir,
    _runtime_tmp: tempfile::TempDir,
    socket_path: PathBuf,
    tmux_sock: PathBuf,
    daemon: Child,
}

impl Env {
    fn spawn(tag: &str) -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("sot-activefe-")
            .tempdir()
            .expect("tempdir");
        let project_root = tmp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("mkdir project_root");
        let state_root = tmp.path().join("state");
        std::fs::create_dir_all(&state_root).expect("mkdir state_root");
        let config_root = tmp.path().join("config");
        std::fs::create_dir_all(&config_root).expect("mkdir config_root");

        #[cfg(unix)]
        let runtime_base = PathBuf::from("/tmp");
        #[cfg(windows)]
        let runtime_base = std::env::temp_dir();
        let runtime_tmp = tempfile::Builder::new()
            .prefix("sotafwrt-")
            .tempdir_in(runtime_base)
            .expect("runtime tempdir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(runtime_tmp.path(), std::fs::Permissions::from_mode(0o700))
                .expect("chmod runtime tempdir to 0700");
        }

        let socket_path = {
            #[cfg(windows)]
            {
                PathBuf::from(format!(r"\\.\pipe\sot-activefe-{tag}-{}", std::process::id()))
            }
            #[cfg(unix)]
            {
                runtime_tmp.path().join(format!("wire-{tag}.sock"))
            }
        };
        let tmux_sock = runtime_tmp.path().join("tmux.sock");

        let daemon = Command::new(sotd_exe())
            .arg("--socket")
            .arg(&socket_path)
            .arg("--project-root")
            .arg(&project_root)
            .env("LOCALAPPDATA", &state_root)
            .env("XDG_STATE_HOME", &state_root)
            .env("XDG_CONFIG_HOME", &config_root)
            .env("SOT_STATE_HOST", format!("activefe-{tag}"))
            .env("SOT_RUNTIME_DIR", runtime_tmp.path())
            .env("SOT_TMUX_SOCK", &tmux_sock)
            .stdin(Stdio::null())
            .spawn()
            .expect("spawn sotd");

        Self {
            _tmp: tmp,
            _runtime_tmp: runtime_tmp,
            socket_path,
            tmux_sock,
            daemon,
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
        let _ = Command::new("tmux")
            .arg("-S")
            .arg(&self.tmux_sock)
            .arg("kill-server")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn sotd_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sotd"))
}

async fn try_connect(socket_path: &std::path::Path) -> Option<LocalStream> {
    let name = socket_path
        .to_str()
        .expect("socket path is valid UTF-8")
        .to_fs_name::<GenericFilePath>()
        .expect("interpret socket path as a local-socket name");
    tokio::time::timeout(Duration::from_secs(2), LocalStream::connect(name))
        .await
        .ok()
        .and_then(Result::ok)
}

type Conn = tokio::io::BufReader<LocalStream>;

async fn poll_until_connected(socket_path: &std::path::Path) -> Conn {
    let deadline = std::time::Instant::now() + BOUND;
    loop {
        if let Some(s) = try_connect(socket_path).await {
            return tokio::io::BufReader::new(s);
        }
        assert!(std::time::Instant::now() < deadline, "sotd's socket never accepted a connection");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Connect + hello (with `fe_handle`), returning the connection and the next
/// free request id (2, since id 1 is hello).
async fn connect_and_hello(socket_path: &std::path::Path, client_id: &str, fe_handle: &str) -> (Conn, u64) {
    let mut conn = poll_until_connected(socket_path).await;
    let hello = HelloReq {
        client_id: client_id.to_string(),
        session_id: None,
        last_seen_revision: 0,
        token: None,
        protocol: sot_protocol::PROTOCOL_VERSION,
        app_version: sot_protocol::app_version(),
        fe_handle: Some(fe_handle.to_string()),
    };
    codec::write_frame(&mut conn, &Frame::req(1, op::HELLO, serde_json::to_value(&hello).unwrap()), None)
        .await
        .expect("write hello");
    let (frame, _blob) = codec::read_frame(&mut conn).await.expect("read hello reply");
    assert_eq!(frame.id, 1);
    assert!(frame.payload.get("error").is_none(), "hello refused: {:?}", frame.payload);
    (conn, 2)
}

/// Write a request and read replies (skipping evt frames and any other
/// request's reply) until `id`'s `res` frame is seen. Returns its payload.
async fn call(conn: &mut Conn, id: u64, opname: &str, payload: serde_json::Value) -> serde_json::Value {
    codec::write_frame(conn, &Frame::req(id, opname, payload), None)
        .await
        .unwrap_or_else(|e| panic!("write {opname} (id {id}): {e}"));
    let body = async {
        loop {
            let (frame, _blob) = codec::read_frame(conn).await.expect("read reply");
            if frame.kind == Kind::Res && frame.id == id {
                return frame.payload;
            }
        }
    };
    tokio::time::timeout(BOUND, body)
        .await
        .unwrap_or_else(|_| panic!("no reply to {opname} (id {id}) within BOUND"))
}

/// Fire-and-forget: write a request with no reply to wait for (`pty.write`).
async fn fire(conn: &mut Conn, id: u64, opname: &str, payload: serde_json::Value) {
    codec::write_frame(conn, &Frame::req(id, opname, payload), None)
        .await
        .unwrap_or_else(|e| panic!("write {opname} (id {id}): {e}"));
}

/// `version.query`'s roster entry for `client_id`, or `None` if absent.
fn client_row<'a>(version_res: &'a serde_json::Value, client_id: &str) -> Option<&'a serde_json::Value> {
    version_res
        .get("clients")?
        .as_array()?
        .iter()
        .find(|c| c.get("client_id").and_then(|v| v.as_str()) == Some(client_id))
}

/// Finding 1/2/3 (2026-09-08 review): every op that an earlier design
/// stamped presence from — `tree.root`, `preview.get`,
/// `workspace.activate{read:true}`, `pty.write` — leaves this connection's
/// `active` flag `false`. Only `fe.presence` flips it `true`.
#[tokio::test]
async fn navigation_and_typing_ops_never_stamp_presence_only_fe_presence_does() {
    let env = Env::spawn("nav");
    let (mut conn, mut id) = connect_and_hello(&env.socket_path, "presence-test", "win-fe-nav").await;

    let body = async {
        // Every op an earlier design stamped presence from — none of them
        // may flip this connection active.
        let _ = call(&mut conn, id, op::TREE_ROOT, serde_json::json!({"mode": "files"})).await;
        id += 1;
        let _ = call(&mut conn, id, op::PREVIEW_GET, serde_json::json!({"node_id": "does-not-exist"})).await;
        id += 1;
        let _ = call(&mut conn, id, op::WORKSPACE_ACTIVATE, serde_json::json!({"read": true})).await;
        id += 1;
        fire(&mut conn, id, op::PTY_WRITE, serde_json::json!({"data_b64": "eA=="})).await;
        id += 1;

        let v1 = call(&mut conn, id, op::VERSION_QUERY, serde_json::json!({})).await;
        id += 1;
        let row = client_row(&v1, "presence-test").expect("this connection's own roster row");
        assert_eq!(
            row.get("active").and_then(|v| v.as_bool()),
            Some(false),
            "tree.root/preview.get/workspace.activate(read:true)/pty.write must never stamp presence: {v1}"
        );

        // Now the ONLY thing that should stamp it.
        let presence = call(&mut conn, id, op::FE_PRESENCE, serde_json::json!({})).await;
        assert_eq!(presence.get("ok").and_then(|v| v.as_bool()), Some(true));
        id += 1;

        let v2 = call(&mut conn, id, op::VERSION_QUERY, serde_json::json!({})).await;
        let row = client_row(&v2, "presence-test").expect("this connection's own roster row");
        assert_eq!(
            row.get("active").and_then(|v| v.as_bool()),
            Some(true),
            "fe.presence must stamp this connection active: {v2}"
        );
    };
    tokio::time::timeout(BOUND, body).await.expect("exchange did not finish within BOUND");
}

/// Finding 5 (2026-09-08 review): two connections sharing one `fe_handle`
/// (a stale reconnect, or a genuine hostname collision) must never both
/// receive an untargeted `fe.command.send` — resolution is by connection
/// SERIAL, and delivery is filtered server-side before the frame is ever
/// written, so the non-winning connection's request log (here: its own read
/// side) sees nothing at all.
#[tokio::test]
async fn duplicate_handle_connections_get_exactly_one_delivery() {
    let env = Env::spawn("dup");
    let (mut conn_a, mut id_a) = connect_and_hello(&env.socket_path, "dup-a", "win-fe-dup").await;
    let (mut conn_b, _id_b) = connect_and_hello(&env.socket_path, "dup-b", "win-fe-dup").await;

    let body = async {
        // Make conn_a the active frontend.
        let presence = call(&mut conn_a, id_a, op::FE_PRESENCE, serde_json::json!({})).await;
        assert_eq!(presence.get("ok").and_then(|v| v.as_bool()), Some(true));
        id_a += 1;

        // Untargeted fe.command.send from conn_a itself — sender identity is
        // irrelevant to routing, only the resolved ACTIVE connection matters.
        let ack = call(
            &mut conn_a,
            id_a,
            op::FE_COMMAND_SEND,
            serde_json::json!({"cmd": "notify", "args": {"text": "hi"}}),
        )
        .await;
        assert_eq!(ack.get("ok").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            ack.get("resolved_target").and_then(|v| v.as_str()),
            Some("win-fe-dup"),
            "must resolve to the active handle: {ack}"
        );

        // conn_a (the winner) must see the fe.command evt.
        let saw_evt = async {
            loop {
                let (frame, _blob) = codec::read_frame(&mut conn_a).await.expect("read from conn_a");
                if frame.kind == Kind::Evt && frame.op == op::FE_COMMAND {
                    return frame.payload;
                }
            }
        };
        let evt = tokio::time::timeout(BOUND, saw_evt)
            .await
            .expect("the active (winning) connection must receive the fe.command evt");
        assert_eq!(evt.get("cmd").and_then(|v| v.as_str()), Some("notify"));

        // conn_b (the untouched duplicate) must see NO fe.command within
        // ABSENCE_WAIT — proving server-side exclusive delivery, not merely
        // "the FE would have ignored it anyway." Unrelated broadcasts
        // (`workspace.changed` from the daemon's own workspace bookkeeping)
        // reach every connection by design and are not what this test is
        // about, so they are skipped, not failed on.
        let saw_command = async {
            loop {
                let (frame, _blob) = codec::read_frame(&mut conn_b).await.expect("read from conn_b");
                if frame.kind == Kind::Evt && frame.op == op::FE_COMMAND {
                    return frame.payload;
                }
            }
        };
        let saw_command = tokio::time::timeout(ABSENCE_WAIT, saw_command).await;
        assert!(
            saw_command.is_err(),
            "the non-active duplicate connection must receive NO fe.command, got: {saw_command:?}"
        );
    };
    tokio::time::timeout(BOUND, body).await.expect("exchange did not finish within BOUND");
}
