//! The chat request ops through a real connection, against the exported fake Slack.

use super::*;
use crate::chat::ops::{code_of, post_refusal_code};
use sot_chat::api::ApiError;
use sot_chat::http::HttpError;
use sot_protocol::ops::ChatCode;
use crate::chat::tests::{row, Home, SELF};
use crate::chat::{row_facts, Chat, Status};
use crate::lifecycle::retire::Registry;
use crate::server::conn::handle_connection;
use serde_json::{json, Value};
use sot_chat::config;
use sot_chat::fake_slack::{FakeSlack, Identity, Reply};
use sot_chat::service::Service;
use std::sync::atomic::{AtomicU8, Ordering};

/// What a chat op name gets from the server: a handler, or (an event) the unknown-op answer a request carrying its
/// name gets.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Answer {
    Handled,
    Event,
}

/// Every chat op name of the wire and its answer; the walk below fails for a name missing here.
const OPS: &[(&str, Answer)] = &[
    (op::CHAT_RESOLVE, Answer::Handled),
    (op::CHAT_HISTORY, Answer::Handled),
    (op::CHAT_POST, Answer::Handled),
    (op::CHAT_WATCH, Answer::Handled),
    (op::CHAT_UNWATCH, Answer::Handled),
    (op::CHAT_CHANGED, Answer::Event),
    (op::CHAT_STATUS, Answer::Event),
];

/// What the fake answers a chat call with.
pub(super) const OK: u8 = 0;
pub(super) const HOLD: u8 = 1;
pub(super) const DROP: u8 = 2;

/// The calls a request makes. The listener's own `auth.test` and `apps.connections.open` are in the fake's record too
/// and are not counted: they run with no admission.
fn is_chat_call(path: &str) -> bool {
    path.ends_with("/conversations.history") || path.ends_with("/conversations.replies") || path.ends_with("/chat.postMessage") || path.ends_with("/users.info")
}

/// A fake Slack whose history and post answers follow `mode`; `auth.test` names the workspace and the socket's ticket
/// is refused, so the service runs with no live socket.
pub(super) async fn slack(mode: &Arc<AtomicU8>) -> FakeSlack {
    let mode = mode.clone();
    FakeSlack::start(Identity::Trusted, move |seen, _| {
        let path = seen.path();
        let now = mode.load(Ordering::SeqCst);
        if path.ends_with("/auth.test") {
            Reply::json(&json!({"ok": true, "team_id": "T0AAAA", "user_id": "U0BOT", "bot_id": "B0BOT"}))
        } else if path.ends_with("/conversations.history") {
            if now == HOLD { Reply::Hang } else { Reply::json(&json!({"ok": true, "messages": [], "has_more": false})) }
        } else if path.ends_with("/chat.postMessage") {
            if now == DROP { Reply::Drop } else { Reply::json(&json!({"ok": true, "ts": "1700000000.000100"})) }
        } else {
            Reply::json(&json!({"ok": false, "error": "invalid_auth"}))
        }
    })
    .await
}

pub(super) fn calls(fake: &FakeSlack) -> usize {
    fake.seen().iter().filter(|s| is_chat_call(&s.path())).count()
}

pub(super) struct Rig {
    pub(super) _home: Home,
    pub(super) services: Services,
    pub(super) rows: Workspaces,
    /// The ids of rows alpha (folder a, C0AAAA) and beta (folder b, C0BBBB); folder c maps to nothing.
    pub(super) alpha: String,
    pub(super) beta: String,
    pub(super) folders: [std::path::PathBuf; 3],
    _root: tempfile::TempDir,
}

pub(super) fn starter(fake: &FakeSlack) -> crate::chat::Starter<Service> {
    let (http, base) = (fake.http(), fake.url("slack.com", "/api/"));
    Box::new(move |config, credentials, dir| {
        let (http, base) = (http.clone(), base.clone());
        Box::pin(async move { Service::start_at(config, credentials, http, dir, base).await.map(Arc::new).map_err(|e| e.code()) })
    })
}

pub(super) async fn running(chat: &Chat<Service>) {
    while chat.status() == Status::Starting {
        tokio::task::yield_now().await;
    }
}

