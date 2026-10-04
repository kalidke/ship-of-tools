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
//! handler level in `src/topology/set.rs`'s own unit tests — cheaper, and
//! this file's job is narrower: prove the real daemon binary actually
//! dispatches `topology.set`, writes tmp+rename to the real file on disk,
//! and broadcasts `topology.changed` to a SEPARATE connection, over the
//! real socket, not just in-process.
//!
//! `mod support;` is used for exactly one helper, `comm_isolation_dirs` —
//! this file's own `Env` stays local, same reasoning as `ping_reaper.rs`.

mod support;

use std::path::{Path, PathBuf};
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
    comm_root: PathBuf,
    daemon: Child,
}

impl Env {
    /// `hub` is this daemon's own declared host AND the topology's `hub`
    /// (a hub daemon); `non_hub` instead declares itself something else
    /// while the file still names `hub-a` as hub (proving `not_hub`
    /// against the REAL daemon, not just the handler unit test).
    fn spawn(tag: &str, self_host: &str, hosts_toml_text: &str) -> Self {
        Self::start(tag, self_host, hosts_toml_text, None)
    }

    /// A guest daemon of `hub`: its places on tmpfs, off this box's own disk,
    /// so it is its comm folder's guest and forwards every filing; and its
    /// runtime base holding `sot-relay.sock` linked to the hub's socket,
    /// where the relay tunnel lands it.
    #[cfg(target_os = "linux")]
    fn spawn_guest(tag: &str, self_host: &str, hosts_toml_text: &str, hub: &Env) -> Self {
        Self::start(tag, self_host, hosts_toml_text, Some(&hub.socket_path))
    }

