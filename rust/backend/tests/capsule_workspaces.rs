#![cfg(windows)]
//! ADR 0042 slice L1a real-process integration test: a real `sotd`, a real
//! `sot-capsule.exe` it spawns DETACHED, talking the actual wire protocol
//! over a real named pipe — the same posture `rust/log/tests/supervisor_win.rs`
//! takes for the supervisor authority one layer down. Requires
//! `sot-capsule.exe` already built into the SAME target directory as
//! `sotd.exe` (the CI job builds it first — see `.github/workflows/rust.yml`'s
//! `conpty-windows-2022` job; production locates it the identical way, next
//! to the daemon's own executable).
//!
//! Every wait below is a BOUNDED POLL for an external, observable fact (a
//! named pipe accepting a connection, a `workspace.list` row's own `phase`
//! field) — never a sleep-and-hope.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use interprocess::local_socket::tokio::{prelude::*, Stream as LocalStream};
use interprocess::local_socket::GenericFilePath;
use sot_protocol::{codec, op, Frame, HelloReq, Kind};

/// Real-process tests share one CI runner; serialize them like
/// `supervisor_win.rs`'s own `SERIAL` — a spawned `sotd` plus a spawned
/// `sot-capsule` plus a spawned `cmd.exe` is real load on a two-core box.
/// `tokio::sync::Mutex`, not `std::sync::Mutex`: this test is async and
/// holds the guard across `.await` points for its whole body, which an
/// async-aware mutex is what avoids a `clippy::await_holding_lock` flag
/// (`supervisor_win.rs`'s own tests are synchronous, so a std mutex is
/// right there — this file's own async shape needs the other one).
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn sotd_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sotd"))
}

/// Resolved the same way production does — `current_exe().parent()` — but
/// from the TEST binary's own known sibling (`sotd.exe`'s own directory),
/// since a `tests/*.rs` binary itself lives in `target/<profile>/deps/`,
/// not `target/<profile>/`.
fn sot_capsule_exe() -> PathBuf {
    sotd_exe().with_file_name("sot-capsule.exe")
}

