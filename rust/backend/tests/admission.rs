#![cfg(any(windows, target_os = "linux"))]
//! Admission, ADR 0049 `## User isolation`: a process is "refused before anything is served", and "Two OS users on
//! one hub account get a loud refusal". Every connection, whatever it becomes, is admitted once at its first frame:
//! an accepted `hello`. These tests pin that over the real wire against a real `sotd`, with raw JSON hellos whose
//! `protocol` is this build's. A synthetic account names each client (`uid:900001`), because the daemon's own
//! account is never what is under test.

#[allow(dead_code, reason = "the shared fixture serves more suites than this one uses")]
mod support;

use std::time::Duration;

use serde_json::{json, Value};
use sot_protocol::{codec, op, Frame, Kind};
use support::{call, poll_until, try_connect, Conn, Env, BOUND};

const ACCOUNT_A: &str = "uid:900001";
const ACCOUNT_B: &str = "uid:900002";
/// The wire literal of the role that means "my next frame is `proxy.connect`, `lane.connect` or `fe.lease`".
const HANDOFF: &str = "handoff";

/// A refusal's end must come at once; a connection still open after this was not closed.
const CLOSE_WITHIN: Duration = Duration::from_secs(3);

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn open(env: &Env) -> Conn {
    let stream = poll_until(|| async { try_connect(&env.socket_path).await }, BOUND, "sotd's socket").await;
    tokio::io::BufReader::new(stream)
}

/// A raw hello payload declaring `host` and `os_user`.
fn hello(client_id: &str, host: &str, os_user: &str, role: &str) -> Value {
    json!({
        "client_id": client_id,
        "protocol": sot_protocol::PROTOCOL_VERSION,
        "app_version": sot_protocol::app_version(),
        "host": host,
        "os_user": os_user,
        "role": role,
        "name": format!("{role}@{client_id}"),
    })
}

fn without(mut payload: Value, key: &str) -> Value {
    payload.as_object_mut().expect("a hello is an object").remove(key);
    payload
}

fn code(payload: &Value) -> Option<&str> {
    payload.get("code").and_then(Value::as_str)
}

/// Writes every frame in one write, as a client that pipelines its hello with its next frame does.
async fn write_together(conn: &mut Conn, frames: &[Frame]) {
    let mut bytes = Vec::new();
    for f in frames {
        codec::write_frame(&mut bytes, f, None).await.expect("encode a frame");
    }
    tokio::io::AsyncWriteExt::write_all(conn, &bytes).await.expect("write the frames");
    tokio::io::AsyncWriteExt::flush(conn).await.expect("flush the frames");
}

/// Every frame the daemon sends until it closes the connection.
async fn until_closed(conn: &mut Conn, what: &str) -> Vec<Frame> {
    let mut seen = Vec::new();
    let end = tokio::time::timeout(CLOSE_WITHIN, async {
        while let Ok((frame, _blob)) = codec::read_frame(conn).await {
            seen.push(frame);
        }
    })
    .await;
    assert!(end.is_ok(), "{what}: the connection stayed open; it sent {seen:?}");
    seen
}

/// A connection that says hello and reads its reply (any replayed evt before it is skipped).
async fn said_hello(env: &Env, payload: Value) -> (Conn, Value) {
    let mut conn = open(env).await;
    write_together(&mut conn, &[Frame::req(1, op::HELLO, payload)]).await;
    let body = async {
        loop {
            let (frame, _blob) = codec::read_frame(&mut conn).await.expect("read the hello reply");
            if frame.kind == Kind::Res && frame.id == 1 {
                return frame.payload;
            }
        }
    };
    let reply = tokio::time::timeout(BOUND, body).await.unwrap_or_else(|_| panic!("no reply to hello in {BOUND:?}"));
    (conn, reply)
}

/// A refused hello is one reply with `want` as its code, then the connection's end.
async fn assert_refused(env: &Env, payload: Value, want: &str) -> Value {
    let (mut conn, reply) = said_hello(env, payload).await;
    assert_eq!(code(&reply), Some(want), "{reply}");
    let rest = until_closed(&mut conn, want).await;
    assert!(rest.is_empty(), "{want}: nothing follows the refusal: {rest:?}");
    reply
}

/// The roster as `version.query` shows it has `client_id`.
fn listed(version: &Frame, client_id: &str) -> bool {
    let clients = version.payload.get("clients").and_then(Value::as_array).expect("clients");
    clients.iter().any(|c| c.get("client_id").and_then(Value::as_str) == Some(client_id))
}

