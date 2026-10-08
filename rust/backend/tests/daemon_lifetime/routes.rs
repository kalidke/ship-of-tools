//! The daemon's real routes the cases drive, and what they read back: a ready capsule row, a REPL cell that spins, the
//! identities of a tree a fixture started, the nonce round trip through a row's agent, and the observed relations of a
//! process (its children, its command line). Nothing here ends a process; ending is the fixture owner's.

use crate::fixture_owner::Fixture;
use crate::guard::parent_of;
use crate::support::{call, connect_and_hello, find_row, poll_until, Conn, Env, BOUND};
use crate::tree::Tree;
use sot_protocol::op;
use sot_protocol::{codec, Frame};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The row's supervisor as the product's lane reports it: its pid and creation time. The lane is found through `env`'s
/// runtime folder, which this call makes the process's own.
pub async fn supervisor_in(env: &Env, state_dir: &Path) -> Option<(i32, u64)> {
    std::env::set_var("SOT_RUNTIME_DIR", env._runtime_tmp.path());
    let dir = state_dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        sot_log::attach_client::supervisor_client::query_status(&dir)
            .ok()
            .map(|(report, _)| (report.pid as i32, report.created))
    })
    .await
    .unwrap()
}

/// A Julia the REPL can run: `SOT_JULIA_BIN` of this test's own environment, else `julia` found on its PATH.
pub fn julia_bin() -> String {
    if let Some(bin) = std::env::var_os("SOT_JULIA_BIN") {
        return bin.to_string_lossy().into_owned();
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("julia"))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("no julia: set SOT_JULIA_BIN or put julia on the PATH"))
        .to_string_lossy()
        .into_owned()
}

/// A capsule row, created through the daemon and waited to `ready`; its id and its state folder.
pub async fn ready_row(
    env: &Env,
    conn: &mut Conn,
    next_id: &mut u64,
    label: &str,
) -> (String, PathBuf) {
    let req = serde_json::json!({ "label": label, "project_root": env.workspace_project_root.to_string_lossy(), "runtime": "capsule" });
    let res = call(conn, *next_id, op::WORKSPACE_CREATE, req).await;
    *next_id += 1;
    assert!(
        res.payload.get("error").is_none(),
        "workspace.create failed: {:?}",
        res.payload
    );
    let id = res.payload["workspace_id"]
        .as_str()
        .expect("workspace_id")
        .to_string();
    let began = Instant::now();
    loop {
        let listed = call(conn, *next_id, op::WORKSPACE_LIST, serde_json::json!({}))
            .await
            .payload;
        *next_id += 1;
        if let Some(row) = find_row(&listed, &id).filter(|row| row["phase"] == "ready") {
            return (
                id,
                PathBuf::from(row["state_dir"].as_str().expect("the row's state folder")),
            );
        }
        assert!(
            began.elapsed() < Duration::from_secs(90),
            "the row {label} never reached ready"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Run a cell in the row's REPL on a connection of its own. The call does not answer while the cell spins, so it runs in a
/// task that the caller aborts; the reply, if one ever comes, is of no interest.
pub async fn spin_in_repl(
    socket: &Path,
    workspace_id: &str,
    cell: String,
) -> tokio::task::JoinHandle<()> {
    let (mut conn, next_id) = connect_and_hello(socket).await;
    let workspace_id = workspace_id.to_string();
    tokio::spawn(async move {
        let _ = call_long(
            &mut conn,
            next_id,
            op::REPL_EXECUTE,
            serde_json::json!({ "workspace_id": workspace_id, "input": { "kind": "eval", "code": cell }, "timeout_ms": 600_000 }),
            Duration::from_secs(3600),
        )
        .await;
    })
}

/// The identities of a tree once it has reported, each opened while it lives.
pub async fn watch_tree(fx: &mut Fixture, tree: &Tree, forking: bool, what: &str) -> Vec<usize> {
    let pids = poll_until(
        || async { tree.pids(forking) },
        Duration::from_secs(120),
        &format!("the {what} tree to report"),
    )
    .await;
    pids.into_iter()
        .map(|(name, pid)| {
            fx.watch(pid, None, &format!("{what} {name}"))
                .expect("an identity for a process the tree reported")
        })
        .collect()
}

/// Whether every identity has ended, within `bound`.
pub fn all_ended(fx: &Fixture, ids: &[usize], bound: Duration) -> bool {
    let deadline = Instant::now() + bound;
    ids.iter().all(|i| {
        fx.identity(*i)
            .exited(deadline.saturating_duration_since(Instant::now()))
    })
}

/// A fresh nonce typed into the row's agent through the daemon and read back from its screen: the answer is `"true ..."`
/// when it came back, with what `pty.input` said.
pub async fn nonce_round_trip(
    conn: &mut Conn,
    next_id: &mut u64,
    workspace_id: &str,
    origin: &str,
) -> String {
    use base64::Engine as _;
    static COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let nonce = format!(
        "l2-nonce-{}-{}",
        std::process::id(),
        COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    );
    let input = call(
        conn,
        *next_id,
        op::PTY_INPUT,
        serde_json::json!({
            "workspace_id": workspace_id,
            "data_b64": base64::engine::general_purpose::STANDARD.encode(format!("echo {nonce}")),
            "enter": true,
            "origin": origin,
        }),
    )
    .await;
    *next_id += 1;
    let deadline = Instant::now() + BOUND;
    let echoed = loop {
        let id = *next_id;
        *next_id += 1;
        let screen = call(
            conn,
            id,
            op::PTY_SCREEN,
            serde_json::json!({ "workspace_id": workspace_id }),
        )
        .await
        .payload;
        let seen = screen["lines"].as_array().is_some_and(|lines| {
            lines
                .iter()
                .any(|line| line.as_str().is_some_and(|l| l.trim_end() == nonce))
        });
        if seen || Instant::now() >= deadline {
            break seen;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    format!("{echoed} (pty.input answered {})", input.payload)
}

/// [`call`] with a bound of the caller's: the first start of Pluto or a Quarto render takes longer than `BOUND`.
pub async fn call_long(
    conn: &mut Conn,
    id: u64,
    op: &str,
    payload: serde_json::Value,
    bound: Duration,
) -> Frame {
    let body = async {
        codec::write_frame(conn, &Frame::req(id, op, payload), None)
            .await
            .expect("write_frame");
        loop {
            let (frame, _blob) = codec::read_frame(conn).await.expect("read_frame");
            if frame.id == id && frame.kind != sot_protocol::Kind::Evt {
                return frame;
            }
        }
    };
    tokio::time::timeout(bound, body)
        .await
        .unwrap_or_else(|_| panic!("{op} (id {id}) did not reply within {bound:?}"))
}

/// The pids whose parent is `pid`, from `/proc` (observed): the children list of a process the case holds.
pub fn children_of(pid: i32) -> Vec<i32> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.filter_map(|entry| {
        let child: i32 = entry.ok()?.file_name().to_str()?.parse().ok()?;
        (parent_of(child) == Some(pid)).then_some(child)
    })
    .collect()
}

/// A process's command line, words joined by spaces (observed, to tell a daemon's children apart).
pub fn command_line(pid: i32) -> String {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| String::from_utf8_lossy(&b).replace('\0', " "))
        .unwrap_or_default()
}