/// The rig as the daemon's boot makes it: the chat's own composition, returned while the service is still starting. Its
/// `chat.toml` delivers nothing: a test that asks whether mail is delivered takes `rig_delivering`.
pub(super) async fn rig_booting(fake: &FakeSlack) -> Rig {
    rig_booting_with(fake, false).await
}

async fn rig_booting_with(fake: &FakeSlack, delivers: bool) -> Rig {
    let home = Home::new(Some(SELF));
    let folders = [home.folder("a"), home.folder("b"), home.folder("c")];
    let own = [folders[0].to_string_lossy().into_owned(), folders[1].to_string_lossy().into_owned()];
    if delivers { home.delivering_toml(&own) } else { home.chat_toml(&own) }
    home.secrets();
    let rows = Workspaces::new();
    let alpha = rows.insert(row("alpha", &folders[0])).workspace_id.clone();
    let beta = rows.insert(row("beta", &folders[1])).workspace_id.clone();
    let chat = Chat::boot(&home.env, &rows, &|p| config::read_file(p), &Registry::new(), starter(fake), crate::chat::HUB_CHECK, sot_chat::delivery::Timing::PRODUCT);
    let root = tempfile::tempdir().unwrap();
    let mut services = Services::for_test("t", root.path());
    services.chat = chat;
    services.workspaces = rows.clone();
    Rig { _home: home, services, rows, alpha, beta, folders, _root: root }
}

pub(super) async fn rig(fake: &FakeSlack) -> Rig {
    rig_started(fake, false).await
}

async fn rig_started(fake: &FakeSlack, delivers: bool) -> Rig {
    let rig = rig_booting_with(fake, delivers).await;
    running(&rig.services.chat).await;
    assert_eq!(rig.services.chat.status(), Status::Running, "the service starts against the fake: {:?}", fake.seen().iter().map(|s| s.target.clone()).collect::<Vec<_>>());
    rig
}

/// One open control connection.
pub(super) struct Conn {
    pub(super) crx: tokio::io::BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    pub(super) ctx: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    pub(super) task: tokio::task::JoinHandle<Result<()>>,
}

pub(super) async fn connect(services: Services) -> Conn {
    connect_as(services, "cli", None).await
}

/// A connection whose hello names a window (`role` `fe`, a name): the only kind a person's chat post is taken from.
pub(super) async fn connect_window(services: Services) -> Conn {
    connect_as(services, "fe", Some("fe@test")).await
}

async fn connect_as(services: Services, role: &str, name: Option<&str>) -> Conn {
    let (client, server) = tokio::io::duplex(1 << 16);
    let (srx, stx) = tokio::io::split(server);
    let task = tokio::spawn(handle_connection(srx, stx, services, None));
    let (crx, mut ctx) = tokio::io::split(client);
    let mut crx = codec::buffered(crx);
    let hello = json!({
        "client_id": "chat-test", "protocol": sot_protocol::PROTOCOL_VERSION, "app_version": sot_protocol::app_version(),
        "host": "test-host", "os_user": "test-user", "role": role, "name": name,
    });
    codec::write_frame(&mut ctx, &Frame::req(1, op::HELLO, hello), None).await.unwrap();
    let (reply, _) = codec::read_frame(&mut crx).await.unwrap();
    assert_eq!(reply.op, op::HELLO, "{reply:?}");
    Conn { crx, ctx, task }
}

impl Conn {
    pub(super) async fn send(&mut self, id: u64, name: &str, payload: Value) {
        codec::write_frame(&mut self.ctx, &Frame::req(id, name, payload), None).await.unwrap();
    }

    /// The payload of the reply to request `id`; other replies and events are passed over.
    pub(super) async fn reply(&mut self, id: u64) -> Value {
        loop {
            let (frame, _) = codec::read_frame(&mut self.crx).await.unwrap();
            if frame.kind == Kind::Res && frame.id == id {
                return frame.payload;
            }
        }
    }

    pub(super) async fn ask(&mut self, id: u64, name: &str, payload: Value) -> Value {
        self.send(id, name, payload).await;
        self.reply(id).await
    }
}