/// The done test. Frame 1 of every connection kind, and of two ops that are no kind of connection, is refused
/// `unauthenticated` and the connection closed; after a hello with role `handoff` each of the three handoff ops
/// reaches its handler in the same write; a handoff connection serves nothing else; a control connection's second
/// hello closes it.
#[tokio::test]
async fn every_connection_kind_needs_an_accepted_hello() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("admit-kinds");
    env.spawn_sotd();
    let proxy = (op::PROXY_CONNECT, json!({"port": 1}));
    let lane = (op::LANE_CONNECT, json!({"target": "no-such-row", "lane": "supervisor"}));
    let lease = (op::FE_LEASE, json!({"boot": "", "pid": 1, "created": 0}));
    // (a) No hello first: one `unauthenticated` reply, then the end, and nothing else.
    let others = [
        (op::FILE_READ, json!({"path": "a.txt"})),
        (op::AGENT_SEND, json!({"from": "t", "to": "", "text": "x", "id": "adm-1"})),
    ];
    for (name, payload) in [&proxy, &lane, &lease].into_iter().chain(others.iter()) {
        let mut conn = open(&env).await;
        write_together(&mut conn, &[Frame::req(1, name, payload.clone())]).await;
        let seen = until_closed(&mut conn, name).await;
        assert_eq!(seen.len(), 1, "{name}: exactly one reply: {seen:?}");
        assert_eq!(code(&seen[0].payload), Some("unauthenticated"), "{name}: {:?}", seen[0].payload);
    }
    // (b) A handoff hello and the connect frame in one write: both reach their owners, then the end.
    for ((name, payload), want) in [(&proxy, "bad_port"), (&lane, "unknown_workspace"), (&lease, "")] {
        let mut conn = open(&env).await;
        let first = Frame::req(1, op::HELLO, hello("adm-handoff", "host-a", ACCOUNT_A, HANDOFF));
        write_together(&mut conn, &[first, Frame::req(2, name, payload.clone())]).await;
        let seen = until_closed(&mut conn, name).await;
        assert_eq!(seen.len(), 2, "{name}: the hello's reply and the handler's: {seen:?}");
        assert_eq!((seen[0].id, seen[0].payload.get("error")), (1, None), "{name}: hello accepted: {:?}", seen[0]);
        assert_eq!(seen[1].id, 2, "{name}: {:?}", seen[1]);
        if want.is_empty() {
            let outcome = seen[1].payload.get("outcome").and_then(Value::as_str);
            assert!(outcome.is_some_and(|o| o != "granted"), "{name}: the lease's own refusal: {:?}", seen[1].payload);
        } else {
            assert_eq!(code(&seen[1].payload), Some(want), "{name}: {:?}", seen[1].payload);
        }
    }
    // (c) A handoff connection is not a control session: any other second frame is `bad_request`, then the end.
    let mut conn = open(&env).await;
    let first = Frame::req(1, op::HELLO, hello("adm-handoff", "host-a", ACCOUNT_A, HANDOFF));
    write_together(&mut conn, &[first, Frame::req(2, op::VERSION_QUERY, json!({}))]).await;
    let seen = until_closed(&mut conn, "a handoff connection's version.query").await;
    assert_eq!(seen.len(), 2, "the hello's reply and the refusal: {seen:?}");
    assert_eq!(code(&seen[1].payload), Some("bad_request"), "{:?}", seen[1].payload);
    // (d) A control connection's second hello closes it with no reply.
    let (mut conn, reply) = said_hello(&env, hello("adm-control", "host-a", ACCOUNT_A, "cli")).await;
    assert!(reply.get("error").is_none(), "{reply}");
    write_together(&mut conn, &[Frame::req(2, op::HELLO, hello("adm-control", "host-a", ACCOUNT_A, "cli"))]).await;
    let seen = until_closed(&mut conn, "a second hello").await;
    assert!(seen.iter().all(|f| f.kind != Kind::Res), "a second hello got a reply: {seen:?}");
    env.kill_daemon_bounded().await;
}

/// ADR 0049 `## User isolation`: "Two OS users on one hub account get a loud refusal". The second account to say
/// hello for a host is refused with `os_user_conflict` and the end; the host is then refused for either account, as
/// control and as handoff, until the daemon restarts; another host is served; the first account's open connection
/// is left alone; and no refusal names an account.
#[tokio::test]
async fn a_second_os_account_on_one_host_is_refused_loudly() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("admit-accounts");
    env.spawn_sotd();
    let (mut a, reply) = said_hello(&env, hello("acct-a", "host-h", ACCOUNT_A, "cli")).await;
    assert!(reply.get("error").is_none(), "the first account is served: {reply}");
    let refusal = assert_refused(&env, hello("acct-b", "host-h", ACCOUNT_B, "cli"), "os_user_conflict").await;
    let message = refusal["error"].as_str().expect("the refusal's message");
    assert!(message.contains("host-h"), "the message names the host: {message}");
    for account in ["900001", "900002"] {
        assert!(!refusal.to_string().contains(account), "the refusal names an account: {refusal}");
    }
    for (client, account, role) in [
        ("again-a", ACCOUNT_A, "cli"),
        ("again-b", ACCOUNT_B, "cli"),
        ("hand-a", ACCOUNT_A, HANDOFF),
        ("hand-b", ACCOUNT_B, HANDOFF),
    ] {
        assert_refused(&env, hello(client, "host-h", account, role), "os_user_conflict").await;
    }
    let (mut other, reply) = said_hello(&env, hello("acct-g", "host-g", ACCOUNT_B, "cli")).await;
    assert!(reply.get("error").is_none(), "another host is served: {reply}");
    let version = call(&mut other, 2, op::VERSION_QUERY, json!({})).await;
    assert!(listed(&version, "acct-a") && listed(&version, "acct-g"), "{:?}", version.payload);
    for refused in ["acct-b", "again-a", "again-b", "hand-a", "hand-b"] {
        assert!(!listed(&version, refused), "{refused} was refused and must not be listed: {:?}", version.payload);
    }
    let still = call(&mut a, 2, op::VERSION_QUERY, json!({})).await;
    assert!(still.payload.get("error").is_none(), "the first account's open connection answers: {:?}", still.payload);
    env.kill_daemon_bounded().await;
}

