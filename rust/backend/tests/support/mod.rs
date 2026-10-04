//! Shared test support for this crate's real-process integration suites
//! (`capsule_workspaces/main.rs`, `lane_bridge/main.rs`): a real `sotd`, a real
//! `sot-capsule[.exe]` it spawns DETACHED, talking the actual wire
//! protocol over a real local socket. Lifted out of `capsule_workspaces/main.rs`
//! verbatim (ADR 0045 lane B4b) so `lane_bridge/main.rs`'s own cross-
//! process proofs — an attach client reaching a capsule row THROUGH a
//! daemon in the middle, over a test-owned TCP\u{2192}Unix relay — can
//! stand up the identical `Env`/wire-protocol fixture without a second,
//! drifting copy. Not itself a `tests/*.rs` file (Cargo only auto-
//! discovers direct children of `tests/` as integration-test binaries,
//! never a file inside a subdirectory), so each of the two real binaries
//! declares `mod support;` and gets its own compiled copy — no linkage
//! between them, no shared process state.
//!
//! Every wait below is a BOUNDED poll or `tokio::time::timeout` for an
//! external, observable fact — never a sleep-and-hope, and never an
//! unbounded read/write/kill/wait (Codex review finding 13, carried over
//! from `capsule_workspaces/main.rs`'s own header).

use std::cell::RefCell;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use interprocess::local_socket::tokio::{prelude::*, Stream as LocalStream};
use interprocess::local_socket::GenericFilePath;
use sot_protocol::{codec, op, Frame, HelloReq, Kind};

/// A4b: production's own aim rule, one source — [`arm_scope_guard`]
/// refuses exactly what `rows::spawn::row_scope` refuses.
#[cfg(target_os = "linux")]
#[path = "../../src/rows/spawn/row_scope_aim.rs"]
pub mod row_scope_aim;

/// The daemon's comm-registry poller (`comm/registry/poll.rs`, the ADE state-nav live
/// refresh) reads `<comm home>/registry.json` every 1.5s and broadcasts a
/// `workspace.changed` evt on any change; `paths::sot_comm_home` resolves
/// SOT_COMM_HOME, then HOME/.sot-comm, then USERPROFILE in that order. A
/// test daemon that leaves all three inherited therefore polls the
/// developer's REAL comm registry, and any live session on the box
/// stamping its work state delivers an unexpected evt into a test that
/// assumed a quiet connection — a wrong-result defect (it fails exactly
/// when the machine is busy), not flakiness. Every real-`sotd`-spawning
/// fixture in this crate creates its own home/comm dirs under its own
/// tempdir and pins all three vars on the child, one implementation
/// shared here rather than a copy per file.
pub fn comm_isolation_dirs(tmp: &Path) -> (PathBuf, PathBuf) {
    let home_root = tmp.join("home");
    std::fs::create_dir_all(&home_root).expect("mkdir home_root");
    let comm_root = home_root.join(".sot-comm");
    std::fs::create_dir_all(&comm_root).expect("mkdir comm_root");
    (home_root, comm_root)
}

/// Every bounded wait in this file shares one figure — generous over any
/// single supervisor-lane round trip (connect 2s + hello 2s + status 5s
/// ~= 9s worst case) but still a real bound, never "forever."
pub const BOUND: Duration = Duration::from_secs(30);