    /// `hub` is a guest's: the socket of the hub it forwards to.
    fn start(tag: &str, self_host: &str, hosts_toml_text: &str, hub: Option<&Path>) -> Self {
        let tmp = match hub {
            Some(_) => tempfile::Builder::new().prefix("sot-toposet-").tempdir_in("/dev/shm").expect("tmpfs tempdir"),
            None => tempfile::Builder::new().prefix("sot-toposet-").tempdir().expect("tempdir"),
        };
        let project_root = tmp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("mkdir project_root");
        let state_root = tmp.path().join("state");
        std::fs::create_dir_all(&state_root).expect("mkdir state_root");
        let config_root = tmp.path().join("config");
        std::fs::create_dir_all(&config_root).expect("mkdir config_root");
        let hosts_toml = tmp.path().join("hosts.toml");
        std::fs::write(&hosts_toml, hosts_toml_text).expect("write hosts.toml");
        let (home_root, comm_root) = support::comm_isolation_dirs(tmp.path());

        #[cfg(unix)]
        let runtime_base = PathBuf::from("/tmp");
        #[cfg(windows)]
        let runtime_base = std::env::temp_dir();
        let runtime_tmp = tempfile::Builder::new().prefix("sottsrt-").tempdir_in(runtime_base).expect("runtime tempdir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(runtime_tmp.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod runtime tempdir");
            if let Some(hub) = hub {
                std::os::unix::fs::symlink(hub, runtime_tmp.path().join("sot-relay.sock")).expect("link the hub's socket");
            }
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
        let mut cmd = Command::new(sotd_exe());
        cmd.arg("--socket")
            .arg(&socket_path)
            .arg("--project-root")
            .arg(&project_root)
            .env("LOCALAPPDATA", &state_root)
            .env("XDG_STATE_HOME", &state_root)
            .env("XDG_CONFIG_HOME", &config_root)
            .env("SOT_SELF_HOST", self_host)
            .env("SOT_RUNTIME_DIR", runtime_tmp.path())
            .env("HOME", &home_root)
            .env("USERPROFILE", &home_root)
            .env("SOT_COMM_HOME", &comm_root)
            .env("SOT_HOSTS", &hosts_toml)
            .stdin(Stdio::null());
        if hub.is_some() {
            cmd.env("XDG_RUNTIME_DIR", runtime_tmp.path());
        }
        let daemon = cmd.spawn().expect("spawn sotd");

        Self { _tmp: tmp, _runtime_tmp: runtime_tmp, hosts_toml, socket_path, comm_root, daemon }
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
    // The single next frame is not necessarily the hello reply — a
    // broadcast evt can legitimately land first — so skip anything that
    // is not hello's own `res`, same as `call` below.
    let frame = loop {
        let (frame, _blob) = codec::read_frame(&mut conn).await.expect("read hello reply");
        if frame.kind == Kind::Res && frame.id == 1 {
            break frame;
        }
    };
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

/// The daemon's `inbox-lock-manager`, once it has written one.
async fn lock_record(env: &Env) -> String {
    let record = env.comm_root.join("inbox-lock-manager");
    let deadline = std::time::Instant::now() + BOUND;
    loop {
        if let Ok(t) = std::fs::read_to_string(&record) {
            return t;
        }
        assert!(std::time::Instant::now() < deadline, "sotd never wrote {}", record.display());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(target_os = "linux")]
const GUEST_TOML: &str = "hub = \"hub-a\"\n\n[host.hub-a]\ndaemon = true\n\n[host.guest-b]\ndaemon = true\n";

// 0031 B1: a guest daemon's `comm.file` forwards, over the real wire to a
// real hub whose record is its own — the line lands in the hub's inbox, not
// the guest's, and the guest answers `ok`.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_guests_forward_files_at_the_hub_over_the_real_wire() {
    let hub = Env::spawn("fwd", "hub-a", GUEST_TOML);
    drop(connect_and_hello(&hub.socket_path, "probe", "hub-a").await);
    lock_record(&hub).await;
    let now = Command::new("date").args(["-u", "+%Y-%m-%dT%H:%M:%SZ"]).output().expect("date").stdout;
    let now = String::from_utf8(now).unwrap().trim().to_string();
    let registry = serde_json::json!({"agents": {"peer": {"host": "hub-a", "last_seen": now}}});
    std::fs::write(hub.comm_root.join("registry.json"), registry.to_string()).expect("write registry");

    let guest = Env::spawn_guest("fwd-guest", "guest-b", GUEST_TOML, &hub);
    let (mut conn, id) = connect_and_hello(&guest.socket_path, "guest-sender", "guest-b").await;
    let req = sot_protocol::CommFileReq {
        from: "guest-sender".into(),
        to: "peer".into(),
        text: "forwarded hi".into(),
        broadcast: false,
        forwarded: false,
    };
    let answer = call(&mut conn, id, op::COMM_FILE, serde_json::to_value(&req).unwrap()).await;
    assert_eq!(answer, serde_json::json!({"ok": true}));
    assert!(!guest.comm_root.join("inbox/peer.jsonl").exists(), "the guest filed it itself");
    let inbox = std::fs::read_to_string(hub.comm_root.join("inbox/peer.jsonl")).expect("the hub's inbox");
    let line: serde_json::Value = serde_json::from_str(inbox.trim_end()).expect("one line");
    assert_eq!((line["from"].as_str(), line["msg"].as_str()), (Some("guest-sender"), Some("forwarded hi")));
}

#[tokio::test]
async fn topology_set_writes_the_real_file_and_broadcasts_over_the_real_wire() {
    let env = Env::spawn("add", "hub-a", HUB_TOML);
    let (mut editor, eid) = connect_and_hello(&env.socket_path, "editor", "hub-a").await;
    let (mut watcher, _wid) = connect_and_hello(&env.socket_path, "watcher", "hub-a").await;

    // 0031 B1: a booted hub names its inbox lock manager in its comm home,
    // and its machine's id as the record's writer.
    let text = lock_record(&env).await;
    let lines: Vec<&str> = text.lines().collect();
    let mid = lines.get(1).copied().unwrap_or("");
    assert!(
        lines.len() == 2
            && !mid.is_empty()
            && (lines[0].starts_with("nfs4 ") || lines[0] == format!("local {mid}") || lines[0] == format!("none@{mid}")),
        "inbox-lock-manager is not an `nfs4 …`, `local <machine-id>` or `none@<machine-id>` line then that machine id: {text:?}"
    );
    #[cfg(target_os = "linux")]
    assert_eq!(mid, std::fs::read_to_string("/etc/machine-id").unwrap().trim(), "line 2 is this machine's id");

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
