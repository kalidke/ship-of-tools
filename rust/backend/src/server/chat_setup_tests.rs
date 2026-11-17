//! The setup ops (`chat.connect`, `chat.channels`, `chat.pair`) through real connections, against the exported fake
//! Slack: its tokens, its channels, its join and a live socket.

use super::chat_ops_tests::{code, connect, connect_window, running, starter, Conn};
use super::chat_watch_tests::{after_hello, message, stamp, until};
use super::*;
use crate::chat::setup::Verifier;
use crate::chat::tests::{row, Home, SELF};
use crate::chat::{Chat, Starter, Status, HUB_CHECK};
use crate::lifecycle::retire::Registry;
use serde_json::{json, Value};
use sot_chat::config::{self, CONFIG_FILE, SECRETS_FILE};
use sot_chat::delivery::Timing;
use sot_chat::fake_slack::{FakeSlack, Identity, Reply};
use sot_chat::service::Service;
use sot_protocol::ops::{ChatChannel, ChatTarget};
use std::sync::Mutex;

/// A channel of the fake workspace: id, name, private, whether the app is in it.
type Fact = (&'static str, &'static str, bool, bool);

/// What the fake Slack holds: its channels, what a join answers (`ok`, Slack's error code, or `500` for an answer that
/// is no acknowledgement), and what `auth.test` and `apps.connections.open` answer ([`answer`]).
struct Slack {
    channels: Mutex<Vec<Fact>>,
    join: Mutex<&'static str>,
    auth: Mutex<&'static str>,
    /// What `auth.test` answers a user token (`xoxp-`) with, and the workspace it names.
    user: Mutex<&'static str>,
    user_team: Mutex<&'static str>,
    open: Mutex<&'static str>,
    /// What `conversations.list` answers (a check's word, [`answer`]).
    list: Mutex<&'static str>,
    /// The app `bots.info` names as the bot's (`None`: the call is unknown) and the app the socket's hello names (`None`:
    /// none).
    bot_app: Mutex<Option<&'static str>>,
    hello_app: Mutex<Option<&'static str>>,
    feed: tokio::sync::mpsc::UnboundedSender<Value>,
}

/// A check's answer: `ok` is `good`, `drop` closes the connection unanswered, a number is that HTTP status, and any other
/// word is Slack's `ok: false` with it as the error code.
fn answer(how: &str, good: Reply) -> Reply {
    match how {
        "ok" => good,
        "drop" => Reply::Drop,
        status if status.parse::<u16>().is_ok() => Reply::Status { code: status.parse().unwrap(), headers: Vec::new(), body: Vec::new() },
        error => Reply::json(&json!({"ok": false, "error": error})),
    }
}

fn channel_json((id, name, private, member): Fact) -> Value {
    json!({"id": id, "name": name, "is_private": private, "is_member": member})
}

