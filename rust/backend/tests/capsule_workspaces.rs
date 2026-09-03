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
//! Every wait below is a BOUNDED poll or `tokio::time::timeout` for an
//! external, observable fact (a named pipe accepting a connection, a
//! `workspace.list` row's own `phase` field, a supervisor lane going
//! silent) — never a sleep-and-hope, and never an unbounded read/write/
//! kill/wait (Codex review finding 13).
//!
//! Uses `sot_log::supervisor_client` directly (a real dependency of this
//! crate, not a test double) for two proofs the daemon's own wire
//! protocol has no op for: (1) `stop` ends JUST the supervisor authority
//! while its capsule leg survives (ADR 0041 Lifecycle — legs are
//! deliberately outside the supervisor's own job), which is how this
//! test proves ADOPTION (a fresh `--resume` finding the SAME leg still
//! alive, never bumping its epoch) rather than mere detachment (an
//! untouched, already-running supervisor merely surviving a daemon
//! restart); (2) `query_status` after `workspace.destroy` independently
//! confirms the record actually closed before this test ever asserts the
//! row is gone from `workspace.list`.

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
/// holds the guard across `.await` points for its whole body.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Every bounded wait in this file shares one figure — generous over any
/// single supervisor-lane round trip (connect 2s + hello 2s + status 5s
/// ~= 9s worst case) but still a real bound, never "forever."
const BOUND: Duration = Duration::from_secs(30);

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
/// `supervisor_win.rs`'s own `KillGuard` — `kill_and_wait_bounded` is the
/// TEST's own deliberate teardown (bounded, asserted); `Drop` stays a
/// best-effort, unbounded-but-brief safety net for the panic/early-return
/// paths a bounded async call cannot run from.
struct KillGuard(Option<Child>);
impl KillGuard {
    fn take(&mut self) -> Option<Child> {
        self.0.take()
    }
}
impl Drop for KillGuard {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Kill + wait a child with a real bound (Codex review finding 13: "the
/// test reuses ... unbounded ... kill waits"). Runs the blocking
/// kill+wait on a `spawn_blocking` thread so the bound is a real
/// `tokio::time::timeout`, not merely a hope that `wait()` returns fast
/// after `kill()`.
async fn kill_and_wait_bounded(child: Child) {
    let mut child = child;
    let res = tokio::time::timeout(
        BOUND,
        tokio::task::spawn_blocking(move || {
            let _ = child.kill();
            let _ = child.wait();
        }),
    )
    .await;
    assert!(res.is_ok(), "killing/waiting a spawned process exceeded {BOUND:?}");
}

/// Bounded async poll for an external, observable fact.
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

/// One isolated test environment. ADR 0042 L1a, Codex review finding 2:
/// the DAEMON's own `--project-root` (`daemon_project_root`) and the
/// workspace this test creates (`workspace_project_root`) are TWO
/// SEPARATE sibling directories — the daemon already registers its own
/// root as the default workspace at startup, so creating a second
/// workspace pointed at that SAME root trips the duplicate-root gate
/// (`code: "duplicate_root"`) before this test's own capsule logic is
/// ever exercised.
struct Env {
    _tmp: tempfile::TempDir,
    daemon_project_root: PathBuf,
    workspace_project_root: PathBuf,
    state_root: PathBuf,
    config_root: PathBuf,
    socket_path: PathBuf,
}

impl Env {
    fn new(tag: &str) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let daemon_project_root = tmp.path().join("daemon-project");
        std::fs::create_dir_all(&daemon_project_root).expect("mkdir daemon_project_root");
        let workspace_project_root = tmp.path().join("workspace-project");
        std::fs::create_dir_all(&workspace_project_root).expect("mkdir workspace_project_root");
        let state_root = tmp.path().join("state");
        std::fs::create_dir_all(&state_root).expect("mkdir state_root");
        let config_root = tmp.path().join("config");
        std::fs::create_dir_all(&config_root).expect("mkdir config_root");
        // A named pipe, not a filesystem path with real collision risk, but
        // still unique per test process so a re-run never collides with a
        // still-tearing-down prior instance.
        let socket_path = PathBuf::from(format!(r"\\.\pipe\sot-test-{tag}-{}", std::process::id()));
        Self {
            _tmp: tmp,
            daemon_project_root,
            workspace_project_root,
            state_root,
            config_root,
            socket_path,
        }
    }

    /// Spawn a real `sotd` rooted at this env's project/state/config —
    /// `sot_log::state_dir::sot_state_dir()` reads `%LOCALAPPDATA%`
    /// directly (no daemon CLI flag exists for it), and `workspaces.rs`'s
    /// own registry root reads `%XDG_CONFIG_HOME%` — both overridden here
    /// so this process's capsule state and workspace registry both live
    /// under the SAME temp root a second `sotd` launch (the adoption leg
    /// of this test) can point at again.
    fn spawn_sotd(&self) -> KillGuard {
        let child = Command::new(sotd_exe())
            .arg("--socket")
            .arg(&self.socket_path)
            .arg("--project-root")
            .arg(&self.daemon_project_root)
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
    // Bounded connect attempt (Codex review finding 13): a single try
    // never blocks past a small slice of the outer `poll_until` budget.
    tokio::time::timeout(Duration::from_secs(2), LocalStream::connect(name))
        .await
        .ok()
        .and_then(Result::ok)
}

type Conn = tokio::io::BufReader<LocalStream>;

/// One request/reply round trip, itself bounded (Codex review finding
/// 13): write `payload` under `op`, then read frames until one with the
/// matching `id` arrives (any `Kind::Evt` broadcast in between — e.g.
/// `workspace.created` — is skipped, exactly as a real client's
/// steady-state loop routes it aside), all within `BOUND`.
async fn call(conn: &mut Conn, id: u64, op: &str, payload: serde_json::Value) -> Frame {
    let body = async {
        codec::write_frame(conn, &Frame::req(id, op, payload), None)
            .await
            .expect("write_frame");
        loop {
            let (frame, _blob) = codec::read_frame(conn).await.expect("read_frame");
            if frame.id == id && frame.kind != Kind::Evt {
                return frame;
            }
        }
    };
    tokio::time::timeout(BOUND, body)
        .await
        .unwrap_or_else(|_| panic!("{op} (id {id}) did not reply within {BOUND:?}"))
}

/// Connect (bounded-retried — the pipe takes a moment to bind after
/// `spawn_sotd`) + hello, returning the connection and the next free
/// request id.
async fn connect_and_hello(socket_path: &Path) -> (Conn, u64) {
    let stream = poll_until(
        || async { try_connect(socket_path).await },
        BOUND,
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

/// One bounded `query_status` attempt — `Ok` when the lane answered,
/// `None` (not an error) when it is legitimately absent/unreachable,
/// which two of this test's own polls treat as the fact they're waiting
/// for (the old supervisor going away after `stop`).
async fn try_query_status(state_dir: PathBuf) -> Option<sot_log::supervisor_client::StatusReport> {
    tokio::task::spawn_blocking(move || sot_log::supervisor_client::query_status(&state_dir).ok())
        .await
        .unwrap_or(None)
}

#[tokio::test]
async fn capsule_workspace_create_list_attach_refusal_adopt_and_destroy() {
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

    // workspace.create — a SEPARATE project root from the daemon's own
    // default (finding 2). No autostart requested, so the capsule's own
    // producer is `agent_argv("none")` == `cmd.exe` (ADR 0042 L1a's own
    // fallback; also exactly "cmd.exe as the agent" per this test's spec).
    let create_req = serde_json::json!({
        "label": "cwl-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
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
    // supervisor's own lane answering `status`).
    let list_deadline = Instant::now() + BOUND.max(Duration::from_secs(90));
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
    let state_dir_path = PathBuf::from(&state_dir);
    assert!(state_dir_path.is_dir(), "reported state_dir does not exist on disk: {state_dir}");

    // pty.open on a capsule workspace: refused, never proxied.
    let pty_req = serde_json::json!({ "cols": 80, "rows": 24, "target": target, "user_switch": true });
    let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    // `next_id` has no further use on this connection (it is dropped and
    // replaced after the daemon restart below), so no further increment.
    assert_eq!(pty_res.payload["code"], "attach_direct", "pty.open payload: {:?}", pty_res.payload);
    assert_eq!(
        pty_res.payload["state_dir"].as_str(),
        Some(state_dir.as_str()),
        "pty.open's attach_direct state_dir should match workspace.list's"
    );

    // --- Adoption proof (Codex review finding 13) ---
    // Record the current leg epoch, then STOP just the supervisor
    // authority over its own lane (sot_log::supervisor_client::stop) --
    // its capsule leg is deliberately outside the supervisor's own job
    // (ADR 0041 Lifecycle) and survives. Poll until the OLD supervisor's
    // lane is confirmed gone, kill+restart the DAEMON, and poll the NEW
    // daemon's workspace.list back to "ready". If the leg epoch is
    // UNCHANGED, the new supervisor ADOPTED the still-alive leg rather
    // than spawning a fresh one -- proving adoption, not mere detachment
    // of an untouched, already-running process.
    let status_before = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::supervisor_client::query_status(&dir).expect("query_status before stop")
    })
    .await
    .unwrap();
    let leg_before = status_before.leg.expect("a ready capsule has a leg");

    tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::supervisor_client::stop(&dir).expect("stop the supervisor authority")
    })
    .await
    .unwrap();

    poll_until(
        || {
            let dir = state_dir_path.clone();
            async move { if try_query_status(dir).await.is_none() { Some(()) } else { None } }
        },
        BOUND,
        "the stopped supervisor's own lane to go silent",
    )
    .await;

    if let Some(child) = daemon.take() {
        kill_and_wait_bounded(child).await;
    }
    drop(conn);

    let mut daemon2 = env.spawn_sotd();
    let (mut conn2, mut next_id2) = connect_and_hello(&env.socket_path).await;
    let resume_deadline = Instant::now() + BOUND.max(Duration::from_secs(60));
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
    let status_after = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::supervisor_client::query_status(&dir).expect("query_status after adoption")
    })
    .await
    .unwrap();
    assert_eq!(
        status_after.leg,
        Some(leg_before),
        "the leg epoch changed across the daemon restart -- a fresh leg was SPAWNED, not adopted"
    );