pub(super) fn target(row: &str, channel: &str) -> Value {
    json!({"row": row, "workspace": "T0AAAA", "channel": channel})
}

pub(super) fn history(row: &str, channel: &str) -> Value {
    json!({"target": target(row, channel), "limit": 5})
}

pub(super) fn watch(row: &str, channel: &str) -> Value {
    json!({"target": target(row, channel)})
}

pub(super) fn post(row: &str, channel: &str) -> Value {
    json!({"target": target(row, channel), "kind": "person", "name": "Ann", "text": "hello"})
}

pub(super) fn code(reply: &Value) -> &str {
    reply["code"].as_str().unwrap_or("none")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_current_row_reaches_slack_through_each_request_op() {
    let mode = Arc::new(AtomicU8::new(OK));
    let fake = slack(&mode).await;
    let rig = rig_started(&fake, true).await;
    let mut conn = connect_window(rig.services.clone()).await;
    let resolved = conn.ask(2, op::CHAT_RESOLVE, json!({"row": rig.alpha})).await;
    assert_eq!((resolved["state"].as_str(), resolved["channel"].as_str(), resolved["delivers"].as_bool()), (Some("mapped"), Some("C0AAAA"), Some(true)));
    assert_eq!(calls(&fake), 0, "a resolve calls nothing");
    let page = conn.ask(3, op::CHAT_HISTORY, history(&rig.alpha, "C0AAAA")).await;
    assert_eq!(page["records"], json!([]), "{page}");
    let posted = conn.ask(4, op::CHAT_POST, post(&rig.alpha, "C0AAAA")).await;
    assert_eq!((posted["outcome"].as_str(), posted["ts"].as_str()), (Some("posted"), Some("1700000000.000100")), "{posted}");
    assert_eq!(calls(&fake), 2, "one call for each of history and post");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_removed_replaced_remapped_or_anchor_row_makes_no_call_through_any_request_op() {
    let mode = Arc::new(AtomicU8::new(OK));
    let fake = slack(&mode).await;
    let rig = rig(&fake).await;
    let mut conn = connect(rig.services.clone()).await;
    let mut id = 10;
    let mut ask = |name: &'static str, payload: Value| {
        id += 1;
        (id, name, payload)
    };

    // Remapped: the row is current, and the expected channel is the other one.
    for (name, payload) in [(op::CHAT_HISTORY, history(&rig.alpha, "C0BBBB")), (op::CHAT_POST, post(&rig.alpha, "C0BBBB")), (op::CHAT_WATCH, watch(&rig.alpha, "C0BBBB"))] {
        let (n, name, payload) = ask(name, payload);
        assert_eq!(code(&conn.ask(n, name, payload).await), "stale_mapping", "{name} remapped");
    }
    // Unknown post kind and an undecodable request are `bad_request`, from the handler.
    let mut bad = post(&rig.alpha, "C0AAAA");
    bad["kind"] = json!("robot");
    for (name, payload) in [(op::CHAT_POST, bad), (op::CHAT_HISTORY, json!({"target": 3})), (op::CHAT_RESOLVE, json!({}))] {
        let (n, name, payload) = ask(name, payload);
        assert_eq!(code(&conn.ask(n, name, payload).await), "bad_request", "{name} undecodable");
    }
    // The anchor never acts; a resolve names it as not mapped.
    rig.rows.set_default(&rig.alpha);
    for (name, payload) in [(op::CHAT_HISTORY, history(&rig.alpha, "C0AAAA")), (op::CHAT_POST, post(&rig.alpha, "C0AAAA")), (op::CHAT_WATCH, watch(&rig.alpha, "C0AAAA"))] {
        let (n, name, payload) = ask(name, payload);
        assert_eq!(code(&conn.ask(n, name, payload).await), "unmapped", "{name} anchor");
    }
    let (n, name, payload) = ask(op::CHAT_RESOLVE, json!({"row": rig.alpha}));
    assert_eq!(conn.ask(n, name, payload).await["state"], "unmapped");
    let gamma = rig.rows.insert(row("gamma", &rig.folders[2])).workspace_id.clone();
    rig.rows.set_default(&gamma);
    // Removed, then replaced under a new id: the old id is gone for every op.
    rig.rows.remove_by_id(&rig.alpha).unwrap();
    rig.rows.insert(row("alpha", &rig.folders[0]));
    for (name, payload) in [
        (op::CHAT_HISTORY, history(&rig.alpha, "C0AAAA")),
        (op::CHAT_POST, post(&rig.alpha, "C0AAAA")),
        (op::CHAT_RESOLVE, json!({"row": rig.alpha})),
        (op::CHAT_WATCH, watch(&rig.alpha, "C0AAAA")),
    ] {
        let (n, name, payload) = ask(name, payload);
        assert_eq!(code(&conn.ask(n, name, payload).await), "stale_row", "{name} replaced");
    }
    assert_eq!(calls(&fake), 0, "no refused request reached Slack");
}

#[tokio::test(flavor = "multi_thread")]
async fn every_chat_op_name_has_a_handler_or_a_stated_answer() {
    let mode = Arc::new(AtomicU8::new(OK));
    let fake = slack(&mode).await;
    let rig = rig(&fake).await;
    let mut conn = connect(rig.services.clone()).await;
    let mut seen = 0;
    for (n, name) in op::ALL.iter().filter(|n| n.starts_with("chat.")).enumerate() {
        let answer = OPS.iter().find(|(o, _)| o == name).unwrap_or_else(|| panic!("{name} has no handler and no stated answer")).1;
        let reply = conn.ask(100 + n as u64, name, history(&rig.alpha, "C0AAAA")).await;
        match answer {
            // The events are the daemon's to send: a request carrying the name is an unknown op.
            Answer::Event => assert!(reply["error"].as_str().unwrap().starts_with("unknown op"), "{name}: {reply}"),
            // A running service and a current row: a name with a handler is neither unknown nor `unavailable`.
            Answer::Handled => {
                assert!(!reply["error"].as_str().unwrap_or("").starts_with("unknown op"), "{name}: {reply}");
                assert_ne!(code(&reply), "unavailable", "{name}: {reply}");
            }
        }
        seen += 1;
    }
    assert_eq!(seen, OPS.len(), "a table row names no op of the wire");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_request_is_unavailable_with_no_running_service() {
    let mode = Arc::new(AtomicU8::new(OK));
    let fake = slack(&mode).await;
    let rig = rig(&fake).await;
    let failing: crate::chat::Starter<Service> = Box::new(|_, _, _| Box::pin(async { Err("down") }));
    let down = Chat::begin_unowned(&rig._home.env, &row_facts(&rig.rows), &|p| config::read_file(p), &Registry::new(), failing);
    running(&down).await;
    let non_hub = Home::new(Some("elsewhere"));
    let bare = Home::new(Some(SELF));
    let unstarted = |home: &Home| Chat::<Service>::begin_unowned(&home.env, &[], &|p| config::read_file(p), &Registry::new(), starter(&fake));
    // The running one, stopped: a request after the stop began.
    let stopped = rig.services.chat.clone();
    crate::lifecycle::retire::Participant::request_stop(&*stopped);
    assert!(crate::lifecycle::retire::Participant::wait_retired(&*stopped, std::time::Duration::from_secs(60)));
    for (what, chat) in [("failed start", down), ("not the hub", unstarted(&non_hub)), ("no chat.toml", unstarted(&bare)), ("stopped", stopped)] {
        let mut services = rig.services.clone();
        services.chat = chat;
        let mut conn = connect(services).await;
        for (n, (name, payload)) in [(op::CHAT_HISTORY, history(&rig.alpha, "C0AAAA")), (op::CHAT_POST, post(&rig.alpha, "C0AAAA"))].into_iter().enumerate() {
            assert_eq!(code(&conn.ask(20 + n as u64, name, payload).await), "unavailable", "{what} {name}");
        }
        // A resolve shows the file's mapping without delivering (the file's, so a non-hub says unavailable and a hub
        // with no chat.toml says invalid).
        let resolved = conn.ask(30, op::CHAT_RESOLVE, json!({"row": rig.alpha})).await;
        match what {
            "not the hub" => assert_eq!(code(&resolved), "unavailable", "{what} resolve"),
            "no chat.toml" => assert_eq!(resolved["state"], "invalid", "{what} resolve: {resolved}"),
            _ => assert_eq!((resolved["state"].as_str(), resolved["delivers"].as_bool()), (Some("mapped"), Some(false)), "{what} resolve"),
        }
    }
    assert_eq!(calls(&fake), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn held_chat_request_does_not_delay_ping_or_other_ops() {
    let mode = Arc::new(AtomicU8::new(HOLD));
    let fake = slack(&mode).await;
    let rig = rig(&fake).await;
    let mut conn = connect(rig.services.clone()).await;
    conn.send(10, op::CHAT_HISTORY, history(&rig.alpha, "C0AAAA")).await;
    conn.send(11, op::PING, json!({})).await;
    conn.send(12, op::VERSION_QUERY, json!({})).await;
    // Both later replies arrive while the fake still holds the history answer: the history reply cannot have come.
    let (mut ping, mut other) = (false, false);
    while !(ping && other) {
        let (frame, _) = codec::read_frame(&mut conn.crx).await.unwrap();
        if frame.kind != Kind::Res {
            continue;
        }
        assert_ne!(frame.id, 10, "the held request answered early: {frame:?}");
        ping |= frame.id == 11;
        other |= frame.id == 12;
    }
    // Releasing the stall (the fake ends its connections) answers the request with `failed`; a stalled answer that
    // finishes with content is not exercised.
    while calls(&fake) == 0 {
        tokio::task::yield_now().await;
    }
    fake.shutdown().await;
    assert_eq!(code(&conn.reply(10).await), "failed");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_in_flight_when_its_connection_closes_ends_when_its_stall_is_released() {
    let mode = Arc::new(AtomicU8::new(HOLD));
    let fake = slack(&mode).await;
    let rig = rig(&fake).await;
    let held = Arc::strong_count(&rig.services.chat);
    let mut conn = connect(rig.services.clone()).await;
    conn.send(10, op::CHAT_HISTORY, history(&rig.alpha, "C0AAAA")).await;
    while calls(&fake) == 0 {
        tokio::task::yield_now().await;
    }
    let Conn { crx, ctx, task } = conn;
    drop((crx, ctx));
    task.await.unwrap().unwrap();
    // The job holds a copy of the services until it ends; releasing the stall ends it, and its reply has nowhere to go.
    fake.shutdown().await;
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while Arc::strong_count(&rig.services.chat) > held {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the in-flight chat job ended after its connection closed and its stall was released");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_post_whose_outcome_is_unknown_is_made_once_and_answered_unknown() {
    let mode = Arc::new(AtomicU8::new(DROP));
    let fake = slack(&mode).await;
    let rig = rig(&fake).await;
    let mut conn = connect_window(rig.services.clone()).await;
    let reply = conn.ask(10, op::CHAT_POST, post(&rig.alpha, "C0AAAA")).await;
    assert_eq!(reply["outcome"], "unknown", "{reply}");
    let posts = fake.seen().iter().filter(|s| s.path().ends_with("/chat.postMessage")).count();
    assert_eq!(posts, 1, "the handler sent the post again");
}

/// A row post as the verb sends it: the row's bound handle as the name, into `thread`.
fn row_post(row: &str, channel: &str, name: &str, thread: Option<&str>) -> Value {
    json!({"target": target(row, channel), "thread": thread, "kind": "row", "name": name, "text": "done"})
}

#[tokio::test(flavor = "multi_thread")]
async fn row_verbs_target_current_thread_and_submit_once() {
    let mode = Arc::new(AtomicU8::new(OK));
    let fake = slack(&mode).await;
    let rig = rig(&fake).await;
    rig.rows.set_agent_handle(&rig.alpha, "alpha-h").unwrap();
    // Not a window: a person's post, a row's post under another name (the other row's, a stranger's, none), and a row
    // post to the other row's channel under alpha's handle are refused, and none reaches Slack.
    let mut cli = connect(rig.services.clone()).await;
    let thread = "1700000050.000000";
    let refused = [
        post(&rig.alpha, "C0AAAA"),
        row_post(&rig.alpha, "C0AAAA", "someone-else", Some(thread)),
        row_post(&rig.alpha, "C0AAAA", "", Some(thread)),
        // The handle is compared exactly: no case folding, no trimming.
        row_post(&rig.alpha, "C0AAAA", "ALPHA-H", Some(thread)),
        row_post(&rig.alpha, "C0AAAA", " alpha-h", Some(thread)),
        row_post(&rig.alpha, "C0AAAA", "alpha-h\n", Some(thread)),
        row_post(&rig.beta, "C0BBBB", "alpha-h", Some(thread)),
    ];
    for (n, payload) in refused.into_iter().enumerate() {
        let reply = cli.ask(10 + n as u64, op::CHAT_POST, payload).await;
        // The daemon's own refusal, an error reply: not `Api::post`'s `outcome: refused`, which would also stop a spelling
        // the handle check let through (a trimmed or case-folded name).
        assert_eq!((code(&reply), reply.get("outcome")), ("bad_request", None), "refused post {n}: {reply}");
    }
    assert_eq!(calls(&fake), 0, "a refused post reached Slack");
    // A window may post as a person; its row post is held to the handle as well.
    let mut window = connect_window(rig.services.clone()).await;
    assert_eq!(window.ask(20, op::CHAT_POST, post(&rig.alpha, "C0AAAA")).await["outcome"], "posted");
    assert_eq!(code(&window.ask(21, op::CHAT_POST, row_post(&rig.alpha, "C0AAAA", "someone-else", None)).await), "bad_request");
    assert_eq!(calls(&fake), 1);
    // The row under its bound handle, from a connection that is not a window: one post, into the thread, under the
    // handle's username.
    let posted = cli.ask(30, op::CHAT_POST, row_post(&rig.alpha, "C0AAAA", "alpha-h", Some(thread))).await;
    assert_eq!((posted["outcome"].as_str(), posted["ts"].as_str()), (Some("posted"), Some("1700000000.000100")), "{posted}");
    let sent: Vec<Value> = fake.seen().iter().filter(|s| s.path().ends_with("/chat.postMessage")).filter_map(|s| s.json()).collect();
    assert_eq!(sent.len(), 2, "one post for the window's and one for the row's");
    assert_eq!((sent[1]["thread_ts"].as_str(), sent[1]["username"].as_str()), (Some(thread), Some("alpha-h (row via Ship of Tools)")), "{}", sent[1]);
}

#[test]
fn each_api_error_has_its_stated_code() {
    use std::time::Duration;
    let limited = ApiError::Limited { retry_after: Duration::from_secs(1), assumed: false };
    for (e, want) in [
        (ApiError::Http(HttpError::Busy), ChatCode::Busy),
        (ApiError::Http(HttpError::Timeout), ChatCode::Failed),
        (limited, ChatCode::Limited),
        (ApiError::Closed, ChatCode::Unavailable),
        (ApiError::BadRequest, ChatCode::BadRequest),
        (ApiError::BadBound, ChatCode::BadRequest),
        (ApiError::Altered, ChatCode::BadRequest),
        (ApiError::Malformed, ChatCode::Failed),
        (ApiError::Slack("x".into()), ChatCode::Failed),
        (ApiError::Status(500), ChatCode::Failed),
        (ApiError::NoIdentity, ChatCode::Failed),
        (ApiError::WrongWorkspace, ChatCode::Failed),
        (ApiError::Repeated, ChatCode::Failed),
        (ApiError::Missing, ChatCode::Failed),
        (ApiError::TooLarge, ChatCode::Failed),
    ] {
        assert_eq!(code_of(&e), want, "{e:?}");
    }
    for (why, want) in [(HttpError::Busy.code(), ChatCode::Busy), (HttpError::Invalid.code(), ChatCode::BadRequest), ("destination", ChatCode::Failed), ("http_403", ChatCode::Failed), ("channel_not_found", ChatCode::Failed)] {
        assert_eq!(post_refusal_code(why), want, "{why}");
    }
}