/// Reaps a spawned child on every exit path, mirroring
/// `supervisor_win.rs`'s own `KillGuard`.
struct KillGuard(Option<Child>);
impl KillGuard {
    fn kill_and_wait(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}
impl Drop for KillGuard {
    fn drop(&mut self) {
        self.kill_and_wait();
    }
}

/// Bounded async poll for an external, observable fact — the async
/// counterpart of `supervisor_win.rs`'s own synchronous `poll_until`
/// (this test is written against the async `interprocess`/tokio local-
/// socket client, so its own waits are async top to bottom rather than
/// bridging into a nested `block_on`).
async fn poll_until<T, F, Fut>(mut attempt: F, timeout: Duration, what: &str) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = attempt().await {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// One isolated test environment: its own project root, state root
/// (`%LOCALAPPDATA%`), and config root (`%XDG_CONFIG_HOME%` — the
/// workspace TOML registry) — so a `sotd` launched with these env vars
/// touches nothing on the real machine and two test runs never collide.
struct Env {
    _tmp: tempfile::TempDir,
    project_root: PathBuf,
    state_root: PathBuf,
    config_root: PathBuf,
    socket_path: PathBuf,
}

impl Env {
    fn new(tag: &str) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project_root = tmp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("mkdir project_root");
        let state_root = tmp.path().join("state");
        std::fs::create_dir_all(&state_root).expect("mkdir state_root");
        let config_root = tmp.path().join("config");
        std::fs::create_dir_all(&config_root).expect("mkdir config_root");
        // A named pipe, not a filesystem path with real collision risk, but
        // still unique per test process so a re-run never collides with a
        // still-tearing-down prior instance.
        let socket_path = PathBuf::from(format!(r"\\.\pipe\sot-test-{tag}-{}", std::process::id()));
        Self { _tmp: tmp, project_root, state_root, config_root, socket_path }
    }

    /// Spawn a real `sotd` rooted at this env's project/state/config —
    /// `sot_log::state_dir::sot_state_dir()` reads `%LOCALAPPDATA%`
    /// directly (no daemon CLI flag exists for it — ADR 0042 L1a's own
    /// "no --data-dir flag" finding), and `workspaces.rs`'s own registry
    /// root reads `%XDG_CONFIG_HOME%` — both overridden here so this
    /// process's capsule state and workspace registry both live under the
    /// SAME temp root a second `sotd` launch (the resume-adopt leg of this
    /// test) can point at again.
    fn spawn_sotd(&self) -> KillGuard {
        let child = Command::new(sotd_exe())
            .arg("--socket")
            .arg(&self.socket_path)
            .arg("--project-root")
            .arg(&self.project_root)
            .env("LOCALAPPDATA", &self.state_root)
            .env("XDG_CONFIG_HOME", &self.config_root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sotd");
        KillGuard(Some(child))
    }
}

async fn try_connect(socket_path: &Path) -> Option<LocalStream> {
    let name = socket_path
        .to_str()
        .expect("socket path is valid UTF-8")
        .to_fs_name::<GenericFilePath>()
        .expect("interpret socket path as a local-socket name");
    LocalStream::connect(name).await.ok()
}

/// This test never reads and writes concurrently (a strict request/reply
/// protocol, one in flight at a time), so ONE handle serves both
/// directions: `tokio::io::BufReader<T>` forwards `AsyncWrite` straight
/// through when `T` implements it, buffering only the read side — no
/// split needed (`LocalStream` has no owned `into_split`, only a
/// borrowing `split()` that would tie the two halves' lifetimes together
/// for no benefit here).
type Conn = tokio::io::BufReader<LocalStream>;

/// One request/reply round trip: write `payload` under `op`, then read
/// frames until one with the matching `id` arrives (any `Kind::Evt`
/// broadcast in between — e.g. `workspace.created` — is skipped, exactly
/// as a real client's steady-state loop routes it aside).
async fn call(conn: &mut Conn, id: u64, op: &str, payload: serde_json::Value) -> Frame {
    codec::write_frame(conn, &Frame::req(id, op, payload), None)
        .await
        .expect("write_frame");
    loop {
        let (frame, _blob) = codec::read_frame(conn).await.expect("read_frame");
        if frame.id == id && frame.kind != Kind::Evt {
            return frame;
        }
    }
}

/// Connect (bounded-retried — the pipe takes a moment to bind after
/// `spawn_sotd`) + hello, returning the connection and the next free
/// request id.
async fn connect_and_hello(socket_path: &Path) -> (Conn, u64) {
    let stream = poll_until(
        || async { try_connect(socket_path).await },
        Duration::from_secs(30),
        "sotd's local socket to accept a connection",
    )
    .await;
    let mut conn = tokio::io::BufReader::new(stream);
    let hello = HelloReq {
        client_id: "capsule-workspaces-test".to_string(),
        session_id: None,
        last_seen_revision: 0,
        token: None,
        protocol: sot_protocol::PROTOCOL_VERSION,
        app_version: sot_protocol::app_version(),
    };
    let reply = call(&mut conn, 1, op::HELLO, serde_json::to_value(&hello).unwrap()).await;
    assert!(reply.payload.get("error").is_none(), "hello refused: {:?}", reply.payload);
    (conn, 2)
}

fn find_row(payload: &serde_json::Value, workspace_id: &str) -> Option<serde_json::Value> {
    payload["workspaces"].as_array()?.iter().find(|w| w["workspace_id"] == workspace_id).cloned()
}

#[tokio::test]
async fn capsule_workspace_create_list_attach_refusal_resume_and_destroy() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "sot-capsule.exe not found next to sotd.exe at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd.exe was built into",
        sot_capsule_exe()
    );