    // --- Destroy proof (Codex review finding 13) ---
    // workspace.destroy: ends the run; independently confirm via the
    // lane itself (not just the daemon's own say-so) that the record
    // actually closed BEFORE asserting the row disappears from
    // workspace.list; the state dir is NOT deleted (the record persists
    // by design).
    let destroy_req = serde_json::json!({ "workspace_id": workspace_id });
    let destroy_res = call(&mut conn2, next_id2, op::WORKSPACE_DESTROY, destroy_req).await;
    next_id2 += 1;
    assert!(destroy_res.payload.get("error").is_none(), "workspace.destroy failed: {:?}", destroy_res.payload);

    let ended = poll_until(
        || {
            let dir = state_dir_path.clone();
            async move {
                let report = try_query_status(dir).await?;
                (report.phase == sot_log::wire::SupervisorPhase::EndedNoRespawn).then_some(report)
            }
        },
        BOUND,
        "the supervisor's own lane to report phase EndedNoRespawn after workspace.destroy",
    )
    .await;
    assert!(ended.voyage.is_some(), "an ended run still names its voyage");

    let destroy_deadline = Instant::now() + BOUND;
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
        state_dir_path.is_dir(),
        "the capsule's state dir must survive workspace.destroy (the record persists by design): {state_dir}"
    );

    if let Some(child) = daemon2.take() {
        kill_and_wait_bounded(child).await;
    }
}