/// A workspace `T0AAAA` whose list shows a private channel only to a member, whose info shows any, and whose socket
/// says hello and then sends what `feed` is given.
async fn slack() -> (FakeSlack, Arc<Slack>) {
    let (feed, rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let state = Arc::new(Slack { channels: Mutex::new(Vec::new()), join: Mutex::new("ok"), auth: Mutex::new("ok"), user: Mutex::new("ok"), user_team: Mutex::new("T0AAAA"), open: Mutex::new("ok"), list: Mutex::new("ok"), bot_app: Mutex::new(None), hello_app: Mutex::new(None), feed });
    let (held, rx) = (state.clone(), Arc::new(tokio::sync::Mutex::new(rx)));
    let fake = FakeSlack::start(Identity::Trusted, move |seen, port| {
        let path = seen.path();
        let known = |id: &str| held.channels.lock().unwrap().iter().copied().find(|c| c.0 == id);
        let wanted = seen.param("channel").or_else(|| seen.json().and_then(|j| j["channel"].as_str().map(str::to_string))).unwrap_or_default();
        // Each token its own identity: the user token's is a person's, with no bot id.
        let by_user = seen.authorization.as_deref().is_some_and(|a| a.starts_with("Bearer xoxp-"));
        if path.ends_with("/auth.test") && by_user {
            let team = *held.user_team.lock().unwrap();
            answer(*held.user.lock().unwrap(), Reply::json(&json!({"ok": true, "team_id": team, "user_id": "U0PERSON"})))
        } else if path.ends_with("/auth.test") {
            answer(*held.auth.lock().unwrap(), Reply::json(&json!({"ok": true, "team_id": "T0AAAA", "user_id": "U0BOT", "bot_id": "B0BOT"})))
        } else if path.ends_with("/chat.postMessage") {
            Reply::json(&json!({"ok": true, "ts": "1700000999.000100"}))
        } else if path.ends_with("/apps.connections.open") {
            let url = format!("wss://wss-primary.slack.com:{port}/link/?ticket=sentinel-ticket");
            answer(*held.open.lock().unwrap(), Reply::json(&json!({"ok": true, "url": url})))
        } else if path.ends_with("/bots.info") && held.bot_app.lock().unwrap().is_some() {
            let app = held.bot_app.lock().unwrap().unwrap_or_default();
            Reply::json(&json!({"ok": true, "bot": {"id": "B0BOT", "app_id": app}}))
        } else if path.ends_with("/conversations.list") {
            let listed: Vec<Value> = held.channels.lock().unwrap().iter().filter(|c| !c.2 || c.3).copied().map(channel_json).collect();
            answer(*held.list.lock().unwrap(), Reply::json(&json!({"ok": true, "channels": listed, "response_metadata": {"next_cursor": ""}})))
        } else if path.ends_with("/conversations.info") {
            match known(&wanted) {
                Some(c) => Reply::json(&json!({"ok": true, "channel": channel_json(c)})),
                None => Reply::json(&json!({"ok": false, "error": "channel_not_found"})),
            }
        } else if path.ends_with("/conversations.join") {
            match *held.join.lock().unwrap() {
                "ok" => {
                    held.channels.lock().unwrap().iter_mut().filter(|c| c.0 == wanted).for_each(|c| c.3 = true);
                    Reply::json(&json!({"ok": true}))
                }
                "500" => Reply::Status { code: 500, headers: Vec::new(), body: Vec::new() },
                error => Reply::json(&json!({"ok": false, "error": error})),
            }
        } else if path.ends_with("/conversations.history") {
            Reply::json(&json!({"ok": true, "messages": [], "has_more": false}))
        } else if path.ends_with("/users.info") {
            Reply::json(&json!({"ok": true, "user": {"profile": {"display_name": "Name"}}}))
        } else if seen.upgrade {
            let (rx, held) = (rx.clone(), held.clone());
            Reply::socket(move |mut peer| {
                let (rx, held) = (rx.clone(), held.clone());
                async move {
                    let mut said = json!({"type": "hello", "num_connections": 1});
                    if let Some(app) = *held.hello_app.lock().unwrap() {
                        said["connection_info"] = json!({"app_id": app});
                    }
                    peer.send(&said).await;
                    let mut rx = rx.lock().await;
                    // Until the client goes, so a reload's socket takes the feed.
                    loop {
                        let envelope = tokio::select! {
                            e = rx.recv() => e,
                            () = peer.drain() => None,
                        };
                        let Some(envelope) = envelope else { break };
                        peer.send(&envelope).await;
                        let _ = peer.recv(std::time::Duration::from_secs(10)).await;
                    }
                }
            })
        } else {
            Reply::json(&json!({"ok": false, "error": "unknown_method"}))
        }
    })
    .await;
    (fake, state)
}

fn fresh() -> String {
    config::fresh("T0AAAA").unwrap()
}

fn public(id: &'static str, member: bool) -> Fact {
    (id, "general", false, member)
}

struct World {
    home: Home,
    fake: FakeSlack,
    slack: Arc<Slack>,
    services: Services,
    rows: Workspaces,
    /// The ids of rows alpha and beta, and their folders.
    ids: [String; 2],
    folders: [std::path::PathBuf; 2],
    _root: tempfile::TempDir,
}

/// A hub whose rows alpha and beta map to nothing, booted on `chat_toml` and the secrets (`None`: no chat file at all, so
/// the chat is off), the fake holding `channels`.
async fn world(chat_toml: Option<String>, channels: &[Fact]) -> World {
    let home = Home::new(Some(SELF));
    if let Some(text) = chat_toml {
        home.write(CONFIG_FILE, &text);
        home.secrets();
    }
    world_on(home, channels, None, None).await
}

/// [`world`] on a home as the test made it, with the fake's starter and checks unless the test gives its own.
async fn world_on(home: Home, channels: &[Fact], start: Option<Starter<Service>>, verify: Option<Verifier>) -> World {
    let (fake, slack) = slack().await;
    slack.channels.lock().unwrap().extend_from_slice(channels);
    let folders = [home.folder("a"), home.folder("b")];
    let rows = Workspaces::new();
    let ids = [rows.insert(row("alpha", &folders[0])).workspace_id.clone(), rows.insert(row("beta", &folders[1])).workspace_id.clone()];
    let (start, verify) = (start.unwrap_or_else(|| starter(&fake)), verify.unwrap_or_else(|| Verifier::at(&fake)));
    let chat = Chat::boot(&home.env, &rows, &|p| config::read_file(p), &Registry::new(), start, verify, HUB_CHECK, Timing::PRODUCT);
    running(&chat).await;
    let root = tempfile::tempdir().unwrap();
    let mut services = Services::for_test("t", root.path());
    services.chat = chat;
    services.workspaces = rows.clone();
    World { home, fake, slack, services, rows, ids, folders, _root: root }
}

impl World {
    fn calls(&self, name: &str) -> usize {
        self.fake.seen().iter().filter(|s| s.path().ends_with(name)).count()
    }

    /// The services this world has started: one `auth.test` under the bot's token for each (a connect's check of the
    /// user token is another bearer's).
    fn starts(&self) -> usize {
        let by_bot = |s: &&sot_chat::fake_slack::Seen| s.authorization.as_deref().is_some_and(|a| a.starts_with("Bearer xoxb-"));
        self.fake.seen().iter().filter(|s| s.path().ends_with("/auth.test")).filter(by_bot).count()
    }

    fn file(&self) -> String {
        std::fs::read_to_string(self.home.config().join(CONFIG_FILE)).unwrap()
    }

    /// The mappings of `chat.toml` as (folder, channel).
    fn mappings(&self) -> Vec<(String, String)> {
        let t: toml::Table = self.file().parse().unwrap();
        let field = |m: &toml::Value, k: &str| m[k].as_str().unwrap().to_string();
        t.get("mapping").and_then(toml::Value::as_array).map(|a| a.iter().map(|m| (field(m, "folder"), field(m, "channel"))).collect()).unwrap_or_default()
    }

    fn mapped(&self, folder: usize, channel: &str) -> (String, String) {
        (self.folders[folder].to_string_lossy().into_owned(), channel.to_string())
    }

    fn target(&self, row: usize, channel: &str) -> ChatTarget {
        ChatTarget { row: self.ids[row].clone(), workspace: "T0AAAA".into(), channel: ChatChannel::try_from(channel.to_string()).unwrap() }
    }
}

impl Conn {
    async fn pair(&mut self, row: &str, channel: Option<&str>) -> Value {
        self.ask(10, op::CHAT_PAIR, json!({"row": row, "channel": channel})).await
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_window_pairs_a_row_with_a_public_channel_the_app_joins_and_watches_it_at_once() {
    let w = world(Some(fresh()), &[public("C0PUB", false)]).await;
    let mut conn = connect_window(w.services.clone()).await;
    assert_eq!(conn.pair(&w.ids[0], Some("C0PUB")).await, json!({}));
    assert_eq!((w.calls("/conversations.join"), w.starts(), w.services.chat.status()), (1, 2, Status::Running), "joined once, reloaded once");
    assert_eq!(w.mappings(), [w.mapped(0, "C0PUB")]);
    assert!(w.file().contains("C0PUB"), "the channel is listed");
    let resolved = conn.ask(11, op::CHAT_RESOLVE, json!({"row": w.ids[0]})).await;
    assert_eq!((resolved["state"].as_str(), resolved["channel"].as_str()), (Some("mapped"), Some("C0PUB")), "{resolved}");
    let watched = conn.ask(12, op::CHAT_WATCH, json!({"target": w.target(0, "C0PUB")})).await;
    assert!(matches!(watched["state"].as_str(), Some("pending" | "complete")), "{watched}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_private_channel_the_app_is_not_in_is_refused_unjoined() {
    let w = world(Some(fresh()), &[("G0PRIV", "secret", true, false)]).await;
    let (before, mut conn) = (w.file(), connect_window(w.services.clone()).await);
    assert_eq!(code(&conn.pair(&w.ids[0], Some("G0PRIV")).await), "not_in_channel");
    assert_eq!((w.calls("/conversations.join"), w.starts(), w.file()), (0, 1, before), "nothing joined, written or reloaded");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_join_without_the_scope_is_not_in_channel() {
    let w = world(Some(fresh()), &[public("C0PUB", false)]).await;
    let (before, mut conn) = (w.file(), connect_window(w.services.clone()).await);
    *w.slack.join.lock().unwrap() = "missing_scope";
    assert_eq!(code(&conn.pair(&w.ids[0], Some("C0PUB")).await), "not_in_channel");
    // A join with no acknowledgement is settled by reading the channel, and is sent once.
    *w.slack.join.lock().unwrap() = "500";
    assert_eq!(code(&conn.pair(&w.ids[0], Some("C0PUB")).await), "not_in_channel");
    assert_eq!((w.calls("/conversations.join"), w.starts(), w.file()), (2, 1, before), "one join each, nothing written or reloaded");
}

#[tokio::test(flavor = "multi_thread")]
async fn unpairing_removes_the_mapping_and_the_channel_and_the_row_reads_unmapped() {
    let w = world(Some(fresh()), &[public("C0PUB", true)]).await;
    let mut conn = connect_window(w.services.clone()).await;
    assert_eq!(conn.pair(&w.ids[0], Some("C0PUB")).await, json!({}));
    assert_eq!(conn.pair(&w.ids[0], None).await, json!({}));
    assert_eq!((w.mappings(), w.file().contains("C0PUB")), (vec![], false), "{}", w.file());
    assert_eq!(conn.ask(11, op::CHAT_RESOLVE, json!({"row": w.ids[0]})).await["state"], "unmapped");
    let watched = conn.ask(12, op::CHAT_WATCH, json!({"target": w.target(0, "C0PUB")})).await;
    assert_eq!(code(&watched), "unmapped");
    assert_eq!((w.calls("/conversations.join"), w.services.chat.status()), (0, Status::Running));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_channel_of_another_existing_folder_is_taken_and_one_of_a_missing_folder_is_taken_over() {
    let w = world(Some(fresh()), &[public("C0PUB", true)]).await;
    let mut conn = connect_window(w.services.clone()).await;
    assert_eq!(conn.pair(&w.ids[0], Some("C0PUB")).await, json!({}));
    let before = w.file();
    assert_eq!(code(&conn.pair(&w.ids[1], Some("C0PUB")).await), "channel_taken");
    assert_eq!((w.file(), w.starts()), (before, 2), "a refused pairing writes and reloads nothing");
    std::fs::remove_dir(&w.folders[0]).unwrap();
    assert_eq!(conn.pair(&w.ids[1], Some("C0PUB")).await, json!({}));
    assert_eq!(w.mappings(), [w.mapped(1, "C0PUB")], "the missing folder's mapping was taken over");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pair_that_changes_nothing_writes_and_reloads_nothing() {
    let w = world(Some(fresh()), &[public("C0PUB", true)]).await;
    let mut conn = connect_window(w.services.clone()).await;
    assert_eq!(conn.pair(&w.ids[0], Some("C0PUB")).await, json!({}));
    // A rewrite would drop this comment.
    let kept = format!("{}\n# kept\n", w.file());
    w.home.write(CONFIG_FILE, &kept);
    assert_eq!(conn.pair(&w.ids[0], Some("C0PUB")).await, json!({}));
    assert_eq!((w.file(), w.starts()), (kept, 2));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delivering_files_new_mapping_floors_at_the_clock() {
    let w = world(Some(format!("{}mail_after = \"1.000000\"\n", fresh())), &[public("C0PUB", true)]).await;
    let mut conn = connect_window(w.services.clone()).await;
    let before = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    assert_eq!(conn.pair(&w.ids[0], Some("C0PUB")).await, json!({}));
    let after = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let t: toml::Table = w.file().parse().unwrap();
    let floor = t["mapping"][0]["mail_after"].as_str().unwrap();
    let secs: u64 = floor.split_once('.').unwrap().0.parse().unwrap();
    assert!((before..=after).contains(&secs), "the floor {floor} is the clock's, not the file's");
    assert_eq!(t["mail_after"].as_str(), Some("1.000000"), "the hub's own floor stays");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hand_edited_file_the_load_refuses_is_never_rewritten() {
    let w = world(Some(fresh()), &[public("C0PUB", true)]).await;
    let mut conn = connect_window(w.services.clone()).await;
    let refused = "workspace = \n# a hand edit in progress\n";
    w.home.write(CONFIG_FILE, refused);
    assert_eq!(code(&conn.pair(&w.ids[0], Some("C0PUB")).await), "not_saved");
    assert_eq!((w.file(), w.starts()), (refused.to_string(), 1), "nothing written or reloaded");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hub_deposed_since_its_boot_writes_nothing() {
    let w = world(Some(fresh()), &[public("C0PUB", true)]).await;
    let (before, mut conn) = (w.file(), connect_window(w.services.clone()).await);
    // It steps down within a minute; until then a pairing asks the topology first.
    w.home.list("other-box");
    assert_eq!(code(&conn.pair(&w.ids[0], Some("C0PUB")).await), "unavailable");
    assert_eq!((w.file(), w.starts()), (before, 1));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_channel_list_needs_a_running_service_and_a_window() {
    // A hub with no chat file: nothing runs, so a window's request is `unavailable`.
    let off = world(None, &[]).await;
    let mut conn = connect_window(off.services.clone()).await;
    assert_eq!(code(&conn.ask(2, op::CHAT_CHANNELS, json!({})).await), "unavailable");
    assert_eq!(code(&conn.pair(&off.ids[0], Some("C0PUB")).await), "unavailable");
    assert_eq!(off.calls("/conversations.list") + off.calls("/conversations.info"), 0, "no call without a service");

    let channels = [public("C0PUB", false), ("G0PRIV", "secret", true, false), ("G0IN", "inside", true, true)];
    let w = world(Some(fresh()), &channels).await;
    let mut other = connect(w.services.clone()).await;
    assert_eq!(code(&other.ask(2, op::CHAT_CHANNELS, json!({})).await), "bad_request");
    assert_eq!(code(&other.pair(&w.ids[0], Some("C0PUB")).await), "bad_request");
    assert_eq!(w.calls("/conversations.list") + w.calls("/conversations.info"), 0, "a row's connection reached no call");
    let mut conn = connect_window(w.services.clone()).await;
    let listed = conn.ask(2, op::CHAT_CHANNELS, json!({})).await;
    assert_eq!(
        listed,
        json!({"channels": [
            {"id": "C0PUB", "name": "general", "private": false, "member": false},
            {"id": "G0IN", "name": "inside", "private": true, "member": true},
        ]}),
        "a private channel the app is not in is not listed"
    );
}

// ---- the connect ----

/// Fixture tokens are made at run time, so no token literal is in the tree.
fn bot(body: &str) -> String {
    format!("xoxb-{body}-{}", "4".repeat(12))
}

fn app(body: &str) -> String {
    format!("xapp-{body}-{}", "5".repeat(12))
}

fn user(body: &str) -> String {
    format!("xoxp-{body}-{}", "6".repeat(12))
}

/// A connect of the three tokens, the user token `user("1")`.
fn connect_req(bot: &str, app: &str) -> Value {
    connect_with(bot, &user("1"), app)
}

fn connect_with(bot: &str, user: &str, app: &str) -> Value {
    json!({"bot_token": bot, "user_token": user, "app_token": app})
}

/// Every file of the config folder and its bytes.
fn files(home: &Home) -> Vec<(String, Vec<u8>)> {
    let mut all: Vec<_> = std::fs::read_dir(home.config())
        .unwrap()
        .map(|e| e.unwrap())
        .map(|e| (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap_or_default()))
        .collect();
    all.sort();
    all
}

/// The secrets file holds exactly the four keys, with these tokens; it says so without printing them.
fn secrets_hold(home: &Home, bot: &str, user: &str, app: &str) -> bool {
    let text = std::fs::read_to_string(home.config().join(SECRETS_FILE)).unwrap();
    let t: toml::Table = text.parse().unwrap();
    t.len() == 4
        && t["workspace"].as_str() == Some("T0AAAA")
        && t["bot_token"].as_str() == Some(bot)
        && t["user_token"].as_str() == Some(user)
        && t["app_token"].as_str() == Some(app)
}

/// One capture's bytes, shared by every thread that logs into it.
struct Sink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Sink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Runs `test` on a runtime of its own whose every thread, its blocking pool included, logs into one capture with this
/// thread's, and answers what was logged: a line from any part of a job is seen, not only a task's (`test_log`'s
/// capture is one thread's).
fn logged<F: std::future::Future<Output = ()>>(test: impl FnOnce() -> F) -> String {
    let out = Arc::new(Mutex::new(Vec::new()));
    let sink = out.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || Sink(sink.clone()))
        .with_ansi(false)
        .without_time()
        .with_max_level(tracing::level_filters::LevelFilter::TRACE)
        .finish();
    let _here = sot_platform::test_log::install(subscriber);
    let every = tracing::dispatcher::get_default(|d| d.clone());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .on_thread_start(move || std::mem::forget(tracing::dispatcher::set_default(&every)))
        .build()
        .unwrap();
    runtime.block_on(test());
    runtime.shutdown_timeout(std::time::Duration::from_secs(5));
    let text = String::from_utf8_lossy(&out.lock().unwrap()).into_owned();
    text
}

/// A log or a reply holds none of these token bodies; the message names the place, never the text.
fn holds_no_token(what: &str, text: &str, bodies: &[&str]) {
    for body in bodies {
        assert!(!text.contains(body), "{what} holds a token's bytes");
    }
}

/// From a hub with no chat file, a window connects with three tokens, lists the channels, pairs its
/// row, watches the channel and gets a socket post as `chat.changed`; afterwards the secrets are the four keys and
/// owner-only, `chat.toml` loads with the mapping, and no reply, event or log line of any thread held a token's bytes.
#[test]
fn from_nothing_a_window_connects_pairs_and_reads_its_channel() {
    let bodies = ["6c6d6e6f", "7a7b7c7d", "5f5e5d5c"];
    let mut seen = Vec::new();
    let text = logged(|| async {
        let w = world(None, &[public("C0PUB", false)]).await;
        let chat = w.services.chat.clone();
        assert_eq!(chat.status(), Status::Refused(config::ConfigError::Missing));
        let (b, a) = (bot(bodies[0]), app(bodies[1]));
        let mut conn = connect_window(w.services.clone()).await;
        let u = user(bodies[2]);
        let connected = conn.ask(2, op::CHAT_CONNECT, connect_with(&b, &u, &a)).await;
        assert_eq!(connected, json!({"workspace": "T0AAAA"}));
        assert_eq!(chat.status(), Status::Running);
        assert_eq!(w.file(), fresh(), "a connect with no chat.toml writes the fresh one");

        let listed = conn.ask(3, op::CHAT_CHANNELS, json!({})).await;
        assert_eq!(listed["channels"][0]["id"], "C0PUB", "{listed}");
        let paired = conn.pair(&w.ids[0], Some("C0PUB")).await;
        assert_eq!(paired, json!({}));
        after_hello(&chat, &w.rows, &w.target(0, "C0PUB")).await;
        let watched = conn.ask(4, op::CHAT_WATCH, json!({"target": w.target(0, "C0PUB")})).await;
        assert!(matches!(watched["state"].as_str(), Some("pending" | "complete")), "{watched}");
        let hub = chat.hub().clone();
        let probe = hub.connect();
        until("the snapshot", || probe.conn.state("T0AAAA", "C0PUB") == sot_protocol::ops::ChatWatchState::Complete).await;
        w.slack.feed.send(message("C0PUB", 1)).unwrap();
        let put = loop {
            let (frame, _) = codec::read_frame(&mut conn.crx).await.unwrap();
            if frame.kind == Kind::Evt && frame.op == op::CHAT_CHANGED {
                break frame.payload;
            }
        };
        assert_eq!(put["put"][0]["ts"], stamp(1).as_str(), "{put}");

        let dir = w.home.config();
        assert!(secrets_hold(&w.home, &b, &u, &a), "the secrets file holds other keys or tokens");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dir.join(SECRETS_FILE)).unwrap().permissions().mode() & 0o077, 0, "the secrets are the owner's alone");
        }
        // Through the daemon's own canonicalizer, so the spelling compared is the one the daemon writes and loads.
        let loaded = config::Config::load(&dir, &|p| config::read_file(p), &crate::chat::canon_of).unwrap();
        assert_eq!(loaded.mappings().collect::<Vec<_>>(), [(w.folders[0].to_str().unwrap(), "T0AAAA", "C0PUB")]);
        seen.extend([connected, listed, paired, watched, put].map(|v| v.to_string()));
    });
    assert!(text.contains("saved the tokens"), "the capture saw no connect");
    holds_no_token("the daemon's log", &text, &bodies);
    for (n, reply) in seen.iter().enumerate() {
        holds_no_token(&format!("reply or event {n}"), reply, &bodies);
    }
}

/// Each answer of the two checks against the code it gives, and nothing written for any: a token's form, Slack's
/// answers that name the token (a refusal), and Slack's own state or the transport (`failed`, never a refusal). No line
/// of any thread and no reply holds a token's bytes.
#[test]
fn each_answer_of_the_two_checks_has_its_code_and_writes_nothing() {
    let bodies = ["8a8b8c8d", "9e9f9a9b"];
    let mut replies = Vec::new();
    let text = logged(|| async {
        let w = world(None, &[]).await;
        let before = files(&w.home);
        let (b, a) = (bot(bodies[0]), app(bodies[1]));
        let mut conn = connect_window(w.services.clone()).await;
        // The forms: refused before any call.
        for (what, req, want) in [
            ("bot form", connect_req("nope", &a), "bot_token_refused"),
            ("bot charset", connect_req(&format!("{b}!"), &a), "bot_token_refused"),
            ("app form", connect_req(&b, &bot("2")), "app_token_refused"),
            ("user form", connect_with(&b, "nope", &a), "user_token_refused"),
            ("no user token", json!({"bot_token": b, "app_token": a}), "bad_request"),
            ("undecodable", json!({"bot_token": b, "user_token": user("1"), "app_token": 7}), "bad_request"),
        ] {
            let reply = conn.ask(10, op::CHAT_CONNECT, req).await;
            assert_eq!(code(&reply), want, "{what}");
            replies.push(reply.to_string());
        }
        assert!(w.fake.seen().is_empty(), "a form refusal called Slack");
        let checks: [(&Mutex<&'static str>, &[(&'static str, &str)]); 2] = [
            (&w.slack.auth, &[
                ("invalid_auth", "bot_token_refused"),
                ("token_revoked", "bot_token_refused"),
                ("internal_error", "failed"),
                ("service_unavailable", "failed"),
                ("ratelimited", "failed"),
                ("503", "failed"),
                ("429", "failed"),
                ("drop", "failed"),
            ]),
            (&w.slack.open, &[
                ("invalid_auth", "app_token_refused"),
                ("missing_scope", "app_token_refused"),
                ("401", "app_token_refused"),
                ("internal_error", "failed"),
                ("ratelimited", "failed"),
                ("503", "failed"),
                ("429", "failed"),
                ("drop", "failed"),
            ]),
        ];
        for (n, (check, cases)) in checks.into_iter().enumerate() {
            for (how, want) in cases {
                *check.lock().unwrap() = how;
                let reply = conn.ask(20, op::CHAT_CONNECT, connect_req(&b, &a)).await;
                assert_eq!(code(&reply), *want, "check {n} answered {how}");
                assert_eq!(files(&w.home), before, "check {n} answered {how}: the config folder changed");
                replies.push(reply.to_string());
            }
            *check.lock().unwrap() = "ok";
        }
        assert_eq!(w.services.chat.status(), Status::Refused(config::ConfigError::Missing));
    });
    holds_no_token("the daemon's log", &text, &bodies);
    for (n, reply) in replies.iter().enumerate() {
        holds_no_token(&format!("reply {n}"), reply, &bodies);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connect_from_a_row_or_on_a_computer_that_is_not_the_hub_writes_nothing() {
    // A row's connection (role `cli`) is refused before anything is read.
    let w = world(None, &[]).await;
    let before = files(&w.home);
    let mut conn = connect(w.services.clone()).await;
    assert_eq!(code(&conn.ask(2, op::CHAT_CONNECT, connect_req(&bot("1"), &app("1"))).await), "bad_request");
    assert_eq!(files(&w.home), before);
    // A hub deposed since its boot: the topology asked at the op names another computer.
    w.home.list("elsewhere");
    let deposed = files(&w.home);
    let mut window = connect_window(w.services.clone()).await;
    assert_eq!(code(&window.ask(3, op::CHAT_CONNECT, connect_req(&bot("1"), &app("1"))).await), "unavailable");
    assert_eq!(files(&w.home), deposed);
    assert!(w.fake.seen().is_empty(), "a refused connect reached Slack");
    // Not the hub at its boot.
    let other = world_on(Home::new(Some("elsewhere")), &[], None, None).await;
    let before = files(&other.home);
    let mut window = connect_window(other.services.clone()).await;
    assert_eq!(code(&window.ask(4, op::CHAT_CONNECT, connect_req(&bot("1"), &app("1"))).await), "unavailable");
    assert_eq!(files(&other.home), before);
    assert!(other.fake.seen().is_empty(), "a refused connect reached Slack");
}

/// A retirement that begins while Slack is asked: the connect finds the chat closed under the setup lock and writes
/// nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_retirement_that_begins_while_slack_is_asked_writes_nothing() {
    let (entered, gate) = (Arc::new(tokio::sync::Notify::new()), Arc::new(tokio::sync::Notify::new()));
    let (fake_for_checks, _held) = slack().await;
    let Verifier { identify, app: open } = Verifier::at(&fake_for_checks);
    let (e, g) = (entered.clone(), gate.clone());
    // Only the first check (the bot's) is held: the connect asks the user token's after it.
    let first = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let held_checks = Verifier {
        identify: Box::new(move |bot| {
            let (asked, e, g) = (identify(bot), e.clone(), g.clone());
            let held = first.swap(false, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async move {
                if held {
                    e.notify_one();
                    g.notified().await;
                }
                asked.await
            })
        }),
        app: open,
    };
    let w = world_on(Home::new(Some(SELF)), &[], None, Some(held_checks)).await;
    let before = files(&w.home);
    let mut conn = connect_window(w.services.clone()).await;
    conn.send(2, op::CHAT_CONNECT, connect_req(&bot("1"), &app("1"))).await;
    entered.notified().await;
    crate::lifecycle::retire::Participant::request_stop(&*w.services.chat);
    gate.notify_one();
    assert_eq!(code(&conn.reply(2).await), "unavailable");
    assert_eq!(files(&w.home), before, "a connect wrote for a daemon that was going away");
}

/// A write that fails is `not_saved`, never a reply that says the tokens were saved.
#[tokio::test(flavor = "multi_thread")]
async fn a_write_that_fails_is_not_saved() {
    let w = world(None, &[]).await;
    // A folder where the secrets file goes: the write's rename onto it fails.
    std::fs::create_dir(w.home.config().join(SECRETS_FILE)).unwrap();
    let before = files(&w.home);
    let mut conn = connect_window(w.services.clone()).await;
    assert_eq!(code(&conn.ask(2, op::CHAT_CONNECT, connect_req(&bot("1"), &app("1"))).await), "not_saved");
    assert_eq!(files(&w.home), before, "a failed connect wrote chat.toml");
    assert_eq!(w.services.chat.status(), Status::Refused(config::ConfigError::Missing));
}

/// The reply means saved and the reload's wait ended, not running: a start that fails still replies the workspace, and
/// `chat.resolve` then says why the chat is off.
#[tokio::test(flavor = "multi_thread")]
async fn a_connect_whose_start_fails_is_saved_and_resolve_says_start_failed() {
    let failing: Starter<Service> = Box::new(|_, _, _| Box::pin(async { Err("down") }));
    let w = world_on(Home::new(Some(SELF)), &[], Some(failing), None).await;
    let (b, a) = (bot("3"), app("3"));
    let mut conn = connect_window(w.services.clone()).await;
    assert_eq!(conn.ask(2, op::CHAT_CONNECT, connect_req(&b, &a)).await, json!({"workspace": "T0AAAA"}));
    assert!(secrets_hold(&w.home, &b, &user("1"), &a), "the tokens were not saved");
    assert_eq!(w.services.chat.status(), Status::Failed("down"));
    let resolved = conn.ask(3, op::CHAT_RESOLVE, json!({"row": w.ids[0]})).await;
    assert_eq!(resolved, json!({"state": "unconfigured", "reason": "start_failed"}));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connect_of_another_workspace_adds_it() {
    let home = Home::new(Some(SELF));
    home.write(CONFIG_FILE, "workspace = \"T0OTHER\"\nchannels = []\n");
    let w = world_on(home, &[], None, None).await;
    let before = files(&w.home);
    let mut conn = connect_window(w.services.clone()).await;
    let (b, u, a) = (bot("1"), user("1"), app("1"));
    assert_eq!(conn.ask(2, op::CHAT_CONNECT, connect_with(&b, &u, &a)).await, json!({"workspace": "T0AAAA"}));
    // `chat.toml` and every file that was there are as they were; the new workspace's tokens are its own file.
    let after = files(&w.home);
    let named = "chat-secrets-T0AAAA.toml";
    assert_eq!(after.iter().filter(|(n, _)| n != named).collect::<Vec<_>>(), before.iter().collect::<Vec<_>>());
    let t: toml::Table = std::fs::read_to_string(w.home.config().join(named)).unwrap().parse().unwrap();
    assert_eq!((t.len(), t["workspace"].as_str(), t["bot_token"].as_str()), (4, Some("T0AAAA"), Some(&*b)));
    assert_eq!(w.services.chat.status_of("T0AAAA"), Status::Running);
    // A chat.toml the load refuses is never rewritten from the window.
    w.home.write(CONFIG_FILE, "workspace = [[[");
    let before = files(&w.home);
    assert_eq!(code(&conn.ask(3, op::CHAT_CONNECT, connect_req(&bot("1"), &app("1"))).await), "not_saved");
    assert_eq!(files(&w.home), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn resolve_says_unconfigured_for_each_reason_a_connect_fixes() {
    let resolve = |w: World| async move {
        // The whole world, so its temporary folders outlive the request (a block captures only the fields it names).
        let w = w;
        let mut conn = connect_window(w.services.clone()).await;
        conn.ask(2, op::CHAT_RESOLVE, json!({"row": w.ids[0]})).await
    };
    let unconfigured = |reason: &str| json!({"state": "unconfigured", "reason": reason});
    // No chat.toml.
    assert_eq!(resolve(world(None, &[]).await).await, unconfigured("not_connected"));
    // chat.toml and no secrets.
    let home = Home::new(Some(SELF));
    home.write(CONFIG_FILE, &fresh());
    assert_eq!(resolve(world_on(home, &[], None, None).await).await, unconfigured("not_connected"));
    // Secrets the load refuses.
    let home = Home::new(Some(SELF));
    home.write(CONFIG_FILE, &fresh());
    home.write(SECRETS_FILE, "workspace = \"T0AAAA\"\nbot_token = \"nope\"\napp_token = \"nope\"\n");
    assert_eq!(resolve(world_on(home, &[], None, None).await).await, unconfigured("tokens_refused"));
    // A start that failed.
    let home = Home::new(Some(SELF));
    home.configured();
    let failing: Starter<Service> = Box::new(|_, _, _| Box::pin(async { Err("down") }));
    // A mapped row's answer names its workspace, the one whose start failed.
    assert_eq!(resolve(world_on(home, &[], Some(failing), None).await).await, json!({"state": "unconfigured", "reason": "start_failed", "workspace": "T0AAAA"}));
    // A refused chat.toml stays `invalid`: a connect never rewrites it.
    let home = Home::new(Some(SELF));
    home.write(CONFIG_FILE, "workspace = [[[");
    assert_eq!(resolve(world_on(home, &[], None, None).await).await, json!({"state": "invalid"}));
    // The row before this: a row that is gone is `stale_row`, whatever the chat's state.
    let w = world(None, &[]).await;
    w.rows.remove_by_id(&w.ids[0]);
    assert_eq!(code(&resolve(w).await), "stale_row");
}

/// A service that ended on its own is off with its code, and `chat.resolve` says why: another app's token is `wrong_app`,
/// a token Slack refused (the app's at the socket's open, the bot's on a call) is `tokens_refused`.
#[tokio::test(flavor = "multi_thread")]
async fn resolve_says_why_a_service_ended() {
    for ended in ["wrong_app", "app_token_refused", "bot_token_refused"] {
        let home = Home::new(Some(SELF));
        home.configured();
        let w = world_on(home, &[public("C0PUB", true)], None, None).await;
        assert_eq!(w.services.chat.status(), Status::Running, "{ended}");
        let mut conn = connect_window(w.services.clone()).await;
        let chat = w.services.chat.clone();
        match ended {
            "wrong_app" => {
                *w.slack.bot_app.lock().unwrap() = Some("A0OWN");
                *w.slack.hello_app.lock().unwrap() = Some("A0OTHER");
                chat.reload(&chat.setup().await, &w.rows).await;
            }
            "app_token_refused" => {
                *w.slack.open.lock().unwrap() = "invalid_auth";
                chat.reload(&chat.setup().await, &w.rows).await;
            }
            _ => {
                *w.slack.list.lock().unwrap() = "token_revoked";
                conn.ask(2, op::CHAT_CHANNELS, json!({})).await;
            }
        }
        until(&format!("the chat off with {ended}"), || chat.status() == Status::Failed(ended)).await;
        let reason = if ended == "wrong_app" { "wrong_app" } else { "tokens_refused" };
        let resolved = conn.ask(3, op::CHAT_RESOLVE, json!({"row": w.ids[0]})).await;
        assert_eq!(resolved, json!({"state": "unconfigured", "reason": reason, "workspace": "T0AAAA"}), "{ended}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connect_while_running_replaces_the_tokens_and_restarts() {
    let home = Home::new(Some(SELF));
    home.configured();
    let w = world_on(home, &[], None, None).await;
    assert_eq!(w.services.chat.status(), Status::Running);
    let toml = w.file();
    let (b, a) = (bot("2"), app("2"));
    let auth = |token: &str| w.fake.seen().iter().filter(|x| x.path().ends_with("/auth.test") && x.authorization.as_deref() == Some(&*format!("Bearer {token}"))).count();
    let mut conn = connect_window(w.services.clone()).await;
    assert_eq!(conn.ask(2, op::CHAT_CONNECT, connect_req(&b, &a)).await, json!({"workspace": "T0AAAA"}));
    assert_eq!(w.services.chat.status(), Status::Running, "the reloaded chat is not running");
    assert!(secrets_hold(&w.home, &b, &user("1"), &a), "the secrets file does not hold the new tokens");
    assert_eq!(w.file(), toml, "a connect rewrote an admitted chat.toml");
    assert_eq!(auth(&b), 2, "the new bot token: the connect's identify and the restarted service's own auth.test");
    let resolved = conn.ask(3, op::CHAT_RESOLVE, json!({"row": w.ids[0]})).await;
    assert_eq!((resolved["state"].as_str(), resolved["channel"].as_str()), (Some("mapped"), Some("C0AAAA")), "{resolved}");
}

/// The user token's own check, in the connect's order after the bot's: its absence and its form before any call, Slack's
/// answers that name the token (and another workspace) refuse it, Slack's state and the transport are `failed`, and
/// nothing is written for any.
#[test]
fn each_answer_of_the_user_tokens_check_has_its_code_and_writes_nothing() {
    let bodies = ["1a1b1c1d", "2a2b2c2d", "3a3b3c3d"];
    let mut replies = Vec::new();
    let text = logged(|| async {
        let w = world(None, &[]).await;
        let before = files(&w.home);
        let (b, u, a) = (bot(bodies[0]), user(bodies[1]), app(bodies[2]));
        let mut conn = connect_window(w.services.clone()).await;
        for (what, req, want) in [
            ("absent", json!({"bot_token": b, "app_token": a}), "bad_request"),
            ("form", connect_with(&b, "nope", &a), "user_token_refused"),
            ("the bot's form", connect_with(&b, &b, &a), "user_token_refused"),
            ("charset", connect_with(&b, &format!("{u}!"), &a), "user_token_refused"),
        ] {
            let reply = conn.ask(10, op::CHAT_CONNECT, req).await;
            assert_eq!(code(&reply), want, "{what}");
            assert_eq!(files(&w.home), before, "{what}: the config folder changed");
            replies.push(reply.to_string());
        }
        assert!(w.fake.seen().is_empty(), "a form refusal called Slack");
        for (how, team, want) in [
            ("invalid_auth", "T0AAAA", "user_token_refused"),
            ("token_revoked", "T0AAAA", "user_token_refused"),
            ("ok", "T0OTHER", "user_token_refused"),
            ("internal_error", "T0AAAA", "failed"),
            ("ratelimited", "T0AAAA", "failed"),
            ("503", "T0AAAA", "failed"),
            ("drop", "T0AAAA", "failed"),
        ] {
            (*w.slack.user.lock().unwrap(), *w.slack.user_team.lock().unwrap()) = (how, team);
            let reply = conn.ask(20, op::CHAT_CONNECT, connect_with(&b, &u, &a)).await;
            assert_eq!(code(&reply), want, "the user token's check answered {how} for {team}");
            assert_eq!(files(&w.home), before, "{how} for {team}: the config folder changed");
            replies.push(reply.to_string());
        }
        assert_eq!(w.services.chat.status(), Status::Refused(config::ConfigError::Missing));
    });
    holds_no_token("the daemon's log", &text, &bodies);
    for (n, reply) in replies.iter().enumerate() {
        holds_no_token(&format!("reply {n}"), reply, &bodies);
    }
}

/// A hub whose secrets file was written before the user token (three keys) runs for rows: a row's connection resolves its
/// row as mapped and posts (the route `sot-fe chat post` takes), a person's post is refused `no_user_token` with no
/// `chat.postMessage` sent, and the load warned once.
#[test]
fn a_hub_whose_secrets_hold_no_user_token_says_so_and_posts_nothing_for_a_person() {
    let warning = "chat-secrets.toml holds no user token";
    let text = logged(|| async {
        let home = Home::new(Some(SELF));
        home.configured();
        // The same secrets with the user token's line gone.
        let kept: String = std::fs::read_to_string(home.config().join(SECRETS_FILE)).unwrap().lines().filter(|l| !l.starts_with("user_token")).map(|l| format!("{l}\n")).collect();
        home.write(SECRETS_FILE, &kept);
        let w = world_on(home, &[], None, None).await;
        assert_eq!(w.services.chat.status(), Status::Running);
        w.rows.set_agent_handle(&w.ids[0], "alpha-h").unwrap();
        let mut conn = connect_window(w.services.clone()).await;
        // A row's agent resolves its pair first and posts only when it is mapped: neither is the window's to change.
        let mut cli = connect(w.services.clone()).await;
        for (n, (id, want)) in w.ids.iter().zip(["mapped", "unmapped"]).enumerate() {
            let resolved = cli.ask(2 + n as u64, op::CHAT_RESOLVE, json!({"row": id})).await;
            assert_eq!(resolved["state"], want, "row {n} on a hub with no user token: {resolved}");
        }
        let row_first = json!({"target": w.target(0, "C0AAAA"), "kind": "row", "name": "alpha-h", "text": "done"});
        let posted = cli.ask(6, op::CHAT_POST, row_first).await;
        assert_eq!(posted["outcome"], "posted", "a row's reply on a hub with no user token: {posted}");
        let window_resolved = conn.ask(7, op::CHAT_RESOLVE, json!({"row": w.ids[0]})).await;
        assert_eq!(window_resolved["state"], "mapped", "the window's resolve says nothing of the token: {window_resolved}");
        let sent = |w: &World| w.calls("/chat.postMessage");
        let person = json!({"target": w.target(0, "C0AAAA"), "kind": "person", "name": "tester", "text": "hello"});
        let refused = conn.ask(3, op::CHAT_POST, person).await;
        assert_eq!((refused["outcome"].as_str(), refused["code"].as_str()), (Some("refused"), Some("no_user_token")), "{refused}");
        assert_eq!(sent(&w), 1, "a person's post reached Slack with no user token (the row's own is the one)");
        let row = json!({"target": w.target(0, "C0AAAA"), "kind": "row", "name": "alpha-h", "text": "done"});
        let posted = conn.ask(4, op::CHAT_POST, row).await;
        assert_eq!(posted["outcome"], "posted", "{posted}");
        assert_eq!(sent(&w), 2, "a row's posts are the app's, and go out");
    });
    assert_eq!(text.matches(warning).count(), 1, "{text}");
}

/// The done test, through a real window connection against the fake Slack in one daemon process: a hub with no chat
/// file, a connect of three tokens, a pairing and a watch; a person's post goes out under the user token with nothing of
/// the app, comes back through the socket as the person's own message (a member's), and a row's post goes out under the
/// bot token as the app's. No log line of any thread, reply or event holds a byte of any token.
#[test]
fn a_window_connects_three_tokens_and_its_post_goes_out_as_the_person() {
    let bodies = ["4a4b4c4d", "5a5b5c5d", "6a6b6c6d"];
    let mut seen = Vec::new();
    let text = logged(|| async {
        let w = world(None, &[public("C0PUB", false)]).await;
        let chat = w.services.chat.clone();
        let (b, u, a) = (bot(bodies[0]), user(bodies[1]), app(bodies[2]));
        let mut conn = connect_window(w.services.clone()).await;
        let connected = conn.ask(2, op::CHAT_CONNECT, connect_with(&b, &u, &a)).await;
        assert_eq!(connected, json!({"workspace": "T0AAAA"}));
        assert!(secrets_hold(&w.home, &b, &u, &a), "the secrets hold other keys or tokens");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(w.home.config().join(SECRETS_FILE)).unwrap().permissions().mode() & 0o077, 0, "owner-only");
        }
        let unmapped = conn.ask(3, op::CHAT_RESOLVE, json!({"row": w.ids[0]})).await;
        assert_eq!(unmapped, json!({"state": "unmapped"}), "a hub holding the user token is not unconfigured");
        let paired = conn.pair(&w.ids[0], Some("C0PUB")).await;
        assert_eq!(paired, json!({}));
        after_hello(&chat, &w.rows, &w.target(0, "C0PUB")).await;
        let watched = conn.ask(4, op::CHAT_WATCH, json!({"target": w.target(0, "C0PUB")})).await;
        assert!(matches!(watched["state"].as_str(), Some("pending" | "complete")), "{watched}");
        let hub = chat.hub().clone();
        let probe = hub.connect();
        until("the snapshot", || probe.conn.state("T0AAAA", "C0PUB") == sot_protocol::ops::ChatWatchState::Complete).await;

        // A person's post: one `chat.postMessage` under the user token, with the channel and the text and nothing else.
        let person = json!({"target": w.target(0, "C0PUB"), "kind": "person", "name": "tester", "text": "hello"});
        let posted = conn.ask(5, op::CHAT_POST, person).await;
        assert_eq!(posted["outcome"], "posted", "{posted}");
        let sent: Vec<_> = w.fake.seen().into_iter().filter(|s| s.path().ends_with("/chat.postMessage")).collect();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].authorization.as_deref(), Some(&*format!("Bearer {u}")), "the person's bearer");
        let body = sent[0].json().unwrap();
        let mut keys: Vec<&str> = body.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["channel", "text"], "nothing of the app rides a person's post");

        // Slack sends the post as the person's own message: their user, an app id, no bot id.
        w.slack.feed.send(json!({
            "type": "events_api", "envelope_id": "env-1",
            "payload": {"team_id": "T0AAAA", "event": {"type": "message", "channel": "C0PUB", "user": "U0PERSON", "app_id": "A0APP", "ts": stamp(1), "text": "hello"}},
        })).unwrap();
        let put = loop {
            let (frame, _) = codec::read_frame(&mut conn.crx).await.unwrap();
            if frame.kind == Kind::Evt && frame.op == op::CHAT_CHANGED {
                break frame.payload;
            }
        };
        assert_eq!((put["put"][0]["origin"].as_str(), put["put"][0]["author"]["id"].as_str()), (Some("member"), Some("U0PERSON")), "{put}");

        // A row's post is the app's, under the bot token, with its author and metadata.
        w.rows.set_agent_handle(&w.ids[0], "alpha-h").unwrap();
        let mut cli = connect(w.services.clone()).await;
        let row = json!({"target": w.target(0, "C0PUB"), "kind": "row", "name": "alpha-h", "text": "done"});
        let row_posted = cli.ask(6, op::CHAT_POST, row).await;
        assert_eq!(row_posted["outcome"], "posted", "{row_posted}");
        let sent: Vec<_> = w.fake.seen().into_iter().filter(|s| s.path().ends_with("/chat.postMessage")).collect();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1].authorization.as_deref(), Some(&*format!("Bearer {b}")), "the bot's bearer");
        let body = sent[1].json().unwrap();
        assert_eq!(body["username"], "alpha-h (row via Ship of Tools)");
        assert_eq!(body["metadata"]["event_type"], "sot_chat_post");
        seen.extend([connected, unmapped, paired, watched, posted, put, row_posted].map(|v| v.to_string()));
    });
    holds_no_token("the daemon's log", &text, &bodies);
    for (n, reply) in seen.iter().enumerate() {
        holds_no_token(&format!("reply or event {n}"), reply, &bodies);
    }
}

// ---- several workspaces ----

/// A hub with no chat file, rows alpha and beta in two folders, and one fake answering two workspaces by bearer.
struct Two {
    home: Home,
    teams: sot_chat::fake_slack::Teams,
    services: Services,
    rows: Workspaces,
    ids: [String; 2],
    folders: [std::path::PathBuf; 2],
    _root: tempfile::TempDir,
}

const A: &str = "T0AAAA";
const B: &str = "T0BBBB";

async fn two() -> Two {
    let teams = sot_chat::fake_slack::Teams::start(&[(A, "Alpha Lab"), (B, "Beta Lab")]).await;
    teams.channels(A, &[("C0AAAA01", "alpha-one", false, true), ("C0AAAA02", "alpha-two", false, true)]);
    teams.channels(B, &[("C0BBBB01", "beta-one", false, false)]);
    let home = Home::new(Some(SELF));
    let folders = [home.folder("a"), home.folder("b")];
    let rows = Workspaces::new();
    let ids = [rows.insert(row("alpha", &folders[0])).workspace_id.clone(), rows.insert(row("beta", &folders[1])).workspace_id.clone()];
    let chat = Chat::boot(&home.env, &rows, &|p| config::read_file(p), &Registry::new(), starter(teams.fake()), Verifier::at(teams.fake()), HUB_CHECK, Timing::PRODUCT);
    running(&chat).await;
    let root = tempfile::tempdir().unwrap();
    let mut services = Services::for_test("t", root.path());
    services.chat = chat;
    services.workspaces = rows.clone();
    Two { home, teams, services, rows, ids, folders, _root: root }
}

impl Two {
    fn connect_req(&self, w: &str) -> Value {
        connect_with(&self.teams.bot(w), &self.teams.user(w), &self.teams.app_token(w))
    }

    fn target(&self, row: usize, workspace: &str, channel: &str) -> Value {
        json!({"row": self.ids[row], "workspace": workspace, "channel": channel})
    }

    /// A secrets file holds exactly the four keys, with workspace `w`'s tokens.
    fn holds(&self, file: &str, w: &str) -> bool {
        let t: toml::Table = std::fs::read_to_string(self.home.config().join(file)).unwrap().parse().unwrap();
        t.len() == 4
            && t["workspace"].as_str() == Some(w)
            && t["bot_token"].as_str() == Some(&*self.teams.bot(w))
            && t["user_token"].as_str() == Some(&*self.teams.user(w))
            && t["app_token"].as_str() == Some(&*self.teams.app_token(w))
    }

    #[cfg(unix)]
    fn owner_only(&self, file: &str) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(self.home.config().join(file)).unwrap().permissions().mode() & 0o077 == 0
    }
}

/// The next `chat.changed` on `conn` for `channel`, past every other frame.
async fn changed_in(conn: &mut Conn, channel: &str) -> Value {
    let wait = async {
        loop {
            let (frame, _) = codec::read_frame(&mut conn.crx).await.unwrap();
            if frame.kind == Kind::Evt && frame.op == op::CHAT_CHANGED && frame.payload["channel"] == channel {
                return frame.payload;
            }
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), wait).await.expect("a chat.changed for the channel")
}

/// The plan's done test, the daemon's half: from a hub with no chat file, a window connects two workspaces, each
/// project pairs a channel in its own, reads its own workspace's socket and posts under its own workspace's token; a
/// token Slack revokes in one stops that one alone; no reply, event or log line holds a token's bytes.
#[test]
fn two_workspaces_from_nothing_each_project_reads_and_posts_in_its_own() {
    let mut seen: Vec<String> = Vec::new();
    let mut bodies = Vec::new();
    let text = logged(|| async {
        let w = two().await;
        bodies.extend(w.teams.bodies());
        let mut conn = connect_window(w.services.clone()).await;

        // 2. The first workspace: today's two files.
        let first = conn.ask(2, op::CHAT_CONNECT, w.connect_req(A)).await;
        assert_eq!(first, json!({"workspace": A, "name": "Alpha Lab"}), "the reply names the workspace as Slack gave it");
        assert!(w.holds(SECRETS_FILE, A), "chat-secrets.toml holds the first workspace's four keys");
        #[cfg(unix)]
        assert!(w.owner_only(SECRETS_FILE));
        assert_eq!(std::fs::read_to_string(w.home.config().join(CONFIG_FILE)).unwrap(), config::fresh(A).unwrap());
        let before = files(&w.home);

        // 3. The second: its own file, and the first two byte for byte.
        let second = conn.ask(3, op::CHAT_CONNECT, w.connect_req(B)).await;
        assert_eq!(second, json!({"workspace": B, "name": "Beta Lab"}), "a second workspace is added");
        let named = format!("chat-secrets-{B}.toml");
        let after = files(&w.home);
        assert_eq!(after.iter().filter(|(n, _)| n != &named).collect::<Vec<_>>(), before.iter().collect::<Vec<_>>(), "the first workspace's files are as they were");
        assert!(w.holds(&named, B));
        #[cfg(unix)]
        assert!(w.owner_only(&named));

        // 4. The workspaces, by name; each list is its own workspace's.
        let listed = conn.ask(4, op::CHAT_WORKSPACES, json!({})).await;
        assert_eq!(
            listed,
            json!({"default": A, "workspaces": [{"id": A, "name": "Alpha Lab", "running": true}, {"id": B, "name": "Beta Lab", "running": true}]})
        );
        let a_list = conn.ask(5, op::CHAT_CHANNELS, json!({})).await;
        let b_list = conn.ask(6, op::CHAT_CHANNELS, json!({"workspace": B})).await;
        let ids = |v: &Value| v["channels"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!((ids(&a_list), ids(&b_list)), (vec!["C0AAAA01".to_string(), "C0AAAA02".into()], vec!["C0BBBB01".to_string()]));

        // 5. Each project pairs in its own workspace.
        assert_eq!(conn.ask(7, op::CHAT_PAIR, json!({"row": w.ids[0], "channel": "C0AAAA01"})).await, json!({}));
        assert_eq!(conn.ask(8, op::CHAT_PAIR, json!({"row": w.ids[1], "workspace": B, "channel": "C0BBBB01"})).await, json!({}));

        // 6. Each resolves in its own.
        let alpha = conn.ask(9, op::CHAT_RESOLVE, json!({"row": w.ids[0]})).await;
        let beta = conn.ask(10, op::CHAT_RESOLVE, json!({"row": w.ids[1]})).await;
        assert_eq!((alpha["state"].as_str(), alpha["workspace"].as_str(), alpha["channel"].as_str()), (Some("mapped"), Some(A), Some("C0AAAA01")), "{alpha}");
        assert_eq!((beta["state"].as_str(), beta["workspace"].as_str(), beta["channel"].as_str()), (Some("mapped"), Some(B), Some("C0BBBB01")), "{beta}");

        // 7. Each watch hears its own workspace's socket.
        let chat = w.services.chat.clone();
        let (ta, tb) = (w.target(0, A, "C0AAAA01"), w.target(1, B, "C0BBBB01"));
        for t in [&ta, &tb] {
            after_hello(&chat, &w.rows, &serde_json::from_value(t.clone()).unwrap()).await;
        }
        let wa = conn.ask(11, op::CHAT_WATCH, json!({"target": ta})).await;
        let wb = conn.ask(12, op::CHAT_WATCH, json!({"target": tb})).await;
        assert!(wa["state"].is_string() && wb["state"].is_string(), "{wa} {wb}");
        let probe = chat.hub().connect();
        until("both snapshots", || {
            probe.conn.state(A, "C0AAAA01") == sot_protocol::ops::ChatWatchState::Complete
                && probe.conn.state(B, "C0BBBB01") == sot_protocol::ops::ChatWatchState::Complete
        })
        .await;
        w.teams.feed(A, w.teams.message(A, "C0AAAA01", 1, &stamp(1)));
        let put = changed_in(&mut conn, "C0AAAA01").await;
        assert_eq!(put["workspace"], A, "{put}");
        w.teams.feed(B, w.teams.message(B, "C0BBBB01", 2, &stamp(2)));
        let put_b = changed_in(&mut conn, "C0BBBB01").await;
        assert_eq!(put_b["workspace"], B, "{put_b}");
        // B's owner published its states for that put (alpha's owner had nothing to do): alpha's state is kept.
        assert_eq!(probe.conn.state(A, "C0AAAA01"), sot_protocol::ops::ChatWatchState::Complete, "one workspace's publish cleared another's states");

        // 8. A person's post in beta's channel goes out once under the second workspace's user token.
        let posted = conn.ask(13, op::CHAT_POST, json!({"target": tb, "kind": "person", "name": "tester", "text": "hello"})).await;
        assert_eq!(posted["outcome"], "posted", "{posted}");
        let sent: Vec<_> = w.teams.fake().seen().into_iter().filter(|s| s.path().ends_with("/chat.postMessage")).collect();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].authorization.as_deref(), Some(&*format!("Bearer {}", w.teams.user(B))));

        assert!(chat.hub().running_in(A) && chat.hub().running_in(B), "each workspace has its own owner");
        // 10. Slack revokes the first workspace's bot: alpha's read is refused, its workspace ends, and beta's goes on.
        w.teams.revoke(A);
        let history = conn.ask(14, op::CHAT_HISTORY, json!({"target": ta, "limit": 5})).await;
        assert!(history["code"].is_string(), "a revoked token's read is refused: {history}");
        until("the first workspace's end", || chat.status_of(A) == Status::Failed("bot_token_refused")).await;
        let alpha = conn.ask(15, op::CHAT_RESOLVE, json!({"row": w.ids[0]})).await;
        assert_eq!((alpha["state"].as_str(), alpha["reason"].as_str(), alpha["workspace"].as_str()), (Some("unconfigured"), Some("tokens_refused"), Some(A)), "the answer names the workspace that failed: {alpha}");
        assert_eq!(chat.status_of(B), Status::Running);
        until("the first workspace's owner's end", || !chat.hub().running_in(A)).await;
        assert!(chat.hub().running_in(B), "one workspace's end ended another's owner");
        w.teams.feed(B, w.teams.message(B, "C0BBBB01", 3, &stamp(3)));
        let put_c = changed_in(&mut conn, "C0BBBB01").await;
        assert_eq!(put_c["put"][0]["ts"], stamp(3).as_str(), "{put_c}");

        // 11. The file: alpha's mapping names no workspace, beta's names its own.
        let t: toml::Table = std::fs::read_to_string(w.home.config().join(CONFIG_FILE)).unwrap().parse().unwrap();
        let mappings = t["mapping"].as_array().unwrap();
        let of = |folder: &std::path::Path| mappings.iter().find(|m| m["folder"].as_str() == folder.to_str()).unwrap().clone();
        assert_eq!(of(&w.folders[0]).get("workspace"), None);
        assert_eq!(of(&w.folders[1]).get("workspace").and_then(toml::Value::as_str), Some(B));
        seen.extend([first, second, listed, a_list, b_list, alpha, beta, put, put_b, posted, history, put_c].map(|v| v.to_string()));
    });
    holds_no_token("the daemon's log", &text, &bodies);
    for (n, reply) in seen.iter().enumerate() {
        holds_no_token(&format!("reply or event {n}"), reply, &bodies);
    }
}

/// Writes a secrets file `file` for workspace `w` with tokens of the fake's form (`mark` tells them apart).
fn write_secrets_of(home: &Home, file: &str, w: &str, mark: &str) {
    let text = format!("workspace = \"{w}\"\nbot_token = \"{}\"\nuser_token = \"{}\"\napp_token = \"{}\"\n", bot(mark), user(mark), app(mark));
    home.write(file, &text);
}

/// B1 delta 5 (L10's ninth workspace): a connect of a workspace with no file yet is refused, nothing written, when the
/// folder already lists seven named files beside the default's, a refused one among them (a new name sorting first would
/// push a served one past the load's bound); one of the seven connects again into its own file alone.
#[test]
fn a_connect_that_would_push_a_served_workspace_past_the_bound_is_refused() {
    logged(|| async {
        let teams = sot_chat::fake_slack::Teams::start(&[("T0B00", "New Lab"), ("T0C03", "Third Lab")]).await;
        let home = Home::new(Some(SELF));
        home.write(CONFIG_FILE, &config::fresh("T0AAAA").unwrap());
        write_secrets_of(&home, SECRETS_FILE, "T0AAAA", "0");
        for i in 1..=7 {
            let id = format!("T0C0{i}");
            write_secrets_of(&home, &format!("chat-secrets-{id}.toml"), &id, &i.to_string());
        }
        home.write("chat-secrets-T0C05.toml", "workspace = [[[");
        let rows = Workspaces::new();
        let chat = Chat::boot(&home.env, &rows, &|p| config::read_file(p), &Registry::new(), starter(teams.fake()), Verifier::at(teams.fake()), HUB_CHECK, Timing::PRODUCT);
        running(&chat).await;
        let root = tempfile::tempdir().unwrap();
        let mut services = Services::for_test("t", root.path());
        services.chat = chat;
        let mut conn = connect_window(services.clone()).await;
        let req = |w: &str| connect_with(&teams.bot(w), &teams.user(w), &teams.app_token(w));
        let before = files(&home);
        assert_eq!(code(&conn.ask(2, op::CHAT_CONNECT, req("T0B00")).await), "too_many_workspaces");
        assert_eq!(files(&home), before, "a refused connect wrote something");
        // One of the seven gives new tokens: its own file alone changes.
        assert_eq!(conn.ask(3, op::CHAT_CONNECT, req("T0C03")).await, json!({"workspace": "T0C03", "name": "Third Lab"}));
        let after = files(&home);
        let own = "chat-secrets-T0C03.toml";
        assert_eq!(after.iter().filter(|(n, _)| n != own).collect::<Vec<_>>(), before.iter().filter(|(n, _)| n != own).collect::<Vec<_>>());
        let t: toml::Table = std::fs::read_to_string(home.config().join(own)).unwrap().parse().unwrap();
        assert_eq!(t["bot_token"].as_str(), Some(&*teams.bot("T0C03")));
    });
}

/// B1 delta 6: `chat.workspaces` keeps a workspace's name past its service's end, so a failed workspace is still named;
/// it is listed not running.
#[test]
fn the_workspaces_op_keeps_a_failed_workspaces_name() {
    logged(|| async {
        let w = two().await;
        let mut conn = connect_window(w.services.clone()).await;
        conn.ask(2, op::CHAT_CONNECT, w.connect_req(A)).await;
        conn.ask(3, op::CHAT_CONNECT, w.connect_req(B)).await;
        w.teams.revoke(A);
        // A's bot is refused on its next call, which ends its service.
        assert!(conn.ask(4, op::CHAT_CHANNELS, json!({"workspace": A})).await["code"].is_string());
        let chat = w.services.chat.clone();
        until("the first workspace's end", || chat.status_of(A) == Status::Failed("bot_token_refused")).await;
        let listed = conn.ask(5, op::CHAT_WORKSPACES, json!({})).await;
        assert_eq!(listed["workspaces"][0], json!({"id": A, "name": "Alpha Lab", "running": false}), "{listed}");
        assert_eq!(listed["workspaces"][1], json!({"id": B, "name": "Beta Lab", "running": true}), "{listed}");
        // Any connection may read it; a hub with no chat.toml has none to give.
        let mut cli = connect(w.services.clone()).await;
        assert_eq!(cli.ask(6, op::CHAT_WORKSPACES, json!({})).await, listed);
    });
}

/// PS9, L11 rule 2: a channel the pairing's workspace does not hold is `no_such_channel`, unpaired; a workspace whose
/// listener does not run is `unavailable` for the list and the pair.
#[test]
fn a_channel_of_another_workspace_is_no_such_channel() {
    logged(|| async {
        let w = two().await;
        // The pair asks B's bot about A's channel, to be refused: the one crossing this test makes.
        w.teams.expect_crossing();
        let mut conn = connect_window(w.services.clone()).await;
        conn.ask(2, op::CHAT_CONNECT, w.connect_req(A)).await;
        conn.ask(3, op::CHAT_CONNECT, w.connect_req(B)).await;
        let paired = conn.ask(4, op::CHAT_PAIR, json!({"row": w.ids[1], "workspace": B, "channel": "C0AAAA01"})).await;
        assert_eq!(code(&paired), "no_such_channel", "{paired}");
        assert!(std::fs::read_to_string(w.home.config().join(CONFIG_FILE)).unwrap().parse::<toml::Table>().unwrap().get("mapping").is_none());
        for (op, payload) in [(op::CHAT_CHANNELS, json!({"workspace": "T0ZZZZ"})), (op::CHAT_PAIR, json!({"row": w.ids[1], "workspace": "T0ZZZZ", "channel": "C0BBBB01"}))] {
            assert_eq!(code(&conn.ask(5, op, payload).await), "unavailable", "{op}");
        }
    });
}

/// C4, L8 rule 4: a mapped row is judged by its own workspace; an unmapped one is `unconfigured` only when no workspace's
/// listener runs, so one failed workspace never keeps a person from pairing in another.
#[test]
fn resolve_judges_a_row_by_its_own_workspace_and_an_unmapped_row_by_whether_any_runs() {
    logged(|| async {
        let w = two().await;
        let mut conn = connect_window(w.services.clone()).await;
        conn.ask(2, op::CHAT_CONNECT, w.connect_req(A)).await;
        conn.ask(3, op::CHAT_CONNECT, w.connect_req(B)).await;
        assert_eq!(conn.ask(4, op::CHAT_PAIR, json!({"row": w.ids[0], "channel": "C0AAAA01"})).await, json!({}));
        w.teams.revoke(A);
        assert!(conn.ask(5, op::CHAT_CHANNELS, json!({})).await["code"].is_string());
        let chat = w.services.chat.clone();
        until("the default workspace's end", || chat.status_of(A) == Status::Failed("bot_token_refused")).await;
        let alpha = conn.ask(6, op::CHAT_RESOLVE, json!({"row": w.ids[0]})).await;
        assert_eq!((alpha["state"].as_str(), alpha["reason"].as_str()), (Some("unconfigured"), Some("tokens_refused")), "{alpha}");
        let beta = conn.ask(7, op::CHAT_RESOLVE, json!({"row": w.ids[1]})).await;
        assert_eq!(beta, json!({"state": "unmapped"}), "an unmapped row read unconfigured while another workspace runs");
        // And it pairs there.
        assert_eq!(conn.ask(8, op::CHAT_PAIR, json!({"row": w.ids[1], "workspace": B, "channel": "C0BBBB01"})).await, json!({}));
    });
}

/// B1 (review): a request whose target names one running workspace for a channel the row maps in another is
/// `stale_mapping` at admission, for every request op, and nothing reaches Slack under the named workspace's tokens (the
/// fake records every crossed request, and fails the test when it is dropped holding one).
#[test]
fn a_target_naming_another_running_workspace_for_the_rows_channel_is_stale_mapping() {
    logged(|| async {
        let w = two().await;
        let mut conn = connect_window(w.services.clone()).await;
        conn.ask(2, op::CHAT_CONNECT, w.connect_req(A)).await;
        conn.ask(3, op::CHAT_CONNECT, w.connect_req(B)).await;
        assert_eq!(conn.ask(4, op::CHAT_PAIR, json!({"row": w.ids[0], "channel": "C0AAAA01"})).await, json!({}));
        std::fs::write(w.folders[0].join("fit.png"), b"png").unwrap();
        w.rows.set_agent_handle(&w.ids[0], "alpha-h").unwrap();
        let crafted = w.target(0, B, "C0AAAA01");
        let asks = [
            (op::CHAT_HISTORY, json!({"target": crafted, "limit": 5})),
            (op::CHAT_POST, json!({"target": crafted, "kind": "person", "name": "tester", "text": "hello"})),
            (op::CHAT_FILE, json!({"target": crafted, "file": "F0PIC0001", "variant": "thumb"})),
            (op::CHAT_WATCH, json!({"target": crafted})),
        ];
        for (n, (name, payload)) in asks.into_iter().enumerate() {
            let reply = conn.ask(10 + n as u64, name, payload).await;
            assert_eq!(reply["code"].as_str(), Some("stale_mapping"), "{name}: {reply}");
        }
        let mut cli = connect(w.services.clone()).await;
        let filed = json!({"target": crafted, "kind": "row", "name": "alpha-h", "node_id": "files:fit.png", "comment": "the fit"});
        let reply = cli.ask(20, op::CHAT_POST_FILE, filed).await;
        assert_eq!(reply["code"].as_str(), Some("stale_mapping"), "post_file: {reply}");
        for (kind, method) in [("xoxb", "/conversations.history"), ("xoxb", "/chat.postMessage"), ("xoxp", "/chat.postMessage"), ("xoxb", "/files.info"), ("xoxp", "/files.getUploadURLExternal")] {
            assert!(w.teams.calls(B, kind, method).is_empty(), "the crafted target reached Slack under B's {kind}: {method}");
        }
        assert!(w.teams.crossed().is_empty(), "{:?}", w.teams.crossed());
    });
}
