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
//! 2. Two connections sharing a declared `name` (a stale reconnect, or a
//!    genuine hostname collision) never both receive an untargeted
//!    `fe.command.send` — exactly one does, by connection identity.
//!
//! `mod support;` is used for exactly one helper, `comm_isolation_dirs` —
//! this file's own `Env` stays local (a separate, lighter fixture than
//! `support::Env`'s heavier capsule-process one).

mod support;

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
        let daemon = Command::new(sotd_exe())
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
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
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

/// Connect + hello (declaring `{host, role, instance, name}` per ADR 0046
/// decision 1; `name` is the frontend's `fe@<host>` address), returning
/// the connection and the next free request id (2, since id 1 is hello).
async fn connect_and_hello(socket_path: &std::path::Path, client_id: &str, name: &str) -> (Conn, u64) {
    let mut conn = poll_until_connected(socket_path).await;
    let hello = HelloReq {
        client_id: client_id.to_string(),
        session_id: None,
        last_seen_revision: 0,
        token: None,
        protocol: sot_protocol::PROTOCOL_VERSION,
        app_version: sot_protocol::app_version(),
        host: Some("test-host".to_string()),
        role: "fe".to_string(),
        instance: Some("test-instance".to_string()),
        name: Some(name.to_string()),
        os_user: sot_log::os_account::own_account_id(),
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
        assert_eq!(row.get("instance").and_then(|v| v.as_str()), Some("test-instance"), "{v}");
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
        client_id: "old-frontend".to_string(),
        session_id: None,
        last_seen_revision: 0,
        token: None,
        protocol: old,
        app_version: "0.5.9".to_string(),
        host: Some("test-host".to_string()),
        role: "fe".to_string(),
        instance: None,
        name: Some("fe@test-host".to_string()),
        os_user: sot_log::os_account::own_account_id(),
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

/// A raw hello (a JSON payload, never the `HelloReq` struct) on a fresh
/// connection; returns the connection and the hello's `res` payload.
async fn raw_hello(socket_path: &std::path::Path, payload: serde_json::Value) -> (Conn, serde_json::Value) {
    let mut conn = poll_until_connected(socket_path).await;
    codec::write_frame(&mut conn, &Frame::req(1, op::HELLO, payload), None).await.expect("write hello");
    let body = async {
        loop {
            let (frame, _blob) = codec::read_frame(&mut conn).await.expect("read hello reply");
            if frame.kind == Kind::Res && frame.id == 1 {
                return frame.payload;
            }
        }
    };
    let payload = tokio::time::timeout(BOUND, body)
        .await
        .unwrap_or_else(|_| panic!("no reply to hello within {BOUND:?}"));
    (conn, payload)
}

/// A raw `fe` hello for `client_id` declaring `host`, with `os_user` when given.
fn account_hello(client_id: &str, host: &str, os_user: Option<&str>) -> serde_json::Value {
    let mut h = serde_json::json!({
        "client_id": client_id,
        "protocol": sot_protocol::PROTOCOL_VERSION,
        "app_version": sot_protocol::app_version(),
        "role": "fe",
        "host": host,
        "name": format!("fe@{client_id}"),
    });
    if let Some(u) = os_user {
        h["os_user"] = serde_json::json!(u);
    }
    h
}

/// The daemon closed `conn`: a read returns an error within 2 s.
async fn assert_closed(conn: &mut Conn) {
    let read = tokio::time::timeout(Duration::from_secs(2), codec::read_frame(conn))
        .await
        .expect("a refused hello's connection must be closed, but it stayed open");
    assert!(read.is_err(), "a refused hello's connection must be closed, got a frame");
}

/// A hello payload with `key` removed.
fn without(mut hello: serde_json::Value, key: &str) -> serde_json::Value {
    hello.as_object_mut().expect("hello is an object").remove(key);
    hello
}

fn code(payload: &serde_json::Value) -> Option<&str> {
    payload.get("code").and_then(|v| v.as_str())
}

/// The roster as `version.query` shows it, polled until `client_id` is gone (2 s).
async fn wait_until_gone(conn: &mut Conn, mut id: u64, client_id: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let v = call(conn, id, op::VERSION_QUERY, serde_json::json!({})).await;
        id += 1;
        if client_row(&v, client_id).is_none() {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "{client_id} never left the roster");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Decision 0031, the detector: the second OS account to say hello for a host is refused
/// (`os_user_conflict`) and closed, every live connection of that host is closed, and every later
/// hello for it, either account, is refused. Another host is served. A refused client is never listed.
#[tokio::test]
async fn a_second_account_closes_both_and_refuses_the_computer() {
    let env = Env::spawn("os-user");
    let body = async {
        let (mut a, pa) = raw_hello(&env.socket_path, account_hello("acct-a", "acct-box", Some("uid:900001"))).await;
        assert!(pa.get("error").is_none(), "A must be accepted: {pa}");
        let (mut b, pb) = raw_hello(&env.socket_path, account_hello("acct-b", "acct-box", Some("uid:900002"))).await;
        assert_eq!(code(&pb), Some("os_user_conflict"), "{pb}");
        assert_closed(&mut b).await;
        assert_closed(&mut a).await;
        let (mut c, pc) = raw_hello(&env.socket_path, account_hello("acct-c", "acct-box", Some("uid:900001"))).await;
        assert_eq!(code(&pc), Some("os_user_conflict"), "{pc}");
        assert_closed(&mut c).await;
        let (mut d, pd) = raw_hello(&env.socket_path, account_hello("acct-d", "other-box", Some("uid:900002"))).await;
        assert!(pd.get("error").is_none(), "another computer is served: {pd}");
        let v = call(&mut d, 2, op::VERSION_QUERY, serde_json::json!({})).await;
        for refused in ["acct-a", "acct-b", "acct-c"] {
            assert!(client_row(&v, refused).is_none(), "{refused} must not be listed: {v}");
        }
        assert!(client_row(&v, "acct-d").is_some(), "{v}");
    };
    tokio::time::timeout(BOUND, body).await.expect("exchange did not finish within BOUND");
}

/// An account that said hello and left still counts: alternating logins on one computer are caught.
#[tokio::test]
async fn alternating_accounts_on_one_computer_are_caught() {
    let env = Env::spawn("alternating");
    let body = async {
        let (mut witness, pw) = raw_hello(&env.socket_path, account_hello("witness", "witness-box", Some("uid:900003"))).await;
        assert!(pw.get("error").is_none(), "{pw}");
        let (a, pa) = raw_hello(&env.socket_path, account_hello("alt-a", "alt-box", Some("uid:900001"))).await;
        assert!(pa.get("error").is_none(), "{pa}");
        drop(a);
        wait_until_gone(&mut witness, 2, "alt-a").await;
        let (mut b, pb) = raw_hello(&env.socket_path, account_hello("alt-b", "alt-box", Some("uid:900002"))).await;
        assert_eq!(code(&pb), Some("os_user_conflict"), "{pb}");
        assert_closed(&mut b).await;
    };
    tokio::time::timeout(BOUND, body).await.expect("exchange did not finish within BOUND");
}

/// Every hello declares its computer and its OS account.
#[tokio::test]
async fn a_hello_without_host_or_os_user_is_refused() {
    let env = Env::spawn("identity-missing");
    let body = async {
        for (id, hello) in [
            ("no-user", without(account_hello("no-user", "id-box", Some("uid:900001")), "os_user")),
            ("no-host", without(account_hello("no-host", "id-box", Some("uid:900001")), "host")),
        ] {
            let (mut conn, p) = raw_hello(&env.socket_path, hello).await;
            assert_eq!(code(&p), Some("identity_missing"), "{id}: {p}");
            assert_closed(&mut conn).await;
        }
    };
    tokio::time::timeout(BOUND, body).await.expect("exchange did not finish within BOUND");
}

/// No op is served before a hello: the connection gets the `unauthenticated` reply, then its end, and
/// none of the mail another client sends meanwhile.
#[tokio::test]
async fn skipping_hello_gets_no_mail_and_is_closed() {
    let env = Env::spawn("skip-hello");
    let body = async {
        let mut s = poll_until_connected(&env.socket_path).await;
        fire(&mut s, 1, op::PING, serde_json::json!({})).await;
        let (mut m, pm) = raw_hello(&env.socket_path, account_hello("mailer", "skip-box", Some("uid:900001"))).await;
        assert!(pm.get("error").is_none(), "{pm}");
        let mail = serde_json::json!({"from": "mailer", "to": "", "text": "secret", "id": "skip-1"});
        let sent = call(&mut m, 2, op::AGENT_SEND, mail).await;
        assert_eq!(sent.get("ok").and_then(|v| v.as_bool()), Some(true), "{sent}");
        let mut seen = Vec::new();
        let drained = tokio::time::timeout(Duration::from_secs(3), async {
            while let Ok((frame, _blob)) = codec::read_frame(&mut s).await {
                seen.push(frame);
            }
        })
        .await;
        let first = seen.first().expect("an unauthenticated reply");
        assert_eq!(first.kind, Kind::Res, "{first:?}");
        assert_eq!(code(&first.payload), Some("unauthenticated"), "{:?}", first.payload);
        assert!(seen.iter().all(|f| f.op != op::AGENT_MESSAGE), "mail reached a connection that skipped hello: {seen:?}");
        assert!(drained.is_ok(), "the connection that skipped hello must be closed");
    };
    tokio::time::timeout(BOUND, body).await.expect("exchange did not finish within BOUND");
}

/// One hello per connection: a second closes it without a reply.
#[tokio::test]
async fn a_second_hello_closes_the_connection() {
    let env = Env::spawn("second-hello");
    let body = async {
        let (mut conn, p) = raw_hello(&env.socket_path, account_hello("twice", "twice-box", Some("uid:900001"))).await;
        assert!(p.get("error").is_none(), "{p}");
        codec::write_frame(&mut conn, &Frame::req(2, op::HELLO, account_hello("twice", "twice-box", Some("uid:900001"))), None)
            .await
            .expect("write second hello");
        let end = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match codec::read_frame(&mut conn).await {
                    Ok((frame, _blob)) => assert!(!(frame.kind == Kind::Res && frame.id == 2), "a second hello got a reply: {frame:?}"),
                    Err(_) => return,
                }
            }
        })
        .await;
        assert!(end.is_ok(), "a second hello must close the connection");
    };
    tokio::time::timeout(BOUND, body).await.expect("exchange did not finish within BOUND");
}