/// Field finding (v0.6.0-rc.2 shakedown): the daemon's own default/home
/// workspace is registered with `runtime: "capsule"` at startup (ADR 0042
/// L1a, server.rs's default-workspace seed), but `workspace.create` was
/// the ONLY path that ever spawned a capsule's supervisor — this row was
/// never created through it, so it never got one. Selecting it in the
/// frontend sent `pty.open`, which answered `attach_direct` against a
/// supervisor that had never been started, parking the frontend on an
/// empty pane with no way to start the session. This proves the fix:
/// `pty.open` on this row now starts the supervisor itself (start-on-
/// attach, sharing `workspace.create`'s own spawn path) before ever
/// answering `attach_direct`.
#[tokio::test]
async fn capsule_default_workspace_starts_its_supervisor_on_first_attach() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "sot-capsule.exe not found next to sotd.exe at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd.exe was built into",
        sot_capsule_exe()
    );

    let env = Env::new("dsa");
    let mut daemon = env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    // Find the default row — never touched `workspace.create`, so its
    // supervisor has never been spawned.
    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id += 1;
    let default_row = list_payload["workspaces"]
        .as_array()
        .expect("workspaces array")
        .iter()
        .find(|w| w["is_default"].as_bool() == Some(true))
        .cloned()
        .expect("a default workspace row");
    assert_eq!(default_row["runtime"], "capsule", "default row: {default_row:?}");
    let default_workspace_id = default_row["workspace_id"].as_str().expect("workspace_id").to_string();
    let default_target = default_row["tmux_session"].as_str().expect("tmux_session").to_string();

    // Same path arithmetic `capsule_workspace::state_dir_for` uses:
    // `<LOCALAPPDATA>\sot\workspaces\<workspace_id>` — `env.state_root` IS
    // the LOCALAPPDATA value this daemon was launched with (see
    // `Env::spawn_sotd`).
    let state_dir_path = env.state_root.join("sot").join("workspaces").join(&default_workspace_id);

    // Rule H: prove the "never started" precondition BEFORE `pty.open` —
    // no state dir on disk at all, and `workspace.list`'s own row already
    // reads "stopped" from THIS first list call. Rule B guarantees this:
    // the startup resume-scan (`resume_all`) SKIPS every row with no
    // published voyage pointer, so it never touched this row — without
    // rule B this assertion would race the resume-scan and could flake.
    assert!(
        !state_dir_path.exists(),
        "the default capsule's state dir must not exist before its first attach: {state_dir_path:?}"
    );
    assert_eq!(
        default_row["phase"].as_str(),
        Some("stopped"),
        "the default row's phase must read \"stopped\" before its first attach: {default_row:?}"
    );

    // `target` MUST be the row's own `tmux_session` — a targetless
    // `pty.open` addresses the drawer's own special SoT LLM terminal
    // (`pty::DEFAULT_TMUX_TARGET` == "sot-llm"), never a workspace row;
    // `server.rs`'s `workspace_for_tmux(requested_target)` only resolves
    // to this row when `target` matches its `tmux_session`. This is
    // exactly what the frontend sends attaching a capsule row.
    let pty_req = serde_json::json!({
        "cols": 80, "rows": 24, "user_switch": true, "target": default_target,
    });
    let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    next_id += 1;
    assert_eq!(pty_res.payload["code"], "attach_direct", "pty.open payload: {:?}", pty_res.payload);
    let expected_state_dir = state_dir_path.to_string_lossy().into_owned();
    assert_eq!(
        pty_res.payload["state_dir"].as_str(),
        Some(expected_state_dir.as_str()),
        "pty.open's attach_direct state_dir should be the default workspace's own capsule state dir"
    );

    // The state dir appears on disk — start-on-attach actually spawned
    // something, not just answered a stale path.
    let dir_deadline = Instant::now() + BOUND;
    while !state_dir_path.is_dir() {
        assert!(
            Instant::now() < dir_deadline,
            "timed out waiting for the default capsule's state dir to appear: {state_dir_path:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // The lane answers — a real supervisor authority is listening, not
    // just an empty directory left behind by a partial spawn.
    poll_until(
        || {
            let dir = state_dir_path.clone();
            async move { try_query_status(dir).await }
        },
        BOUND,
        "the default capsule's supervisor lane to answer a status query",
    )
    .await;

    // workspace.list's own row leaves "stopped" for this workspace,
    // confirming the daemon's own reported phase agrees with the lane.
    let phase_deadline = Instant::now() + BOUND.max(Duration::from_secs(90));
    loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, &default_workspace_id) {
            assert_eq!(row["runtime"], "capsule", "row: {row:?}");
            if row["phase"].as_str() != Some("stopped") {
                break;
            }
        }
        assert!(
            Instant::now() < phase_deadline,
            "timed out waiting for workspace.list to report a phase other than \"stopped\" for the default capsule workspace"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Rule H: "KillGuard" the spawned supervisor — it is DETACHED
    // (spawned by the daemon, survives the daemon's own exit by design,
    // ADR 0042 L1a), so killing `daemon` below does NOT reap it and it
    // would otherwise leak past this test. There is no `std::process::
    // Child` for it here (the daemon owns the actual spawn), so this
    // stops it over its own lane instead — the same
    // `sot_log::supervisor_client::stop` the create-test's own adoption
    // proof uses. Best-effort: the lane may already be gone (a `claude`
    // producer missing from this runner's PATH would eventually make the
    // supervisor's own internal flap budget give up and exit on its
    // own, per rule F) — either way nothing is left running afterward.
    let _ = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::supervisor_client::stop(&dir)
    })
    .await;

    if let Some(child) = daemon.take() {
        kill_and_wait_bounded(child).await;
    }
}