/// Decision 1: a client that says hello and then only reads (the hub link does) is a control session at once and
/// gets the broadcasts, with no second frame from it.
#[tokio::test]
async fn a_connection_that_only_listens_after_its_hello_gets_broadcasts() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("admit-listen");
    env.spawn_sotd();
    let (mut listener, reply) = said_hello(&env, hello("listener", "host-l", ACCOUNT_A, "cli")).await;
    assert!(reply.get("error").is_none(), "{reply}");
    let (mut sender, reply) = said_hello(&env, hello("sender", "host-l", ACCOUNT_A, "cli")).await;
    assert!(reply.get("error").is_none(), "{reply}");
    let mail = json!({"from": "sender", "to": "", "text": "for the listener", "id": "adm-listen-1"});
    let sent = call(&mut sender, 2, op::AGENT_SEND, mail).await;
    assert_eq!(sent.payload.get("ok").and_then(Value::as_bool), Some(true), "{:?}", sent.payload);
    let heard = tokio::time::timeout(BOUND, async {
        loop {
            let (frame, _blob) = codec::read_frame(&mut listener).await.expect("the listener's connection stays open");
            if frame.op == op::AGENT_MESSAGE {
                return frame;
            }
        }
    })
    .await
    .expect("the broadcast reached the listener");
    assert_eq!(heard.payload["text"], "for the listener", "{heard:?}");
    env.kill_daemon_bounded().await;
}

/// An account that said hello and left still counts: alternating logins on one computer are caught.
#[tokio::test]
async fn alternating_accounts_on_one_computer_are_caught() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("admit-alternate");
    env.spawn_sotd();
    let (mut witness, reply) = said_hello(&env, hello("witness", "witness-box", ACCOUNT_A, "cli")).await;
    assert!(reply.get("error").is_none(), "{reply}");
    let (first, reply) = said_hello(&env, hello("alt-a", "alt-box", ACCOUNT_A, "cli")).await;
    assert!(reply.get("error").is_none(), "{reply}");
    drop(first);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let version = call(&mut witness, 2, op::VERSION_QUERY, json!({})).await;
        if !listed(&version, "alt-a") {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "alt-a never left the roster");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_refused(&env, hello("alt-b", "alt-box", ACCOUNT_B, "cli"), "os_user_conflict").await;
    env.kill_daemon_bounded().await;
}

/// Every hello declares its computer and its OS account.
#[tokio::test]
async fn a_hello_without_host_or_os_user_is_refused() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("admit-identity");
    env.spawn_sotd();
    for key in ["os_user", "host"] {
        let payload = without(hello("no-identity", "id-box", ACCOUNT_A, "cli"), key);
        assert_refused(&env, payload, "identity_missing").await;
    }
    env.kill_daemon_bounded().await;
}

/// The refusal names no OS account, and the roster never reports one.
#[tokio::test]
async fn a_refusal_names_no_account() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("admit-names-none");
    env.spawn_sotd();
    let (_a, reply) = said_hello(&env, hello("acct-a", "acct-box", ACCOUNT_A, "cli")).await;
    assert!(reply.get("error").is_none(), "{reply}");
    let refusal = assert_refused(&env, hello("acct-b", "acct-box", ACCOUNT_B, "cli"), "os_user_conflict").await;
    let whole = refusal.to_string();
    assert!(!whole.contains("900001") && !whole.contains("900002"), "the refusal names an account: {whole}");
    let (mut other, _) = said_hello(&env, hello("acct-d", "other-box", ACCOUNT_B, "cli")).await;
    let version = call(&mut other, 2, op::VERSION_QUERY, json!({})).await;
    for row in version.payload["clients"].as_array().expect("clients") {
        assert!(row.get("os_user").is_none(), "the roster reports an account: {row}");
    }
    env.kill_daemon_bounded().await;
}

/// Every refused hello closes its connection, not only an account conflict: a hello from another protocol is
/// answered `protocol_mismatch` and then closed.
#[tokio::test]
async fn a_refused_hello_closes_the_connection() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("admit-refused-close");
    env.spawn_sotd();
    let mut payload = hello("old-fe", "host-a", ACCOUNT_A, "fe");
    payload["protocol"] = json!(sot_protocol::PROTOCOL_VERSION - 1);
    assert_refused(&env, payload, "protocol_mismatch").await;
    env.kill_daemon_bounded().await;
}
