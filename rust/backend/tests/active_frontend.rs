#![cfg(any(windows, target_os = "linux"))]
//! "Active frontend" (2026-09-08 review rework): server-level regression
//! tests through the REAL wire protocol against a real `sotd`, mirroring
//! `switch_latency/main.rs`'s own real-process posture (no protocol doubles, no
//! mocked `handle_connection`). Gated off macOS like that file and
//! `capsule_workspaces/main.rs`: the daemon's default-row boot path shells out to
//! a real `tmux` server on Linux unless `SOT_TMUX_SOCK` isolates it, and
//! macOS CI runners don't ship `tmux` by default.
//!
//! Two properties the unit tests in `clients.rs` can't reach
//! because they never go through `server/dispatch.rs`'s real dispatch loop:
//!
//! 1. Ordinary navigation/typing ops (`tree.root`, `preview.get`,
//!    `workspace.activate{read:true}`, `pty.write`) — EVERY op an earlier
//!    design stamped presence from — leave a connection's activity
//!    untouched; only `fe.presence` does.
//! 2. Two connections sharing a declared `name` (a stale reconnect, or a
//!    genuine hostname collision) never both receive an untargeted
//!    `fe.command.send` — exactly one does, by connection identity.
//!
//! `mod support;` serves `comm_isolation_dirs`, `sotd_command` and `try_connect` —
//! this file's own `Env` stays local (a separate, lighter fixture than
//! `support::Env`'s heavier capsule-process one).

mod support;

use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::Duration;

use interprocess::local_socket::tokio::Stream as LocalStream;
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
/// `switch_latency/main.rs`'s own `Env::spawn`, trimmed to what this file needs
/// (no slow-op knob).
struct Env {
    _tmp: tempfile::TempDir,
    _runtime_tmp: tempfile::TempDir,
    socket_path: PathBuf,
    daemon: Child,
}

impl Env {
    fn spawn(tag: &str) -> Self {
        Self::spawn_with(tag, &[])
    }