    let env = Env::new("cwl");
    let mut daemon = env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    // workspace.create — no autostart requested, so the capsule's own
    // producer is `agent_argv("none")` == `cmd.exe` (ADR 0042 L1a's own
    // fallback; also exactly "cmd.exe as the agent" per this test's spec).
    let create_req = serde_json::json!({
        "label": "cwl-workspace",
        "project_root": env.project_root.to_string_lossy(),
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let target = create_res.payload["tmux_session"]
        .as_str()
        .expect("tmux_session (the pty.open addressing token)")
        .to_string();

    // workspace.list: runtime "capsule", a state_dir, and — polled — phase
    // reaching "ready" (the capsule's cmd.exe leg coming up and the
    // supervisor's own lane answering `status`). A plain manual loop, not
    // the generic `poll_until` combinator: each iteration needs a fresh
    // mutable re-borrow of `rx`/`tx`/`next_id` across an `.await`, which a
    // `FnMut() -> impl Future` closure cannot express without collecting
    // (and re-losing) ownership every call.
    let list_deadline = Instant::now() + Duration::from_secs(90);
    let state_dir = loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, &workspace_id) {
            assert_eq!(row["runtime"], "capsule", "row: {row:?}");
            if let (Some(sd), Some("ready")) = (row["state_dir"].as_str(), row["phase"].as_str()) {
                break sd.to_string();
            }
        }
        assert!(
            Instant::now() < list_deadline,
            "timed out waiting for workspace.list to report phase \"ready\" for the new capsule workspace"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert!(Path::new(&state_dir).is_dir(), "reported state_dir does not exist on disk: {state_dir}");

    // pty.open on a capsule workspace: refused, never proxied.
    let pty_req = serde_json::json!({ "cols": 80, "rows": 24, "target": target, "user_switch": true });
    let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    // `next_id` has no further use in this half of the connection (the
    // daemon restart below opens a fresh connection with its own id
    // sequence), so no further increment here.
    assert_eq!(pty_res.payload["code"], "attach_direct", "pty.open payload: {:?}", pty_res.payload);
    assert_eq!(
        pty_res.payload["state_dir"].as_str(),
        Some(state_dir.as_str()),
        "pty.open's attach_direct state_dir should match workspace.list's"
    );

    // Kill the DAEMON (never the detached supervisor) and start a fresh one
    // on the same roots — the supervisor must have survived and the new
    // daemon must adopt it.
    daemon.kill_and_wait();
    drop(conn);
    let mut daemon2 = env.spawn_sotd();
    let (mut conn2, mut next_id2) = connect_and_hello(&env.socket_path).await;
    let resume_deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let id = next_id2;
        next_id2 += 1;
        let payload = call(&mut conn2, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, &workspace_id) {
            assert_eq!(row["runtime"], "capsule", "row: {row:?}");
            assert_eq!(
                row["state_dir"].as_str(),
                Some(state_dir.as_str()),
                "adopted row's state_dir must be the SAME capsule"
            );
            if row["phase"].as_str() == Some("ready") {
                break;
            }
        }
        assert!(
            Instant::now() < resume_deadline,
            "timed out waiting for the resumed daemon's workspace.list to re-adopt the capsule at phase \"ready\""
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // workspace.destroy: ends the run; the workspace disappears from the
    // list; the state dir is NOT deleted (the record persists by design).
    let destroy_req = serde_json::json!({ "workspace_id": workspace_id });
    let destroy_res = call(&mut conn2, next_id2, op::WORKSPACE_DESTROY, destroy_req).await;
    next_id2 += 1;
    assert!(destroy_res.payload.get("error").is_none(), "workspace.destroy failed: {:?}", destroy_res.payload);
    let destroy_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let id = next_id2;
        next_id2 += 1;
        let payload = call(&mut conn2, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if find_row(&payload, &workspace_id).is_none() {
            break;
        }
        assert!(
            Instant::now() < destroy_deadline,
            "timed out waiting for the destroyed workspace to disappear from workspace.list"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        Path::new(&state_dir).is_dir(),
        "the capsule's state dir must survive workspace.destroy (the record persists by design): {state_dir}"
    );

    daemon2.kill_and_wait();
}