/// Pinned `SOT_SELF_HOST` for every `spawn_sotd` in this file — a fixed,
/// known per-host registry dir name instead of whatever hostname the
/// runner has (`host_name()`'s fallback).
/// `Env::seed_default_capsule_toml` computes the same path from it.
pub const TEST_STATE_HOST: &str = "testhost";
pub fn sotd_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sotd"))
}
/// Copy of `rust/log/tests/fe_client/`'s own `wake_flag` helper (a separate test
/// binary; not worth a shared dependency for four lines).
#[allow(dead_code)]
pub fn wake_flag_for_test() -> (std::sync::Arc<std::sync::atomic::AtomicBool>, Box<dyn Fn() + Send + 'static>) {
    let woke = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let woke2 = std::sync::Arc::clone(&woke);
    (woke, Box::new(move || woke2.store(true, std::sync::atomic::Ordering::Relaxed)))
}
/// The capsule executable's own file name for this platform — mirrors
/// `rows::spawn::detach`'s `CAPSULE_SIBLING_NAME`.
#[cfg(windows)]
pub const CAPSULE_EXE_NAME: &str = "sot-capsule.exe";
/// macOS lane: `not(windows)`, matching `rows::spawn::detach`'s
/// `CAPSULE_SIBLING_NAME` — the extensionless name is a Unix fact, and a
/// Linux-only gate here failed the whole `--tests` build on macOS.
#[cfg(not(windows))]
// Dead on a host with no capsule runtime in the daemon yet (macOS, until
// the capsule runtime's gate widens): every consumer of this
// name is itself gated to the platforms that have one.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
pub const CAPSULE_EXE_NAME: &str = "sot-capsule";
/// Resolved the same way production does — `current_exe().parent()` — but
/// from the TEST binary's own known sibling (`sotd[.exe]`'s own directory),
/// since a `tests/*.rs` binary itself lives in `target/<profile>/deps/`,
/// not `target/<profile>/`.
pub fn sot_capsule_exe() -> PathBuf {
    sotd_exe().with_file_name(CAPSULE_EXE_NAME)
}
/// This test file's own daemon-wire socket for `tag` — a named pipe on
/// Windows, a plain filesystem path on Linux (`interprocess::local_socket`'s
/// `GenericFilePath` name kind treats either shape as "just a path" — see
/// its own use in `try_connect` below). Unique per test process (its pid)
/// so a re-run never collides with a still-tearing-down prior instance.
/// This is a SEPARATE socket from every real capsule supervisor/voyage
/// lane the daemon itself spawns (those live under `SOT_RUNTIME_DIR`,
/// ADR 0043 decision 1) — this one is only the test-as-client's own
/// connection to `sotd`'s wire protocol. On Linux the daemon's own
/// `--socket` startup check refuses a group/other-accessible PARENT
/// directory (`secure socket dir ... mode ... is group/other-accessible`)
/// — plain `/tmp` fails that outright — so `runtime_dir` (the SAME
/// private, owner-only dir `SOT_RUNTIME_DIR` already points at) hosts
/// this socket too, under a name that cannot collide with a real
/// supervisor/voyage socket there.
#[cfg(windows)]
fn test_socket_path(_runtime_dir: &Path, tag: &str) -> PathBuf {
    PathBuf::from(format!(r"\\.\pipe\sot-test-{tag}-{}", std::process::id()))
}
/// macOS lane: `unix`, not Linux — a filesystem socket under the test's
/// own runtime dir is portable to every Unix, and nothing in this path
/// arithmetic is Linux-specific.
#[cfg(unix)]
fn test_socket_path(runtime_dir: &Path, tag: &str) -> PathBuf {
    runtime_dir.join(format!("wire-{tag}-{}.sock", std::process::id()))
}
/// Kill + wait a child with a real bound (Codex review finding 13: "the
/// test reuses ... unbounded ... kill waits"). Runs the blocking
/// kill+wait on a `spawn_blocking` thread so the bound is a real
/// `tokio::time::timeout`, not merely a hope that `wait()` returns fast
/// after `kill()`. This is the TEST's own deliberate, asserted teardown
/// of the daemon it owns (`Env::kill_daemon_bounded`); `Env`'s own `Drop`
/// (F4, LU4 review round 2) stays a best-effort, unbounded-but-brief
/// safety net for the panic/early-return paths a bounded async call
/// cannot run from — mirroring `supervisor_win.rs`'s own `KillGuard`,
/// which this file's `Env` now subsumes (the daemon `Child` moved from a
/// separate guard into `Env` itself so its `Drop` can order the daemon
/// kill before the leg sweep and the tmux teardown, F4's own ordering
/// requirement).
pub async fn kill_and_wait_bounded(child: Child) {
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
pub async fn poll_until<T, F, Fut>(mut attempt: F, timeout: Duration, what: &str) -> T
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
mod env;
mod procs;
pub(crate) use self::env::*;
pub(crate) use self::procs::*;
pub async fn try_connect(socket_path: &Path) -> Option<LocalStream> {
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
pub type Conn = tokio::io::BufReader<LocalStream>;
/// One request/reply round trip, itself bounded (Codex review finding
/// 13): write `payload` under `op`, then read frames until one with the
/// matching `id` arrives (any `Kind::Evt` broadcast in between — e.g.
/// `workspace.created` — is skipped, exactly as a real client's
/// steady-state loop routes it aside), all within `BOUND`.
pub async fn call(conn: &mut Conn, id: u64, op: &str, payload: serde_json::Value) -> Frame {
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
pub async fn connect_and_hello(socket_path: &Path) -> (Conn, u64) {
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
        host: None,
        role: String::new(),
        instance: None,
        name: None,
    };
    let reply = call(&mut conn, 1, op::HELLO, serde_json::to_value(&hello).unwrap()).await;
    assert!(reply.payload.get("error").is_none(), "hello refused: {:?}", reply.payload);
    (conn, 2)
}
pub fn find_row(payload: &serde_json::Value, workspace_id: &str) -> Option<serde_json::Value> {
    payload["workspaces"].as_array()?.iter().find(|w| w["workspace_id"] == workspace_id).cloned()
}
/// One bounded `query_status` attempt — `Ok` when the lane answered,
/// `None` (not an error) when it is legitimately absent/unreachable,
/// which two of this test's own polls treat as the fact they're waiting
/// for (the old supervisor going away after `stop`).
pub async fn try_query_status(state_dir: PathBuf) -> Option<sot_log::attach_client::supervisor_client::StatusReport> {
    tokio::task::spawn_blocking(move || {
        sot_log::attach_client::supervisor_client::query_status(&state_dir)
            .ok()
            .map(|(report, _process)| report)
    })
    .await
    .unwrap_or(None)
}
/// Real-supervisor preamble shared by both lane-refusal tests below:
/// create a capsule workspace, wait for a REAL supervisor (this
/// checkout's own build) to reach "ready", then stop JUST the authority
/// (the leg survives, ADR 0041 Lifecycle) so the state dir carries a
/// published pointer with nothing currently answering its socket —
/// exactly the precondition [`spawn_lane_refusal_fixture`]'s caller needs
/// before binding in the real supervisor's place.
#[cfg(target_os = "linux")]
pub async fn create_ready_workspace_then_stop_its_supervisor(
    env: &Env,
    conn: &mut Conn,
    next_id: &mut u64,
    label: &str,
) -> (String, PathBuf) {
    let create_req = serde_json::json!({
        "label": label,
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(conn, *next_id, op::WORKSPACE_CREATE, create_req).await;
    *next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();

    let list_deadline = Instant::now() + BOUND.max(Duration::from_secs(90));
    let state_dir = loop {
        let id = *next_id;
        *next_id += 1;
        let payload = call(conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
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

    tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::attach_client::supervisor_client::stop(&dir).expect("stop the real supervisor authority")
    })
    .await
    .unwrap();
    poll_until(
        || {
            let dir = state_dir_path.clone();
            async move {
                if try_query_status(dir).await.is_none() {
                    Some(())
                } else {
                    None
                }
            }
        },
        BOUND,
        "the stopped supervisor's own lane to go silent",
    )
    .await;

    (workspace_id, state_dir_path)
}
/// Poll `workspace.list` until `workspace_id`'s row reaches `want_phase`,
/// asserting it never reports `"terminal"` along the way (a competing
/// spawn racing the still-held fence would be the WRONG way to reach
/// this test's own target phase).
pub async fn poll_for_phase(conn: &mut Conn, next_id: &mut u64, workspace_id: &str, want_phase: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let id = *next_id;
        *next_id += 1;
        let payload = call(conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, workspace_id) {
            assert_eq!(row["runtime"], "capsule", "row: {row:?}");
            assert_ne!(row["phase"].as_str(), Some("terminal"), "row went terminal instead of {want_phase}: {row:?}");
            if row["phase"].as_str() == Some(want_phase) {
                return;
            }
        }
        assert!(Instant::now() < deadline, "timed out waiting for workspace.list to report phase \"{want_phase}\"");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