    /// `spawn`, with `vars` set in the daemon's environment as well.
    fn spawn_with(tag: &str, vars: &[(&str, &std::path::Path)]) -> Self {
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
        let (home_root, comm_root) = support::comm_isolation_dirs(tmp.path());

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
        let mut command = support::sotd_command();
        command.envs(vars.iter().copied());
        let daemon = command
            .arg("--socket")
            .arg(&socket_path)
            .arg("--project-root")
            .arg(&project_root)
            .env("LOCALAPPDATA", &state_root)
            .env("XDG_STATE_HOME", &state_root)
            .env("XDG_CONFIG_HOME", &config_root)
            .env("SOT_SELF_HOST", format!("activefe-{tag}"))
            .env("SOT_RUNTIME_DIR", runtime_tmp.path())
            .env("HOME", &home_root)
            .env("USERPROFILE", &home_root)
            .env("SOT_COMM_HOME", &comm_root)
            .stdin(Stdio::null())
            .spawn()
            .expect("spawn sotd");

        Self {
            _tmp: tmp,
            _runtime_tmp: runtime_tmp,
            socket_path,
            daemon,
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        support::end_daemon(&mut self.daemon);
    }
}

type Conn = tokio::io::BufReader<LocalStream>;

async fn poll_until_connected(socket_path: &std::path::Path) -> Conn {
    let deadline = std::time::Instant::now() + BOUND;
    loop {
        if let Some(s) = support::try_connect(socket_path).await {
            return tokio::io::BufReader::new(s);
        }
        assert!(std::time::Instant::now() < deadline, "sotd's socket never accepted a connection");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Connect + hello (declaring `{host, role, instance, name}` per ADR 0046
/// decision 1; `name` is the frontend's `fe@<host>` address, and `instance` is
/// `client_id`'s own, so each `client_id` is one window and a second connect
/// with it is that window's redial), returning the connection and the next
/// free request id (2, since id 1 is hello).
async fn connect_and_hello(socket_path: &std::path::Path, client_id: &str, name: &str) -> (Conn, u64) {
    connect_as(socket_path, client_id, "fe", name).await
}

/// `connect_and_hello` declaring `role`: a `"cli"` connection is never a window a command goes to.
async fn connect_as(socket_path: &std::path::Path, client_id: &str, role: &str, name: &str) -> (Conn, u64) {
    let mut conn = poll_until_connected(socket_path).await;
    let hello = HelloReq {
        instance: Some(format!("instance-{client_id}")),
        name: Some(name.to_string()),
        ..HelloReq::this_process(client_id, role, Some("test-host".to_string())).expect("this process's account")
    };
    codec::write_frame(&mut conn, &Frame::req(1, op::HELLO, serde_json::to_value(&hello).unwrap()), None)
        .await
        .expect("write hello");
    // The single next frame is not necessarily the hello reply — a broadcast
    // evt can legitimately land first (`call`'s own doc, just below) — so
    // skip anything that is not hello's own `res`, same as `call` does.
    let body = async {
        loop {
            let (frame, _blob) = codec::read_frame(&mut conn).await.expect("read hello reply");
            if frame.kind == Kind::Res && frame.id == 1 {
                return frame;
            }
        }
    };
    let frame = tokio::time::timeout(BOUND, body)
        .await
        .unwrap_or_else(|_| panic!("no reply to hello within {BOUND:?}"));
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
    let (mut conn, mut id) = connect_and_hello(&env.socket_path, "presence-test", "fe@host-nav").await;

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

/// ADR 0046 decision 1: a real hello declaring `{host, role, name}` is
/// echoed VERBATIM by `version.query`'s roster — read from the
/// connection's own declaration, never recomputed daemon-side.
#[tokio::test]
async fn version_query_echoes_the_declared_host_and_role() {
    let env = Env::spawn("declare");
    let (mut conn, id) = connect_and_hello(&env.socket_path, "declare-test", "fe-declare-test").await;

    let body = async {
        let v = call(&mut conn, id, op::VERSION_QUERY, serde_json::json!({})).await;
        let row = client_row(&v, "declare-test").expect("this connection's own roster row");
        assert_eq!(row.get("host").and_then(|v| v.as_str()), Some("test-host"), "{v}");
        assert_eq!(row.get("role").and_then(|v| v.as_str()), Some("fe"), "{v}");
        assert_eq!(row.get("name").and_then(|v| v.as_str()), Some("fe-declare-test"), "{v}");
        assert!(row.get("fe_handle").is_none(), "fe_handle left the wire at protocol 2: {v}");
        assert_eq!(row.get("instance").and_then(|v| v.as_str()), Some("instance-declare-test"), "{v}");
    };
    tokio::time::timeout(BOUND, body).await.expect("exchange did not finish within BOUND");
}

/// Finding 5 (2026-09-08 review): two connections sharing one `name`
/// (a stale reconnect, or a genuine hostname collision) must never both
/// receive an untargeted `fe.command.send` — resolution is by connection
/// SERIAL, and delivery is filtered server-side before the frame is ever
/// written, so the non-winning connection's request log (here: its own read
/// side) sees nothing at all.
#[tokio::test]
async fn duplicate_handle_connections_get_exactly_one_delivery() {
    let env = Env::spawn("dup");
    let (mut conn_a, mut id_a) = connect_and_hello(&env.socket_path, "dup-a", "fe@host-dup").await;
    let (mut conn_b, _id_b) = connect_and_hello(&env.socket_path, "dup-b", "fe@host-dup").await;

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
            Some("fe@host-dup"),
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

/// A `julia` stand-in speaking the REPL shim's line protocol: every eval announces one page for every frontend, as
/// `wglshow(fig)` does, and ends.
#[cfg(unix)]
const PAGE_JULIA: &str = r#"#!/bin/sh
while read -r line; do
  id=$(printf '%s' "$line" | sed -e 's/.*"id":\([0-9]*\).*/\1/')
  e=$(printf '%s' "$line" | sed -e 's/.*"eval_id":\([0-9]*\).*/\1/')
  printf '{"v":1,"id":0,"kind":"evt","op":"repl.frame","payload":{"eval_id":%s,"frame":{"kind":"browser","url":"http://127.0.0.1:9/page","open":true}}}\n' "$e"
  printf '{"v":1,"id":0,"kind":"evt","op":"repl.frame","payload":{"eval_id":%s,"frame":{"kind":"done"}}}\n' "$e"
  printf '{"v":1,"id":%s,"kind":"res","op":"repl.eval","payload":{}}\n' "$id"
done
"#;

/// The next `browser` frame `conn` is sent, with the evt's own payload (`eval_id`, `workspace_id`, `frame`).
#[cfg(unix)]
async fn next_browser_payload(conn: &mut Conn) -> serde_json::Value {
    let body = async {
        loop {
            let (frame, _blob) = codec::read_frame(conn).await.expect("read a repl.frame");
            if frame.kind == Kind::Evt && frame.op == op::REPL_FRAME && frame.payload["frame"]["kind"] == "browser" {
                return frame.payload;
            }
        }
    };
    tokio::time::timeout(BOUND, body).await.expect("no browser frame within BOUND")
}

/// The next `browser` frame `conn` is sent, as its `repl.frame` evt carries it.
#[cfg(unix)]
async fn next_browser_frame(conn: &mut Conn) -> serde_json::Value {
    let body = async {
        loop {
            let (frame, _blob) = codec::read_frame(conn).await.expect("read a repl.frame");
            if frame.kind == Kind::Evt && frame.op == op::REPL_FRAME && frame.payload["frame"]["kind"] == "browser" {
                return frame.payload["frame"].clone();
            }
        }
    };
    tokio::time::timeout(BOUND, body).await.expect("no browser frame within BOUND")
}

/// Two windows on one daemon: `fe@host-a` shows the daemon's default row (whose REPL the evals below run in) and
/// `fe@host-b` another; each sends `fe.presence`, `fe@host-b` last, so `fe@host-b` is the window in use. Returns both,
/// their next request ids and the default row's id.
async fn two_windows(env: &Env) -> ((Conn, u64), (Conn, u64), String) {
    let (mut conn_a, mut id_a) = connect_and_hello(&env.socket_path, "show-a", "fe@host-a").await;
    let (mut conn_b, mut id_b) = connect_and_hello(&env.socket_path, "show-b", "fe@host-b").await;
    let shown = call(&mut conn_a, id_a, op::WORKSPACE_ACTIVATE, serde_json::json!({"workspace_id": null})).await;
    let row = shown["workspace_id"].as_str().unwrap_or_else(|| panic!("the default row's id: {shown}")).to_string();
    let elsewhere = call(&mut conn_b, id_b, op::WORKSPACE_ACTIVATE, serde_json::json!({"workspace_id": "elsewhere"})).await;
    assert!(elsewhere.get("error").is_none(), "{elsewhere}");
    for (conn, id) in [(&mut conn_a, &mut id_a), (&mut conn_b, &mut id_b)] {
        *id += 1;
        assert_eq!(call(conn, *id, op::FE_PRESENCE, serde_json::json!({})).await["ok"], true);
        *id += 1;
    }
    ((conn_a, id_a), (conn_b, id_b), row)
}

/// A page an eval announces for every frontend (`wglshow(fig)`, `open: true`) opens in one window, never all: the one
/// that connected last while no window has been used, then the window in use (the latest input), over the window
/// showing the REPL's row. Every other window is sent it with `open: false`.
#[cfg(unix)]
#[tokio::test]
async fn an_announced_page_opens_in_one_window_the_one_in_use() {
    let stub = tempfile::tempdir().expect("tempdir");
    let julia = stub.path().join("julia");
    sot_platform::test_exec::write_executable(&julia, PAGE_JULIA);
    let resources = stub.path().join("resources");
    std::fs::create_dir_all(resources.join("julia").join("repl")).expect("mkdir resources");
    let env = Env::spawn_with("page", &[("SOT_JULIA_BIN", &julia), ("SOT_RESOURCE_ROOT", &resources)]);
    let (mut sender, mut id_s) = connect_as(&env.socket_path, "page-sender", "cli", "page-sender").await;

    let body = async {
        let (mut conn_a, _) = connect_and_hello(&env.socket_path, "fresh-a", "fe@host-a").await;
        let (mut conn_b, _) = connect_and_hello(&env.socket_path, "fresh-b", "fe@host-b").await;
        let ack = call(&mut sender, id_s, op::REPL_EVAL, serde_json::json!({"code": "wglshow(fig)", "eval_id": 1})).await;
        assert_eq!(ack["accepted"], true, "{ack}");
        id_s += 1;
        assert_eq!(next_browser_frame(&mut conn_a).await["open"], false, "never every window");
        assert_eq!(next_browser_frame(&mut conn_b).await["open"], true, "no window used: the one connected last");
        drop((conn_a, conn_b));

        let ((mut conn_a, _), (mut conn_b, _), _row) = two_windows(&env).await;
        let ack = call(&mut sender, id_s, op::REPL_EVAL, serde_json::json!({"code": "wglshow(fig)", "eval_id": 2})).await;
        assert_eq!(ack["accepted"], true, "{ack}");
        assert_eq!(next_browser_frame(&mut conn_a).await["open"], false, "the window on the REPL's row, used before");
        assert_eq!(next_browser_frame(&mut conn_b).await["open"], true, "the window in use opens it");
    };
    tokio::time::timeout(BOUND, body).await.expect("exchange did not finish within BOUND");
}

/// A page the window's own eval announces opens in that window, under the number the window gave the eval: the window's
/// own numbering (REPL-DRAWER) and the page's one opener (WGLSHOW-ONE-FE) compose. The run reaches the other window
/// under the daemon's number, with `open: false`.
#[cfg(unix)]
#[tokio::test]
async fn a_page_a_windows_own_eval_announces_opens_there_under_its_own_number() {
    let stub = tempfile::tempdir().expect("tempdir");
    let julia = stub.path().join("julia");
    sot_platform::test_exec::write_executable(&julia, PAGE_JULIA);
    let resources = stub.path().join("resources");
    std::fs::create_dir_all(resources.join("julia").join("repl")).expect("mkdir resources");
    let env = Env::spawn_with("ownpage", &[("SOT_JULIA_BIN", &julia), ("SOT_RESOURCE_ROOT", &resources)]);
    let body = async {
        let (mut conn_b, _) = connect_and_hello(&env.socket_path, "own-b", "fe@host-b").await;
        let (mut conn_a, id_a) = connect_and_hello(&env.socket_path, "own-a", "fe@host-a").await;
        fire(&mut conn_a, id_a, op::REPL_EVAL, serde_json::json!({"code": "wglshow(fig)", "eval_id": 7})).await;
        let own = next_browser_payload(&mut conn_a).await;
        assert_eq!(own["eval_id"], 7, "the window's own number: {own}");
        assert_eq!(own["frame"]["open"], true, "no window used: the one connected last, the sender: {own}");
        let other = next_browser_payload(&mut conn_b).await;
        assert!(other["eval_id"].as_u64().unwrap_or(0) >= 1 << 40, "the other window gets the daemon's number: {other}");
        assert_eq!(other["frame"]["open"], false, "never every window: {other}");
    };
    tokio::time::timeout(BOUND, body).await.expect("exchange did not finish within BOUND");
}

/// What a session puts in the preview pane (`sot-fe preview`, show-result) reaches every window, not only the window
/// showing its row or the window in use; with no window attached, the answer says none was reached.
#[tokio::test]
async fn a_preview_reaches_every_window() {
    let env = Env::spawn("preview");
    let (mut sender, mut id_s) = connect_as(&env.socket_path, "preview-sender", "cli", "preview-sender").await;
    let body = async {
        let preview =
            |row: &str| serde_json::json!({"cmd": "preview", "args": {"workspace": row, "path": "README.md"}});
        let ack = call(&mut sender, id_s, op::FE_COMMAND_SEND, preview("any")).await;
        id_s += 1;
        assert_eq!(ack["delivered_to"], 0, "no window attached: none reached: {ack}");

        let ((mut conn_a, _), (mut conn_b, _), row) = two_windows(&env).await;
        let ack = call(&mut sender, id_s, op::FE_COMMAND_SEND, preview(&row)).await;
        assert_eq!(ack["resolved_target"], serde_json::Value::Null, "no one window: {ack}");
        assert_eq!(ack["delivered_to"], 2, "every window: {ack}");
        for (conn, name) in [(&mut conn_a, "fe@host-a"), (&mut conn_b, "fe@host-b")] {
            let evt = next_fe_command(conn, BOUND).await.unwrap_or_else(|| panic!("{name} gets the preview"));
            assert_eq!(evt["cmd"], "preview", "{evt}");
            assert_eq!(evt["target"], serde_json::Value::Null, "{evt}");
        }
    };
    tokio::time::timeout(BOUND, body).await.expect("exchange did not finish within BOUND");
}

/// What opens the OS browser (`sot-fe open-url`, `sot-fe docs`) reaches one window, the window in use (the latest
/// input), over the window showing the session's row; a command that names no row goes to the window in use too. With
/// no window attached, the answer says none was reached.
#[tokio::test]
async fn a_browser_open_reaches_only_the_window_in_use() {
    let env = Env::spawn("open");
    let (mut sender, mut id_s) = connect_as(&env.socket_path, "open-sender", "cli", "open-sender").await;
    let body = async {
        let url = "http://127.0.0.1:9/p";
        let open = |row: &str| serde_json::json!({"cmd": "open_url", "args": {"url": url, "workspace": row}});
        let ack = call(&mut sender, id_s, op::FE_COMMAND_SEND, open("any")).await;
        id_s += 1;
        assert_eq!(ack["delivered_to"], 0, "no window attached: none reached: {ack}");

        let ((mut conn_a, _), (mut conn_b, _), row) = two_windows(&env).await;
        let docs = serde_json::json!({"cmd": "docs", "args": {"workspace": row, "path": "/x/index.html"}});
        for cmd in [open(&row), docs] {
            let ack = call(&mut sender, id_s, op::FE_COMMAND_SEND, cmd).await;
            id_s += 1;
            assert_eq!(ack["resolved_target"], "fe@host-b", "the window in use, not the one showing the row: {ack}");
            assert_eq!(ack["delivered_to"], 1, "{ack}");
            assert!(next_fe_command(&mut conn_b, BOUND).await.is_some(), "fe@host-b opens it");
            assert!(next_fe_command(&mut conn_a, ABSENCE_WAIT).await.is_none(), "fe@host-a opens nothing");
        }

        let notice = serde_json::json!({"cmd": "notify", "args": {"text": "hi"}});
        let ack = call(&mut sender, id_s, op::FE_COMMAND_SEND, notice).await;
        assert_eq!(ack["resolved_target"], "fe@host-b", "no row: the window in use: {ack}");
    };
    tokio::time::timeout(BOUND, body).await.expect("exchange did not finish within BOUND");
}

/// A window that redials is the same window (its hello's `instance`): the window in use before its link dropped is
/// still the one in use, with no new press; what goes to it goes to its new connection.
#[tokio::test]
async fn a_redialed_window_that_was_in_use_stays_in_use() {
    let env = Env::spawn("redial");
    let (mut sender, id_s) = connect_as(&env.socket_path, "redial-sender", "cli", "redial-sender").await;
    let body = async {
        let (mut b, id_b) = connect_and_hello(&env.socket_path, "redial-b", "fe@host-b").await;
        assert_eq!(call(&mut b, id_b, op::FE_PRESENCE, serde_json::json!({})).await["ok"], true);
        let (mut a, id_a) = connect_and_hello(&env.socket_path, "redial-a", "fe@host-a").await;
        assert_eq!(call(&mut a, id_a, op::FE_PRESENCE, serde_json::json!({})).await["ok"], true);
        drop(a); // the link drops and the window redials, with the same hello
        let (mut a, _) = connect_and_hello(&env.socket_path, "redial-a", "fe@host-a").await;
        let (mut roster, id_r) = connect_as(&env.socket_path, "redial-roster", "cli", "redial-roster").await;
        for id in id_r.. {
            let v = call(&mut roster, id, op::VERSION_QUERY, serde_json::json!({})).await;
            let rows = v["clients"].as_array().map_or(0, |c| c.iter().filter(|r| r["name"] == "fe@host-a").count());
            if rows == 1 {
                break; // the dropped connection has left the roster
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let notice = serde_json::json!({"cmd": "notify", "args": {"text": "hi"}});
        let ack = call(&mut sender, id_s, op::FE_COMMAND_SEND, notice).await;
        assert_eq!(ack["resolved_target"], "fe@host-a", "the window in use before its redial: {ack}");
        assert_eq!(next_fe_command(&mut a, BOUND).await.expect("its new connection gets it")["cmd"], "notify");
        assert!(next_fe_command(&mut b, ABSENCE_WAIT).await.is_none(), "fe@host-b gets nothing");
    };
    tokio::time::timeout(BOUND, body).await.expect("exchange did not finish within BOUND");
}

/// The next `fe.command` evt `conn` is sent within `wait`, or `None`.
async fn next_fe_command(conn: &mut Conn, wait: Duration) -> Option<serde_json::Value> {
    let body = async {
        loop {
            let (frame, _blob) = codec::read_frame(conn).await.expect("read a frame");
            if frame.kind == Kind::Evt && frame.op == op::FE_COMMAND {
                return frame.payload;
            }
        }
    };
    tokio::time::timeout(wait, body).await.ok()
}

/// The hello gate is integer equality on `protocol`, and its refusal
/// names BOTH sides: a frontend still on the previous protocol (a box
/// that has not converged) is turned away with the daemon's version and
/// its own in the payload, never registered, never silently degraded.
/// The frontend-side half of the same skew (a new frontend against an
/// old daemon) is `transport::protocol_mismatch_message`'s unit test.
#[tokio::test]
async fn hello_from_the_previous_protocol_is_refused_naming_both_versions() {
    let env = Env::spawn("proto-skew");
    let mut conn = poll_until_connected(&env.socket_path).await;
    let old = sot_protocol::PROTOCOL_VERSION - 1;
    let hello = HelloReq {
        protocol: old,
        app_version: "0.5.9".to_string(),
        name: Some("fe@test-host".to_string()),
        ..HelloReq::this_process("old-frontend", "fe", Some("test-host".to_string())).expect("this process's account")
    };
    let body = async {
        codec::write_frame(&mut conn, &Frame::req(1, op::HELLO, serde_json::to_value(&hello).unwrap()), None)
            .await
            .expect("write hello");
        // Same reasoning as `connect_and_hello`: skip any evt broadcast
        // that legitimately arrives before hello's own `res`.
        let frame = loop {
            let (frame, _blob) = codec::read_frame(&mut conn).await.expect("read hello reply");
            if frame.kind == Kind::Res && frame.id == 1 {
                break frame;
            }
        };
        let p = &frame.payload;
        assert_eq!(p.get("code").and_then(|v| v.as_str()), Some("protocol_mismatch"), "{p}");
        assert_eq!(p.get("backend_protocol").and_then(|v| v.as_u64()), Some(u64::from(sot_protocol::PROTOCOL_VERSION)), "{p}");
        assert_eq!(p.get("frontend_protocol").and_then(|v| v.as_u64()), Some(u64::from(old)), "{p}");
        assert_eq!(p.get("frontend_version").and_then(|v| v.as_str()), Some("0.5.9"), "{p}");
        let msg = p.get("error").and_then(|v| v.as_str()).unwrap_or("");
        assert!(msg.contains(&format!("protocol {}", sot_protocol::PROTOCOL_VERSION)), "{msg}");
        assert!(msg.contains(&format!("protocol {old}")), "{msg}");
        // Never registered: a second, well-formed connection's roster
        // does not list it.
        let (mut conn2, id) = connect_and_hello(&env.socket_path, "new-frontend", "fe@test-host").await;
        let v = call(&mut conn2, id, op::VERSION_QUERY, serde_json::json!({})).await;
        assert!(client_row(&v, "old-frontend").is_none(), "refused hello must not register: {v}");
    };
    tokio::time::timeout(BOUND, body).await.expect("exchange did not finish within BOUND");
}

/// A 64 MiB sparse file, larger than any socket or pipe buffer, so a download of it blocks on a
/// reader that stops reading.
fn big_sparse_file() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("big.bin");
    std::fs::File::create(&path).and_then(|f| f.set_len(64 << 20)).expect("sparse file");
    (dir, path)
}

/// Read up to request `id`'s first reply (skipping broadcast evts, as `call` does) and assert it
/// is a download chunk that is not the last.
async fn first_chunk_is_not_the_last(conn: &mut Conn, id: u64) {
    let body = async {
        loop {
            let (frame, _blob) = codec::read_frame(conn).await.expect("read first chunk");
            if frame.kind == Kind::Res && frame.id == id {
                return frame.payload;
            }
        }
    };
    let chunk = tokio::time::timeout(Duration::from_secs(5), body).await.expect("no first chunk within 5 s");
    assert_eq!(chunk.get("eof").and_then(|v| v.as_bool()), Some(false), "{chunk}");
}

/// Read `conn` until a read errors, within 5 s; no frame may carry `eof: true`.
async fn drain_until_closed_without_eof(conn: &mut Conn) {
    let body = async {
        loop {
            match codec::read_frame(conn).await {
                Ok((frame, _blob)) => assert_ne!(
                    frame.payload.get("eof").and_then(|v| v.as_bool()),
                    Some(true),
                    "the stalled reader got the whole file"
                ),
                Err(_) => return,
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), body)
        .await
        .expect("the stalled reader was still open 5 s after it left the roster");
}

/// ADR 0027's reaper covers downloads: a reader that stops reading mid-`file.download` is
/// dropped by the per-connection write deadline: the daemon drops the connection's task and
/// roster entry, so they are not held forever (on Linux the socket closes too, which the test
/// then reads).
#[tokio::test]
async fn a_stalled_download_is_closed_by_the_write_deadline() {
    let env = Env::spawn("stalled");
    let (_dir, file) = big_sparse_file();
    let body = async {
        let (mut witness, mut id) = connect_and_hello(&env.socket_path, "witness", "fe@host-witness").await;
        let (mut a, a_id) = connect_and_hello(&env.socket_path, "stall-a", "fe@host-stall").await;
        codec::write_frame(&mut a, &Frame::req(a_id, op::FILE_DOWNLOAD, serde_json::json!({ "path": file })), None)
            .await
            .expect("write file.download");
        first_chunk_is_not_the_last(&mut a, a_id).await;
        let deadline = std::time::Instant::now() + Duration::from_secs(40);
        loop {
            let v = call(&mut witness, id, op::VERSION_QUERY, serde_json::json!({})).await;
            id += 1;
            if client_row(&v, "stall-a").is_none() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "a stalled download was never closed by the write deadline"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        drain_until_closed_without_eof(&mut a).await;
    };
    tokio::time::timeout(Duration::from_secs(60), body).await.expect("exchange did not finish within 60 s");
}
