#![cfg(any(windows, target_os = "linux"))]
//! `topology.set`/`topology.changed` (plan §B "Editing the master list"):
//! a real `sotd` over the REAL wire protocol, same posture as
//! `ping_reaper.rs` (no protocol doubles, no mocked `handle_connection`) —
//! `Env`/`connect_and_hello`/`call`/`sotd_exe` copied and trimmed from that
//! file (its own header explains why: each `tests/*.rs` binary is a
//! separate compilation unit).
//!
//! Everything ELSE about `topology.set`'s refusal matrix (not_hub,
//! remove_hub, remove_self, has_running_rows, invalid) is exercised at the
//! handler level in `src/topology_set.rs`'s own unit tests — cheaper, and
//! this file's job is narrower: prove the real daemon binary actually
//! dispatches `topology.set`, writes tmp+rename to the real file on disk,
//! and broadcasts `topology.changed` to a SEPARATE connection, over the
//! real socket, not just in-process.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use interprocess::local_socket::tokio::{prelude::*, Stream as LocalStream};
use interprocess::local_socket::GenericFilePath;
use sot_protocol::{codec, op, Frame, HelloReq, Kind};

const BOUND: Duration = Duration::from_secs(20);

/// One isolated `sotd` declared as the hub of a one-host topology, with
/// every path it could touch redirected into a fresh tempdir.
struct Env {
    _tmp: tempfile::TempDir,
    _runtime_tmp: tempfile::TempDir,
    hosts_toml: PathBuf,
    socket_path: PathBuf,
    daemon: Child,
}

impl Env {
    /// `hub` is this daemon's own declared host AND the topology's `hub`
    /// (a hub daemon); `non_hub` instead declares itself something else
    /// while the file still names `hub-a` as hub (proving `not_hub`
    /// against the REAL daemon, not just the handler unit test).
    fn spawn(tag: &str, self_host: &str, hosts_toml_text: &str) -> Self {
        let tmp = tempfile::Builder::new().prefix("sot-toposet-").tempdir().expect("tempdir");
        let project_root = tmp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("mkdir project_root");
        let state_root = tmp.path().join("state");
        std::fs::create_dir_all(&state_root).expect("mkdir state_root");
        let config_root = tmp.path().join("config");
        std::fs::create_dir_all(&config_root).expect("mkdir config_root");
        let hosts_toml = tmp.path().join("hosts.toml");
        std::fs::write(&hosts_toml, hosts_toml_text).expect("write hosts.toml");

        #[cfg(unix)]
        let runtime_base = PathBuf::from("/tmp");
        #[cfg(windows)]
        let runtime_base = std::env::temp_dir();
        let runtime_tmp = tempfile::Builder::new().prefix("sottsrt-").tempdir_in(runtime_base).expect("runtime tempdir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(runtime_tmp.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod runtime tempdir");
        }

        let socket_path = {
            #[cfg(windows)]
            {
                PathBuf::from(format!(r"\\.\pipe\sot-toposet-{tag}-{}", std::process::id()))
            }
            #[cfg(unix)]
            {
                runtime_tmp.path().join(format!("wire-{tag}.sock"))
            }
        };
        let daemon = Command::new(sotd_exe())
            .arg("--socket")
            .arg(&socket_path)
            .arg("--project-root")
            .arg(&project_root)
            .env("LOCALAPPDATA", &state_root)
            .env("XDG_STATE_HOME", &state_root)
            .env("XDG_CONFIG_HOME", &config_root)
            .env("SOT_SELF_HOST", self_host)
            .env("SOT_RUNTIME_DIR", runtime_tmp.path())
            .env("SOT_HOSTS", &hosts_toml)
            .stdin(Stdio::null())
            .spawn()
            .expect("spawn sotd");

        Self { _tmp: tmp, _runtime_tmp: runtime_tmp, hosts_toml, socket_path, daemon }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

fn sotd_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sotd"))
}

type Conn = tokio::io::BufReader<LocalStream>;

async fn try_connect(socket_path: &std::path::Path) -> Option<LocalStream> {
    let name = socket_path.to_str().expect("utf8 socket path").to_fs_name::<GenericFilePath>().expect("as local-socket name");
    tokio::time::timeout(Duration::from_secs(2), LocalStream::connect(name)).await.ok().and_then(Result::ok)
}

