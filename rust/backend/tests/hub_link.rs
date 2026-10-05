#![cfg(target_os = "linux")]
//! The daemon's link to the hub (0031 B2): a real hub `sotd` and a real
//! `sotd` for a `frontend = true` host, joined by a stub `ssh` that relays
//! to the hub's socket with `nc -U` (the `lane_bridge/main.rs` pattern). The
//! hub's `agent.message` broadcast reaches the guest, which files for the
//! handles its own registry lists and answers `agent.filed`.
//!
//! `Env`, `connect_and_hello` and `call` are `topology_set.rs`'s, trimmed.

mod support;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use interprocess::local_socket::tokio::{prelude::*, Stream as LocalStream};
use interprocess::local_socket::GenericFilePath;
use sot_protocol::{codec, op, Frame, HelloReq, Kind};

const BOUND: Duration = Duration::from_secs(20);

const TOML: &str = "hub = \"hub-a\"\n\n[host.hub-a]\ndaemon = true\n\n[host.win-b]\ndaemon = true\nfrontend = true\n";

struct Env {
    _tmp: tempfile::TempDir,
    _runtime_tmp: tempfile::TempDir,
    socket_path: PathBuf,
    comm_root: PathBuf,
    daemon: Child,
}

impl Env {
    /// `stub_path`: a directory with a stub `ssh`, put first on the daemon's `PATH`.
    fn start(tag: &str, self_host: &str, stub_path: Option<&Path>) -> Self {
        let tmp = tempfile::Builder::new().prefix("sot-hublink-").tempdir().expect("tempdir");
        let project_root = tmp.path().join("project");
        let state_root = tmp.path().join("state");
        let config_root = tmp.path().join("config");
        for d in [&project_root, &state_root, &config_root] {
            std::fs::create_dir_all(d).expect("mkdir");
        }
        let hosts_toml = tmp.path().join("hosts.toml");
        std::fs::write(&hosts_toml, TOML).expect("write hosts.toml");
        let (home_root, comm_root) = support::comm_isolation_dirs(tmp.path());
        let runtime_tmp = tempfile::Builder::new().prefix("sothlrt-").tempdir_in("/tmp").expect("runtime tempdir");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(runtime_tmp.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
        }
        let socket_path = runtime_tmp.path().join(format!("wire-{tag}.sock"));
        let mut cmd = support::sotd_command();
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
        if let Some(dir) = stub_path {
            let real = std::env::var_os("PATH").unwrap_or_default();
            cmd.env("PATH", std::env::join_paths(std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&real))).unwrap());
        }
        let daemon = cmd.spawn().expect("spawn sotd");
        Self { _tmp: tmp, _runtime_tmp: runtime_tmp, socket_path, comm_root, daemon }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

type Conn = tokio::io::BufReader<LocalStream>;