/// The refusal names no OS account, and the roster never reports one.
#[tokio::test]
async fn a_refusal_names_no_account() {
    let env = Env::spawn("names-none");
    let body = async {
        let (_a, pa) = raw_hello(&env.socket_path, account_hello("acct-a", "acct-box", Some("uid:900001"))).await;
        assert!(pa.get("error").is_none(), "{pa}");
        let (mut b, pb) = raw_hello(&env.socket_path, account_hello("acct-b", "acct-box", Some("uid:900002"))).await;
        assert_eq!(code(&pb), Some("os_user_conflict"), "{pb}");
        let whole = pb.to_string();
        assert!(!whole.contains("900001") && !whole.contains("900002"), "the refusal names an account: {whole}");
        assert_closed(&mut b).await;
        let (mut d, _) = raw_hello(&env.socket_path, account_hello("acct-d", "other-box", Some("uid:900003"))).await;
        let v = call(&mut d, 2, op::VERSION_QUERY, serde_json::json!({})).await;
        for row in v.get("clients").and_then(|c| c.as_array()).expect("clients") {
            assert!(row.get("os_user").is_none(), "the roster reports an account: {row}");
        }
    };
    tokio::time::timeout(BOUND, body).await.expect("exchange did not finish within BOUND");
}

/// Decision 0031: every refused hello closes its connection, not only an
/// account conflict.
#[tokio::test]
async fn a_refused_hello_closes_the_connection() {
    let env = Env::spawn("refused-close");
    let body = async {
        let mut hello = account_hello("old-fe", "test-host", None);
        hello["protocol"] = serde_json::json!(sot_protocol::PROTOCOL_VERSION - 1);
        let (mut conn, p) = raw_hello(&env.socket_path, hello).await;
        assert_eq!(p.get("code").and_then(|v| v.as_str()), Some("protocol_mismatch"), "{p}");
        assert_closed(&mut conn).await;
    };
    tokio::time::timeout(BOUND, body).await.expect("exchange did not finish within BOUND");
}