async fn connect_and_hello(socket_path: &std::path::Path, client_id: &str, host: &str) -> (Conn, u64) {
    let deadline = std::time::Instant::now() + BOUND;
    let mut conn = loop {
        if let Some(s) = try_connect(socket_path).await {
            break tokio::io::BufReader::new(s);
        }
        assert!(std::time::Instant::now() < deadline, "sotd's socket never accepted a connection");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let hello = HelloReq {
        client_id: client_id.to_string(),
        session_id: None,
        last_seen_revision: 0,
        token: None,
        protocol: sot_protocol::PROTOCOL_VERSION,
        app_version: sot_protocol::app_version(),
        host: Some(host.to_string()),
        role: "cli".to_string(),
        instance: None,
        name: Some(client_id.to_string()),
    };
    codec::write_frame(&mut conn, &Frame::req(1, op::HELLO, serde_json::to_value(&hello).unwrap()), None)
        .await
        .expect("write hello");
    let (frame, _blob) = codec::read_frame(&mut conn).await.expect("read hello reply");
    assert!(frame.payload.get("error").is_none(), "hello refused: {:?}", frame.payload);
    (conn, 2)
}

async fn call(conn: &mut Conn, id: u64, opname: &str, payload: serde_json::Value) -> serde_json::Value {
    codec::write_frame(conn, &Frame::req(id, opname, payload), None).await.unwrap_or_else(|e| panic!("write {opname}: {e}"));
    let body = async {
        loop {
            let (frame, _blob) = codec::read_frame(conn).await.expect("read reply");
            if frame.kind == Kind::Res && frame.id == id {
                return frame.payload;
            }
        }
    };
    tokio::time::timeout(BOUND, body).await.unwrap_or_else(|_| panic!("no reply to {opname} within BOUND"))
}

const HUB_TOML: &str = "hub = \"hub-a\"\n\n[host.hub-a]\ndaemon = true\n";

#[tokio::test]
async fn topology_set_writes_the_real_file_and_broadcasts_over_the_real_wire() {
    let env = Env::spawn("add", "hub-a", HUB_TOML);
    let (mut editor, eid) = connect_and_hello(&env.socket_path, "editor", "hub-a").await;
    let (mut watcher, _wid) = connect_and_hello(&env.socket_path, "watcher", "hub-a").await;

    let edit = serde_json::json!({"edit": {"kind": "add_host", "name": "gamma", "daemon": true}});
    let res = call(&mut editor, eid, op::TOPOLOGY_SET, edit).await;
    assert_eq!(res.get("ok").and_then(|v| v.as_bool()), Some(true), "{res:?}");
    let hash = res.get("hash").and_then(|v| v.as_str()).expect("hash present").to_string();

    // The file on disk is the real, freshly written copy.
    let on_disk = std::fs::read_to_string(&env.hosts_toml).expect("read hosts.toml");
    assert!(on_disk.contains("[host.gamma]"), "{on_disk}");
    assert_eq!(sot_protocol::topology::hash_text(&on_disk), hash);

    // A SEPARATE connection (never sent the edit) sees the broadcast.
    let (frame, _blob) = tokio::time::timeout(BOUND, codec::read_frame(&mut watcher))
        .await
        .expect("topology.changed within BOUND")
        .expect("read topology.changed");
    assert_eq!(frame.op, op::TOPOLOGY_CHANGED);
    assert_eq!(frame.kind, Kind::Evt);
    assert_eq!(frame.payload.get("hash").and_then(|v| v.as_str()), Some(hash.as_str()));
}

#[tokio::test]
async fn a_non_hub_daemon_refuses_topology_set_over_the_real_wire() {
    // The file names `hub-a` as hub; this daemon declares itself `beta` —
    // a real, second, non-hub daemon.
    let env = Env::spawn("nonhub", "beta", HUB_TOML);
    let (mut conn, id) = connect_and_hello(&env.socket_path, "editor", "beta").await;
    let edit = serde_json::json!({"edit": {"kind": "add_host", "name": "gamma"}});
    let res = call(&mut conn, id, op::TOPOLOGY_SET, edit).await;
    assert_eq!(res.get("code").and_then(|v| v.as_str()), Some("not_hub"), "{res:?}");
    assert!(res.get("error").and_then(|v| v.as_str()).unwrap_or("").contains("hub-a"));
}