async fn connect_and_hello(socket_path: &Path, name: &str) -> Conn {
    let deadline = std::time::Instant::now() + BOUND;
    let mut conn = loop {
        let fs_name = socket_path.to_str().unwrap().to_fs_name::<GenericFilePath>().unwrap();
        if let Ok(Ok(s)) = tokio::time::timeout(Duration::from_secs(2), LocalStream::connect(fs_name)).await {
            break tokio::io::BufReader::new(s);
        }
        assert!(std::time::Instant::now() < deadline, "sotd's socket never accepted a connection");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let hello = HelloReq {
        client_id: name.to_string(),
        session_id: None,
        last_seen_revision: 0,
        token: None,
        protocol: sot_protocol::PROTOCOL_VERSION,
        app_version: sot_protocol::app_version(),
        host: Some("hub-a".to_string()),
        role: "cli".to_string(),
        instance: None,
        name: Some(name.to_string()),
    };
    codec::write_frame(&mut conn, &Frame::req(1, op::HELLO, serde_json::to_value(&hello).unwrap()), None).await.expect("write hello");
    loop {
        let (frame, _) = codec::read_frame(&mut conn).await.expect("read hello reply");
        if frame.kind == Kind::Res && frame.id == 1 {
            assert!(frame.payload.get("error").is_none(), "hello refused: {:?}", frame.payload);
            return conn;
        }
    }
}

/// Sends `agent.send` with sender-minted `id` and reports whether an
/// `agent.receipt` for that id naming `filer` arrives within `within`.
async fn send_and_wait_receipt(conn: &mut Conn, id: &str, to: &str, filer: &str, within: Duration) -> bool {
    let req = serde_json::json!({"from": "kitt-sender", "to": to, "text": format!("hi {id}"), "id": id});
    codec::write_frame(conn, &Frame::req(100, op::AGENT_SEND, req), None).await.expect("write agent.send");
    let body = async {
        loop {
            let (frame, _) = codec::read_frame(conn).await.expect("read");
            if frame.kind == Kind::Evt && frame.op == op::AGENT_RECEIPT && frame.payload["id"] == id {
                return frame.payload["filer"] == filer;
            }
        }
    };
    tokio::time::timeout(within, body).await.unwrap_or(false)
}

fn now_iso(offset_secs: i64) -> String {
    let out = Command::new("date").args(["-u", "-d", &format!("{offset_secs} seconds"), "+%Y-%m-%dT%H:%M:%SZ"]).output().expect("date");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A stub `ssh`: the first invocation lives 4 s (a link that drops), every later one relays for good.
fn write_stub_ssh(dir: &Path, hub_socket: &Path, count: &Path) {
    let script = format!(
        "#!/bin/sh\necho x >> '{c}'\nif [ \"$(wc -l < '{c}')\" -le 1 ]; then exec timeout 4 nc -U '{s}'; fi\nexec nc -U '{s}'\n",
        c = count.display(),
        s = hub_socket.display()
    );
    let path = dir.join("ssh");
    sot_log::test_exec::write_executable(&path, script);
}

fn inbox_lines(env: &Env, h: &str) -> Vec<serde_json::Value> {
    std::fs::read_to_string(env.comm_root.join(format!("inbox/{h}.jsonl")))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

// Items 1-3 of the lane: a listed live handle is filed and receipted by
// `sotd-<box>`; an unlisted one and a listed one with no live session are
// neither; and after the ssh child drops a new one is up and filing again.
#[tokio::test]
async fn the_daemon_files_for_its_own_folder_over_a_link_that_restarts() {
    let stubs = tempfile::tempdir().unwrap();
    let count = stubs.path().join("count");
    let hub = Env::start("hub", "hub-a", None);
    write_stub_ssh(stubs.path(), &hub.socket_path, &count);
    let mut sender = connect_and_hello(&hub.socket_path, "kitt-sender").await;

    let guest = Env::start("guest", "win-b", Some(stubs.path()));
    let registry = serde_json::json!({"agents": {
        "live": {"host": "win-b", "last_seen": now_iso(0)},
        "stale": {"host": "win-b", "last_seen": now_iso(-100_000)},
    }});
    support::write_registry(&guest.comm_root, |doc| *doc = registry);

    // The link comes up after the daemon does: send until a receipt arrives.
    let mut n = 0;
    let deadline = std::time::Instant::now() + BOUND;
    loop {
        n += 1;
        if send_and_wait_receipt(&mut sender, &format!("up-{n}"), "live", "sotd-win-b", Duration::from_secs(2)).await {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "the link never filed");
    }
    assert_eq!(inbox_lines(&guest, "live").last().unwrap()["from"], "kitt-sender");

    // 2. Refusals file nothing and receipt nothing.
    for to in ["nobody", "stale"] {
        assert!(!send_and_wait_receipt(&mut sender, &format!("no-{to}"), to, "sotd-win-b", Duration::from_secs(2)).await, "{to} was receipted");
        assert!(inbox_lines(&guest, to).is_empty(), "{to} was filed");
    }

    // 3. The first ssh child ends after 4 s; a second one is up and files.
    let deadline = std::time::Instant::now() + BOUND;
    while std::fs::read_to_string(&count).unwrap_or_default().lines().count() < 2 {
        assert!(std::time::Instant::now() < deadline, "no second ssh child");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let before = inbox_lines(&guest, "live").len();
    let deadline = std::time::Instant::now() + BOUND;
    loop {
        n += 1;
        if send_and_wait_receipt(&mut sender, &format!("again-{n}"), "live", "sotd-win-b", Duration::from_secs(2)).await {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "the restarted link never filed");
    }
    assert!(inbox_lines(&guest, "live").len() > before);
}
