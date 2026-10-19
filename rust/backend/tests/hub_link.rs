#![cfg(target_os = "linux")]
//! The daemon's link to the hub (decision 0031): a real hub `sotd` and a real
//! `sotd` for a `frontend = true` host, joined by a stub `ssh` that relays
//! to the hub's socket with `nc -U` (the `lane_bridge/main.rs` pattern). A
//! `comm.file` the hub's own folder does not list is offered to the guest over
//! its link; the guest files for the handles its own registry lists, and the
//! hub's answer is the guest's.
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
        name: Some(name.to_string()),
        ..HelloReq::this_process(name, "cli", Some("hub-a".to_string())).expect("this process's account")
    };
    codec::write_frame(&mut conn, &Frame::req(1, op::HELLO, serde_json::to_value(&hello).unwrap()), None).await.expect("write hello");
    tokio::time::timeout(BOUND, async {
        loop {
            let (frame, _) = codec::read_frame(&mut conn).await.expect("read hello reply");
            if frame.kind == Kind::Res && frame.id == 1 {
                assert!(frame.payload.get("error").is_none(), "hello refused: {:?}", frame.payload);
                return;
            }
        }
    })
    .await
    .expect("the hello reply within BOUND");
    conn
}

/// Sends one `comm.file` for `to` to the hub and returns the payload of its answer, which comes within `BOUND`.
async fn comm_file(conn: &mut Conn, id: u64, to: &str) -> serde_json::Value {
    let req = serde_json::json!({"from": "sender-a", "to": to, "text": format!("hi {id}")});
    codec::write_frame(conn, &Frame::req(id, op::COMM_FILE, req), None).await.expect("write comm.file");
    tokio::time::timeout(BOUND, async {
        loop {
            let (frame, _) = codec::read_frame(conn).await.expect("read");
            if frame.kind == Kind::Res && frame.id == id && frame.op == op::COMM_FILE {
                return frame.payload;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no answer to comm.file for @{to} within {BOUND:?}"))
}

/// Asks the hub's roster (`version.query`) until it lists a connection named `name`, and fails when `BOUND` passes
/// first.
async fn wait_for_client(conn: &mut Conn, n: &mut u64, name: &str) {
    let deadline = std::time::Instant::now() + BOUND;
    loop {
        *n += 1;
        codec::write_frame(conn, &Frame::req(*n, op::VERSION_QUERY, serde_json::json!({})), None).await.expect("write version.query");
        let id = *n;
        let listed = tokio::time::timeout(BOUND, async {
            loop {
                let (frame, _) = codec::read_frame(conn).await.expect("read");
                if frame.kind == Kind::Res && frame.id == id {
                    break frame.payload["clients"].as_array().is_some_and(|cs| cs.iter().any(|c| c["name"] == name));
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("no answer to version.query within {BOUND:?}"));
        if listed {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "no connection named {name} on the hub's roster within {BOUND:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn now_iso(offset_secs: i64) -> String {
    let ran = sot_log::test_wait::run_within(
        Command::new("date").args(["-u", "-d", &format!("{offset_secs} seconds"), "+%Y-%m-%dT%H:%M:%SZ"]),
        BOUND,
    )
    .ok();
    ran.stdout.trim().to_string()
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

// A handle only the linked box lists is filed there and the hub answers `ok`; that box's refusal comes back as its
// own words, and a handle no box lists is `not_here`; after the ssh child drops, a new link is up and filing again.
#[tokio::test]
async fn the_hub_files_into_a_linked_box_and_answers_with_its_verdict() {
    let stubs = tempfile::tempdir().unwrap();
    let count = stubs.path().join("count");
    let hub = Env::start("hub", "hub-a", None);
    write_stub_ssh(stubs.path(), &hub.socket_path, &count);
    let mut sender = connect_and_hello(&hub.socket_path, "alpha-sender").await;

    let guest = Env::start("guest", "win-b", Some(stubs.path()));
    let registry = serde_json::json!({"agents": {
        "live": {"host": "win-b", "last_seen": now_iso(0)},
        "stale": {"host": "win-b", "last_seen": now_iso(-100_000)},
    }});
    support::write_registry(&guest.comm_root, |doc| *doc = registry);

    // 1. Once the guest's link is on the hub's roster, one comm.file for a handle only the guest lists is filed there.
    let mut n = 0;
    wait_for_client(&mut sender, &mut n, "sotd-win-b").await;
    n += 1;
    assert_eq!(comm_file(&mut sender, n, "live").await, serde_json::json!({"ok": true}), "the hub's answer for @live");
    let line = inbox_lines(&guest, "live").last().cloned().expect("a line in the guest's inbox");
    assert_eq!((line["from"].as_str(), line["msg"].as_str()), (Some("sender-a"), Some(format!("hi {n}").as_str())));
    assert!(inbox_lines(&hub, "live").is_empty(), "the hub filed into its own folder");

    // 2. The guest's refusal is the answer; a handle no box lists is not_here. Neither files anything.
    n += 1;
    assert_eq!(
        comm_file(&mut sender, n, "stale").await,
        serde_json::json!({"error": "no live session holds @stale", "code": "no_live_session"})
    );
    n += 1;
    assert_eq!(
        comm_file(&mut sender, n, "nobody").await,
        serde_json::json!({"error": "no box knows that handle: nobody", "code": "not_here"})
    );
    assert!(inbox_lines(&guest, "stale").is_empty() && inbox_lines(&guest, "nobody").is_empty(), "a refusal filed");

    // 3. The first ssh child ends after 4 s; once a second one is up, the next comm.file is filed again.
    let deadline = std::time::Instant::now() + BOUND;
    while std::fs::read_to_string(&count).unwrap_or_default().lines().count() < 2 {
        assert!(std::time::Instant::now() < deadline, "no second ssh child");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    // The roster can still list the first link for a moment, so this step asks until the answer is `ok`.
    let before = inbox_lines(&guest, "live").len();
    let deadline = std::time::Instant::now() + BOUND;
    loop {
        n += 1;
        let answer = comm_file(&mut sender, n, "live").await;
        if answer == serde_json::json!({"ok": true}) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "the restarted link never filed @live; the last answer: {answer}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(inbox_lines(&guest, "live").len(), before + 1, "one ok, one line");
}

// A box the topology says holds a link, with none attached, may hold the handle: the hub's `not_here` names it, and
// once every such box is linked the refusal is the plain one (the step after the link is up, in the test above).
#[tokio::test]
async fn not_here_names_the_boxes_that_hold_no_link() {
    let hub = Env::start("lonehub", "hub-a", None);
    let mut sender = connect_and_hello(&hub.socket_path, "sender-a").await;
    assert_eq!(
        comm_file(&mut sender, 2, "nobody").await,
        serde_json::json!({"error": "no linked box knows that handle: nobody; not linked: win-b", "code": "not_here"})
    );
}
