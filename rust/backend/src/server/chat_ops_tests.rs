//! The chat request ops through a real connection, against the exported fake Slack.

use super::*;
use crate::chat::ops::{code_of, post_refusal_code};
use sot_chat::api::{ApiError, PostKind};
use sot_chat::http::HttpError;
use sot_protocol::ops::{ChatCode, MAX_CHAT_FILE};
use crate::chat::tests::{row, Home, SELF};
use crate::chat::{Chat, Status};
use crate::lifecycle::retire::Registry;
use crate::server::conn::handle_connection;
use serde_json::{json, Value};
use sot_chat::config;
use sot_chat::fake_slack::{FakeSlack, Files, Identity, Reply, Upload};
use sot_chat::service::Service;
use std::sync::atomic::{AtomicU8, Ordering};

/// What a chat op name gets from the server: a handler, or the unknown-op answer a request carrying its name gets
/// (an event, or a name the wire declares before the daemon serves it).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Answer {
    Handled,
    Event,
    /// Declared by the wire, with no handler yet: the daemon answers it as an unknown op.
    Unserved,
}

/// Every chat op name of the wire and its answer; the walk below fails for a name missing here.
const OPS: &[(&str, Answer)] = &[
    (op::CHAT_RESOLVE, Answer::Handled),
    (op::CHAT_HISTORY, Answer::Handled),
    (op::CHAT_POST, Answer::Handled),
    (op::CHAT_POST_FILE, Answer::Handled),
    (op::CHAT_FILE, Answer::Handled),
    (op::CHAT_WATCH, Answer::Handled),
    (op::CHAT_UNWATCH, Answer::Handled),
    (op::CHAT_CHANGED, Answer::Event),
    (op::CHAT_STATUS, Answer::Event),
    (op::CHAT_CONNECT, Answer::Handled),
    (op::CHAT_CHANNELS, Answer::Handled),
    (op::CHAT_PAIR, Answer::Handled),
];

/// What the fake answers a chat call with.
pub(super) const OK: u8 = 0;
pub(super) const HOLD: u8 = 1;
pub(super) const DROP: u8 = 2;
/// A file's first upload step refused by Slack.
const ASK_REFUSED: u8 = 3;
/// A file's share answered 429.
const SHARE_LIMITED: u8 = 4;
/// A post Slack refuses `not_in_channel`, and one it refuses `invalid_auth` (the token it was made with is dead).
const NOT_IN_CHANNEL: u8 = 5;
const TOKEN_GONE: u8 = 6;

/// The calls a request makes. The listener's own `auth.test` and `apps.connections.open` are in the fake's record too
/// and are not counted: they run with no admission.
fn is_chat_call(path: &str) -> bool {
    path.ends_with("/conversations.history") || path.ends_with("/conversations.replies") || path.ends_with("/chat.postMessage") || path.ends_with("/users.info") || is_upload_call(path)
}

/// A step of a file's upload (`fake_slack::Upload`'s three).
fn is_upload_call(path: &str) -> bool {
    path.ends_with("/files.getUploadURLExternal") || path == Upload::new().path() || path.ends_with("/files.completeUploadExternal")
}

/// A fake Slack whose history and post answers follow `mode`, and which answers a file's three upload steps as
/// `Upload` does (the share dropped under `DROP`); `auth.test` names the workspace and the socket's ticket is refused,
/// so the service runs with no live socket.
pub(super) async fn slack(mode: &Arc<AtomicU8>) -> FakeSlack {
    let (mode, upload) = (mode.clone(), Upload::new());
    FakeSlack::start(Identity::Trusted, move |seen, port| {
        let path = seen.path();
        let now = mode.load(Ordering::SeqCst);
        if path.ends_with("/files.completeUploadExternal") && now == DROP {
            return Reply::Drop;
        }
        if path.ends_with("/files.completeUploadExternal") && now == SHARE_LIMITED {
            return Reply::Status { code: 429, headers: vec![("Retry-After".into(), "7".into())], body: b"{}".to_vec() };
        }
        if path.ends_with("/files.getUploadURLExternal") && now == ASK_REFUSED {
            return Reply::json(&json!({"ok": false, "error": "invalid_arguments"}));
        }
        if path == upload.path() && now == HOLD {
            return Reply::Hang;
        }
        if let Some(r) = upload.reply(seen, port) {
            return r;
        }
        if path.ends_with("/auth.test") {
            Reply::json(&json!({"ok": true, "team_id": "T0AAAA", "user_id": "U0BOT", "bot_id": "B0BOT"}))
        } else if path.ends_with("/conversations.history") {
            if now == HOLD { Reply::Hang } else { Reply::json(&json!({"ok": true, "messages": [], "has_more": false})) }
        } else if path.ends_with("/chat.postMessage") {
            match now {
                DROP => Reply::Drop,
                NOT_IN_CHANNEL => Reply::json(&json!({"ok": false, "error": "not_in_channel"})),
                TOKEN_GONE => Reply::json(&json!({"ok": false, "error": "invalid_auth"})),
                _ => Reply::json(&json!({"ok": true, "ts": "1700000000.000100"})),
            }
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
    let chat = Chat::boot(&home.env, &rows, &|p| config::read_file(p), &Registry::new(), starter(fake), crate::chat::setup::Verifier::at(fake), crate::chat::HUB_CHECK, sot_chat::delivery::Timing::PRODUCT);
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

/// A fake whose `conversations.info` answers `channel` (private when `private`) or never answers (`None`).
async fn slack_naming(channel: Option<&'static str>, private: bool) -> FakeSlack {
    FakeSlack::start(Identity::Trusted, move |seen, _| {
        let path = seen.path();
        if path.ends_with("/auth.test") {
            Reply::json(&json!({"ok": true, "team_id": "T0AAAA", "user_id": "U0BOT", "bot_id": "B0BOT"}))
        } else if path.ends_with("/conversations.info") {
            match channel {
                Some(name) => Reply::json(&json!({"ok": true, "channel": {"name": name, "is_private": private}})),
                None => Reply::Hang,
            }
        } else {
            Reply::json(&json!({"ok": false, "error": "invalid_auth"}))
        }
    })
    .await
}

fn info_calls(fake: &FakeSlack) -> usize {
    fake.seen().iter().filter(|s| s.path().ends_with("/conversations.info")).count()
}

/// The service's start looks the mapped channels' names up once in the background, and a resolve carries what the cache
/// then holds without a call of its own; a name not found (here a lookup that never answers) leaves the reply as it was,
/// answered at once, with no `name`.
#[tokio::test(flavor = "multi_thread")]
async fn a_resolve_carries_the_name_the_start_found_and_none_before_it() {
    let fake = slack_naming(Some("general"), false).await;
    let rig = rig_started(&fake, false).await;
    let mut conn = connect_window(rig.services.clone()).await;
    let target = sot_protocol::ops::ChatTarget { row: rig.alpha.clone(), workspace: "T0AAAA".into(), channel: sot_protocol::ops::ChatChannel::try_from("C0AAAA".to_string()).unwrap() };
    let api = rig.services.chat.admit(&rig.rows, &target).expect("the service runs").api().unwrap();
    // The start looks up every configured channel (the rig maps two), one after another: the count below starts once it
    // has ended, so a lookup still running is never taken for a resolve's call.
    sot_chat::fake_slack::eventually("the start's lookup of both channels", || ["C0AAAA", "C0BBBB"].iter().all(|c| api.cached_name(c).is_some())).await;
    assert_eq!(info_calls(&fake), 2, "the lookup asked once for each configured channel");
    let resolved = conn.ask(2, op::CHAT_RESOLVE, json!({"row": rig.alpha})).await;
    assert_eq!((resolved["state"].as_str(), resolved["name"].as_str()), (Some("mapped"), Some("general")), "{resolved}");
    let before = info_calls(&fake);
    conn.ask(3, op::CHAT_RESOLVE, json!({"row": rig.alpha})).await;
    assert_eq!(info_calls(&fake), before, "a resolve asked Slack for the name");
    fake.shutdown().await;

    let fake = slack_naming(None, false).await;
    let rig = rig_started(&fake, false).await;
    let mut conn = connect_window(rig.services.clone()).await;
    let resolved = tokio::time::timeout(std::time::Duration::from_secs(5), conn.ask(2, op::CHAT_RESOLVE, json!({"row": rig.alpha}))).await.expect("a resolve waited for the name");
    assert_eq!(resolved["state"], "mapped");
    assert!(resolved.get("name").is_none(), "{resolved}");
    fake.shutdown().await;
}

/// A mapped channel Slack says is private is named as a public one is: the owner mapped it and the bot reads it.
#[tokio::test(flavor = "multi_thread")]
async fn a_resolve_carries_a_mapped_private_channels_name() {
    let fake = slack_naming(Some("secret"), true).await;
    let rig = rig_started(&fake, false).await;
    let mut conn = connect_window(rig.services.clone()).await;
    let target = sot_protocol::ops::ChatTarget { row: rig.alpha.clone(), workspace: "T0AAAA".into(), channel: sot_protocol::ops::ChatChannel::try_from("C0AAAA".to_string()).unwrap() };
    let api = rig.services.chat.admit(&rig.rows, &target).expect("the service runs").api().unwrap();
    sot_chat::fake_slack::eventually("the start's lookup of both channels", || ["C0AAAA", "C0BBBB"].iter().all(|c| api.cached_name(c).is_some())).await;
    let resolved = conn.ask(2, op::CHAT_RESOLVE, json!({"row": rig.alpha})).await;
    assert_eq!((resolved["state"].as_str(), resolved["name"].as_str()), (Some("mapped"), Some("secret")), "{resolved}");
    fake.shutdown().await;
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
            Answer::Event | Answer::Unserved => assert!(reply["error"].as_str().unwrap().starts_with("unknown op"), "{name}: {reply}"),
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
    let down = Chat::begin_unowned(&rig._home.env, &|p| config::read_file(p), &Registry::new(), failing);
    running(&down).await;
    let non_hub = Home::new(Some("elsewhere"));
    let bare = Home::new(Some(SELF));
    let unstarted = |home: &Home| Chat::<Service>::begin_unowned(&home.env, &|p| config::read_file(p), &Registry::new(), starter(&fake));
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
        // A resolve shows the file's mapping without delivering, except where a connect would fix the chat: a non-hub
        // says unavailable, and a hub with no chat.toml or a failed start says unconfigured.
        let resolved = conn.ask(30, op::CHAT_RESOLVE, json!({"row": rig.alpha})).await;
        match what {
            "not the hub" => assert_eq!(code(&resolved), "unavailable", "{what} resolve"),
            "no chat.toml" => assert_eq!(resolved, json!({"state": "unconfigured", "reason": "not_connected"}), "{what} resolve"),
            "failed start" => assert_eq!(resolved, json!({"state": "unconfigured", "reason": "start_failed"}), "{what} resolve"),
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

/// A file's bytes, as a test writes them into a row's folder.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n synthetic picture bytes";

/// A row's file post as the verb sends it: the row's bound handle as the name, the file by its node id.
fn file_post(row: &str, channel: &str, kind: &str, name: &str, node_id: &str) -> Value {
    json!({"target": target(row, channel), "thread": "1700000050.000000", "kind": kind, "name": name, "node_id": node_id, "comment": "the fit"})
}

/// The upload steps the fake saw, by path.
fn uploads(fake: &FakeSlack) -> Vec<String> {
    fake.seen().iter().map(|s| s.path()).filter(|p| is_upload_call(p)).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_row_posts_a_file_of_its_folder_under_its_own_handle_once() {
    let mode = Arc::new(AtomicU8::new(OK));
    let fake = slack(&mode).await;
    let rig = rig(&fake).await;
    rig.rows.set_agent_handle(&rig.alpha, "alpha-h").unwrap();
    std::fs::create_dir(rig.folders[0].join("figs")).unwrap();
    std::fs::write(rig.folders[0].join("figs/fit.png"), PNG).unwrap();
    let mut cli = connect(rig.services.clone()).await;
    // A person's file from a connection that is not a window, and a row's under another name, are refused before the
    // file is read: a file that does not exist still answers `bad_request`, never `not_a_file`.
    for (n, (kind, name, node)) in [("person", "alpha-h", "files:figs/fit.png"), ("row", "someone-else", "files:figs/fit.png"), ("row", "ALPHA-H", "files:no-such.png")].into_iter().enumerate() {
        let reply = cli.ask(10 + n as u64, op::CHAT_POST_FILE, file_post(&rig.alpha, "C0AAAA", kind, name, node)).await;
        assert_eq!((code(&reply), reply.get("outcome")), ("bad_request", None), "{kind} {name}: {reply}");
    }
    // The other row's channel, and a row that is gone: the recheck refuses each before anything is read or sent.
    let reply = cli.ask(20, op::CHAT_POST_FILE, file_post(&rig.alpha, "C0BBBB", "row", "alpha-h", "files:no-such.png")).await;
    assert_eq!(code(&reply), "stale_mapping", "{reply}");
    let reply = cli.ask(21, op::CHAT_POST_FILE, file_post("ws-gone-0000", "C0AAAA", "row", "alpha-h", "files:no-such.png")).await;
    assert_eq!(code(&reply), "stale_row", "{reply}");
    assert_eq!(calls(&fake), 0, "a refused file post reached Slack");
    // Under its handle it is posted (the steps themselves are goal 1's test, `a_rows_figure_reaches_its_thread_once`).
    let posted = cli.ask(30, op::CHAT_POST_FILE, file_post(&rig.alpha, "C0AAAA", "row", "alpha-h", "files:figs/fit.png")).await;
    assert_eq!(posted, json!({"outcome": "posted", "file": "F0FILE", "ts": "1700000800.000100"}));
    assert_eq!(uploads(&fake).len(), 3);
    // A share that is never acknowledged is unknown, and is not sent again.
    mode.store(DROP, Ordering::SeqCst);
    let unknown = cli.ask(31, op::CHAT_POST_FILE, file_post(&rig.alpha, "C0AAAA", "row", "alpha-h", "files:figs/fit.png")).await;
    assert_eq!(unknown, json!({"outcome": "unknown"}));
    assert_eq!(uploads(&fake).len(), 6, "the unacknowledged share was sent again: {:?}", uploads(&fake));
    // Slack's refusal of the first step shares nothing: `refused`, steps 2 and 3 never sent (the tool's exit 2).
    mode.store(ASK_REFUSED, Ordering::SeqCst);
    let refused = cli.ask(32, op::CHAT_POST_FILE, file_post(&rig.alpha, "C0AAAA", "row", "alpha-h", "files:figs/fit.png")).await;
    assert_eq!(refused, json!({"outcome": "refused", "code": "failed"}));
    assert_eq!(uploads(&fake).len(), 7, "a step after a refused first step was sent: {:?}", uploads(&fake));
    // A share Slack limits is `limited` with its wait (exit 2), sent once.
    mode.store(SHARE_LIMITED, Ordering::SeqCst);
    let limited = cli.ask(33, op::CHAT_POST_FILE, file_post(&rig.alpha, "C0AAAA", "row", "alpha-h", "files:figs/fit.png")).await;
    assert_eq!(limited, json!({"outcome": "limited", "retry_after_secs": 7, "assumed": false}));
    assert_eq!(uploads(&fake).len(), 10);
}

/// Runs `then` just before the daemon opens the file at `path`, once: a test's stand-in for another process acting
/// between the composition of a node and the open.
#[cfg(unix)]
fn before_open(path: std::path::PathBuf, then: impl FnOnce() + Send + 'static) {
    crate::chat::ops::BEFORE_OPEN.lock().unwrap().push((path, Box::new(then)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_outside_the_folder_or_of_the_config_folder_is_refused_before_slack() {
    let mode = Arc::new(AtomicU8::new(OK));
    let fake = slack(&mode).await;
    let rig = rig(&fake).await;
    rig.rows.set_agent_handle(&rig.alpha, "alpha-h").unwrap();
    let folder = &rig.folders[0];
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("o.png"), PNG).unwrap();
    std::fs::write(folder.join("empty.png"), b"").unwrap();
    std::fs::write(folder.join("big.bin"), vec![7u8; MAX_CHAT_FILE + 1]).unwrap();
    std::fs::write(folder.join("max.bin"), vec![7u8; MAX_CHAT_FILE]).unwrap();
    std::fs::create_dir(folder.join("sub")).unwrap();
    std::fs::create_dir(folder.join("figs")).unwrap();
    std::fs::write(folder.join("figs/fit.png"), PNG).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let config = rig._home.config();
        symlink(&config, folder.join("conf")).unwrap();
        symlink(config.join(config::SECRETS_FILE), folder.join("secrets.png")).unwrap();
        symlink(outside.path(), folder.join("results")).unwrap();
        symlink(folder.join("figs/fit.png"), folder.join("alias.png")).unwrap();
        let fifo = std::ffi::CString::new(folder.join("pipe.png").to_string_lossy().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        // A hard link is the secrets file under a name inside the project.
        std::fs::hard_link(config.join(config::SECRETS_FILE), folder.join("hard.png")).unwrap();
        // A folder of the project swapped for a link to the configuration folder between the composition and the open.
        std::fs::create_dir(folder.join("d")).unwrap();
        std::fs::write(folder.join("d").join(config::SECRETS_FILE), b"a decoy").unwrap();
        let (d, held, to) = (folder.join("d"), folder.join("d-held"), config.clone());
        before_open(folder.join("d").join(config::SECRETS_FILE), move || {
            std::fs::rename(&d, &held).unwrap();
            symlink(&to, &d).unwrap();
        });
        // A kernel file whose size says nothing of what a read gives.
        if cfg!(target_os = "linux") {
            symlink("/proc/self/environ", folder.join("env.png")).unwrap();
        }
    }
    let mut cli = connect(rig.services.clone()).await;
    let mut refused = vec![
        ("files:../b/x.png", "not_a_file"),
        ("files:/etc/hostname", "not_a_file"),
        ("files:", "not_a_file"),
        ("files:sub", "not_a_file"),
        ("files:no-such.png", "not_a_file"),
        ("files:empty.png", "not_a_file"),
        ("files:big.bin", "too_large"),
        ("figs/fit.png", "not_a_file"),
    ];
    if cfg!(unix) {
        refused.extend([
            ("files:pipe.png", "not_a_file"),
            ("files:conf/chat-secrets.toml", "not_a_file"),
            ("files:conf/chat.toml", "not_a_file"),
            ("files:secrets.png", "not_a_file"),
            ("files:hard.png", "not_a_file"),
            ("files:d/chat-secrets.toml", "not_a_file"),
        ]);
    }
    if cfg!(target_os = "linux") {
        refused.push(("files:env.png", "not_a_file"));
    }
    for (n, (node, want)) in refused.into_iter().enumerate() {
        let reply = tokio::time::timeout(std::time::Duration::from_secs(10), cli.ask(10 + n as u64, op::CHAT_POST_FILE, file_post(&rig.alpha, "C0AAAA", "row", "alpha-h", node))).await.expect("a refusal waited");
        assert_eq!((code(&reply), reply.get("outcome")), (want, None), "{node}: {reply}");
    }
    assert_eq!(calls(&fake), 0, "a refused file reached Slack");
    // A file of exactly the bound posts, and so do one a link in the folder reaches outside the configuration folder
    // and a link to a file of another name, shared under the link's own name.
    let mut posts = vec![("files:max.bin", "max.bin", MAX_CHAT_FILE)];
    if cfg!(unix) {
        posts.extend([("files:results/o.png", "o.png", PNG.len()), ("files:alias.png", "alias.png", PNG.len())]);
    }
    for (n, (node, _, _)) in posts.iter().enumerate() {
        let reply = cli.ask(40 + n as u64, op::CHAT_POST_FILE, file_post(&rig.alpha, "C0AAAA", "row", "alpha-h", node)).await;
        assert_eq!(reply["outcome"], "posted", "{node}: {reply}");
    }
    let seen = fake.seen();
    let names: Vec<String> = seen.iter().filter(|s| s.path().ends_with("/files.getUploadURLExternal")).filter_map(|s| s.param("filename")).collect();
    let sizes: Vec<usize> = seen.iter().filter(|s| s.path() == Upload::new().path()).map(|s| s.body.len()).collect();
    assert_eq!(names, posts.iter().map(|p| p.1.to_string()).collect::<Vec<_>>(), "each share is named by its node");
    assert_eq!(sizes, posts.iter().map(|p| p.2).collect::<Vec<_>>(), "the bytes sent are the files'");
}

/// No file is read before a transfer permit is held: with two shares under way, a third is `busy`, and since its file
/// does not exist, a read before the permit would have answered `not_a_file`.
#[tokio::test(flavor = "multi_thread")]
async fn a_third_file_post_at_once_is_busy_before_its_file_is_read() {
    let mode = Arc::new(AtomicU8::new(HOLD));
    let fake = slack(&mode).await;
    let rig = rig(&fake).await;
    rig.rows.set_agent_handle(&rig.alpha, "alpha-h").unwrap();
    std::fs::write(rig.folders[0].join("fit.png"), PNG).unwrap();
    let mut cli = connect(rig.services.clone()).await;
    for id in [10, 11] {
        cli.send(id, op::CHAT_POST_FILE, file_post(&rig.alpha, "C0AAAA", "row", "alpha-h", "files:fit.png")).await;
    }
    let held = Upload::new().path().to_string();
    sot_chat::fake_slack::eventually("two uploads held", || fake.seen().iter().filter(|s| s.path() == held).count() == 2).await;
    let third = cli.ask(12, op::CHAT_POST_FILE, file_post(&rig.alpha, "C0AAAA", "row", "alpha-h", "files:no-such.png")).await;
    assert_eq!((code(&third), third.get("outcome")), ("busy", None), "{third}");
    fake.shutdown().await;
}

/// A fake that names the workspace and answers one file's `files.info` and URLs, the file shared in alpha's channel.
async fn files_slack(files: Files) -> FakeSlack {
    let files = Files { channels: vec!["C0AAAA".into()], ..files };
    FakeSlack::start(Identity::Trusted, move |seen, port| {
        if seen.path().ends_with("/auth.test") {
            return Reply::json(&json!({"ok": true, "team_id": "T0AAAA", "user_id": "U0BOT", "bot_id": "B0BOT"}));
        }
        files.reply(seen, port).unwrap_or_else(|| Reply::json(&json!({"ok": false, "error": "invalid_auth"})))
    })
    .await
}

fn file_req(row: &str, variant: &str) -> Value {
    json!({"target": target(row, "C0AAAA"), "file": "F0PIC", "variant": variant})
}

/// The requests a fetch makes: `files.info` and the file URLs (the service's own start is not counted).
fn fetches(fake: &FakeSlack) -> usize {
    fake.seen().iter().filter(|s| s.path() == "/api/files.info" || !s.path().starts_with("/api/")).count()
}

/// The reply payload to request `id` and its blob.
async fn reply_with_blob(conn: &mut Conn, id: u64) -> (Value, Option<Vec<u8>>) {
    loop {
        let (frame, blob) = codec::read_frame(&mut conn.crx).await.unwrap();
        if frame.kind == Kind::Res && frame.id == id {
            return (frame.payload, blob);
        }
    }
}

/// `chat.file` answers a picture's bytes as the frame's blob, its `len` their count; no thumbnail yet is `failed` (asked
/// again), a file over its bound or no picture is its final code; a stale row fetches nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_file_reply_carries_its_bytes_as_its_blob() {
    let fake = files_slack(Files::new()).await;
    let r = rig(&fake).await;
    let mut conn = connect(r.services.clone()).await;
    for (n, (variant, want)) in [("thumb", &b"\x89PNG thumbnail bytes"[..]), ("original", &b"\x89PNG original bytes"[..])].into_iter().enumerate() {
        let id = 10 + n as u64;
        conn.send(id, op::CHAT_FILE, file_req(&r.alpha, variant)).await;
        let (payload, blob) = reply_with_blob(&mut conn, id).await;
        assert_eq!(payload, json!({"mime": "image/png", "blob": {"len": want.len(), "mime": "image/png"}}), "{variant}");
        assert_eq!(blob.as_deref(), Some(want), "{variant}");
    }
    assert_eq!(fetches(&fake), 4, "one files.info and one GET for each");
    // Removed, then replaced under a new id: the old id fetches nothing.
    r.rows.remove_by_id(&r.alpha).unwrap();
    r.rows.insert(row("alpha", &r.folders[0]));
    let (payload, blob) = { conn.send(20, op::CHAT_FILE, file_req(&r.alpha, "thumb")).await; reply_with_blob(&mut conn, 20).await };
    assert_eq!((code(&payload), blob), ("stale_row", None), "{payload}");
    assert_eq!(fetches(&fake), 4, "a stale row reached Slack");
    fake.shutdown().await;

    // No thumbnail and an original over the bound: neither is fetched.
    let big = Files { thumb_keys: vec![], size: Some(sot_protocol::ops::MAX_CHAT_FILE as u64 + 1), ..Files::new() };
    let fake = files_slack(big).await;
    let r = rig(&fake).await;
    let mut conn = connect(r.services.clone()).await;
    assert_eq!(code(&conn.ask(30, op::CHAT_FILE, file_req(&r.alpha, "thumb")).await), "failed");
    assert_eq!(code(&conn.ask(31, op::CHAT_FILE, file_req(&r.alpha, "original")).await), "too_large");
    assert!(downloads_none(&fake), "a file over its bound was downloaded");
    fake.shutdown().await;

    let fake = files_slack(Files { mimetype: "application/pdf".into(), ..Files::new() }).await;
    let r = rig(&fake).await;
    let mut conn = connect(r.services.clone()).await;
    assert_eq!(code(&conn.ask(40, op::CHAT_FILE, file_req(&r.alpha, "original")).await), "not_an_image");
    assert!(downloads_none(&fake), "a PDF was downloaded");
    fake.shutdown().await;
}

fn downloads_none(fake: &FakeSlack) -> bool {
    fake.seen().iter().all(|s| s.path().starts_with("/api/"))
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
        (ApiError::NoPersonToken, ChatCode::NoUserToken),
    ] {
        assert_eq!(code_of(&e), want, "{e:?}");
    }
    // By the post's kind: `not_in_channel` is the app's on a row's post and the person's on theirs, and a code that names the
    // token is the person's user token only on a person's post.
    for (kind, why, want) in [
        (PostKind::Person, HttpError::Busy.code(), ChatCode::Busy),
        (PostKind::Row, HttpError::Busy.code(), ChatCode::Busy),
        (PostKind::Person, HttpError::Invalid.code(), ChatCode::BadRequest),
        (PostKind::Person, "destination", ChatCode::Failed),
        (PostKind::Person, "http_403", ChatCode::Failed),
        (PostKind::Person, "channel_not_found", ChatCode::Failed),
        (PostKind::Person, "not_in_channel", ChatCode::NotAMember),
        (PostKind::Row, "not_in_channel", ChatCode::Failed),
        (PostKind::Person, "invalid_auth", ChatCode::NoUserToken),
        (PostKind::Person, "token_revoked", ChatCode::NoUserToken),
        (PostKind::Person, "missing_scope", ChatCode::NoUserToken),
        (PostKind::Row, "invalid_auth", ChatCode::Failed),
        (PostKind::Row, "missing_scope", ChatCode::Failed),
    ] {
        assert_eq!(post_refusal_code(kind, why), want, "{kind:?} {why}");
    }
}

/// Slack's refusal of a person's post by what it means: `not_in_channel` is theirs to fix (`not_a_member`) and the token
/// stays, so the row still resolves mapped; a code naming the token forgets it, the post answers `no_user_token`, the next
/// resolve says so and the next post is refused with no `chat.postMessage` sent.
#[tokio::test(flavor = "multi_thread")]
async fn a_persons_post_slack_refuses_is_not_a_member_or_forgets_the_token() {
    let mode = Arc::new(AtomicU8::new(NOT_IN_CHANNEL));
    let fake = slack(&mode).await;
    let rig = rig(&fake).await;
    let mut conn = connect_window(rig.services.clone()).await;
    let posts = |fake: &FakeSlack| fake.seen().iter().filter(|s| s.path().ends_with("/chat.postMessage")).count();
    let refused = conn.ask(2, op::CHAT_POST, post(&rig.alpha, "C0AAAA")).await;
    assert_eq!((refused["outcome"].as_str(), refused["code"].as_str()), (Some("refused"), Some("not_a_member")), "{refused}");
    let resolved = conn.ask(3, op::CHAT_RESOLVE, json!({"row": rig.alpha})).await;
    assert_eq!(resolved["state"], "mapped", "the token is kept: {resolved}");

    mode.store(TOKEN_GONE, Ordering::SeqCst);
    let gone = conn.ask(4, op::CHAT_POST, post(&rig.alpha, "C0AAAA")).await;
    assert_eq!((gone["outcome"].as_str(), gone["code"].as_str()), (Some("refused"), Some("no_user_token")), "{gone}");
    let resolved = conn.ask(5, op::CHAT_RESOLVE, json!({"row": rig.alpha})).await;
    assert_eq!(resolved, json!({"state": "unconfigured", "reason": "no_user_token"}));
    let before = posts(&fake);
    let again = conn.ask(6, op::CHAT_POST, post(&rig.alpha, "C0AAAA")).await;
    assert_eq!(again["code"], "no_user_token", "{again}");
    assert_eq!(posts(&fake), before, "a forgotten token's next post was sent");
}
