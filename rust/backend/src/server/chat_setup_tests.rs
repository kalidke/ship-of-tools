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
use sot_platform::test_hold::Holds;
use sot_protocol::ops::{ChatChannel, ChatCode, ChatTarget};
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
    /// How `bots.info` answers a bot token whose body is `other` while it is known (a check's word, [`answer`]), and how
    /// it answers any other bot token then.
    other_bots: Mutex<&'static str>,
    own_bots: Mutex<&'static str>,
    hello_app: Mutex<Option<&'static str>>,
    feed: tokio::sync::mpsc::UnboundedSender<Value>,
}

/// A check's answer: `ok` is `good`, `drop` closes the connection unanswered, `hang` never answers, `noapp` is an `ok`
/// that names no app, a number is that HTTP status, and any other word is Slack's `ok: false` with it as the error code.
fn answer(how: &str, good: Reply) -> Reply {
    match how {
        "ok" => good,
        "drop" => Reply::Drop,
        "hang" => Reply::Hang,
        "noapp" => Reply::json(&json!({"ok": true, "bot": {"id": "B0BOT"}})),
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
    let state = Arc::new(Slack { channels: Mutex::new(Vec::new()), join: Mutex::new("ok"), auth: Mutex::new("ok"), user: Mutex::new("ok"), user_team: Mutex::new("T0AAAA"), open: Mutex::new("ok"), list: Mutex::new("ok"), bot_app: Mutex::new(None), other_bots: Mutex::new("ok"), own_bots: Mutex::new("ok"), hello_app: Mutex::new(None), feed });
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
            // A bot token whose body is `other` is another Slack app's.
            let other = seen.authorization.as_deref().is_some_and(|a| a.contains("-other-"));
            let app = if other { "A0OTHER" } else { held.bot_app.lock().unwrap().unwrap_or_default() };
            let good = Reply::json(&json!({"ok": true, "bot": {"id": "B0BOT", "app_id": app}}));
            answer(*if other { held.other_bots.lock() } else { held.own_bots.lock() }.unwrap(), good)
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
                    // A socket that goes while another holds the feed (a connect's probe beside the listener's) never takes it.
                    let mut rx = tokio::select! {
                        rx = rx.lock() => rx,
                        () = peer.drain() => return,
                    };
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
    let Verifier { identify, app: open, bot_app } = Verifier::at(&fake_for_checks);
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
        bot_app,
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
    // A chat.toml the load refuses is never rewritten from the window (the connect says to replace the app that runs,
    // which this fake's `bots.info` does not tell, so it reaches the write).
    w.home.write(CONFIG_FILE, "workspace = [[[");
    let before = files(&w.home);
    let mut again = connect_req(&bot("1"), &app("1"));
    again["replace_app"] = json!(true);
    assert_eq!(code(&conn.ask(3, op::CHAT_CONNECT, again).await), "not_saved");
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
        // The reloads below restart the workspace: its token changes.
        w.home.rekey(SECRETS_FILE, "cccc");
        let mut conn = connect_window(w.services.clone()).await;
        let chat = w.services.chat.clone();
        match ended {
            "wrong_app" => {
                *w.slack.bot_app.lock().unwrap() = Some("A0OWN");
                *w.slack.hello_app.lock().unwrap() = Some("A0OTHER");
                chat.reload_for(&chat.setup().await, &w.rows, "T0AAAA").await;
            }
            "app_token_refused" => {
                *w.slack.open.lock().unwrap() = "invalid_auth";
                chat.reload_for(&chat.setup().await, &w.rows, "T0AAAA").await;
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

/// A connect while the chat runs: here its start could not tell its Slack app (the fake knows no `bots.info`), so the
/// connect is asked about first, nothing written; told to replace, it saves the tokens and restarts the chat.
#[tokio::test(flavor = "multi_thread")]
async fn a_connect_while_running_replaces_the_tokens_and_restarts() {
    let home = Home::new(Some(SELF));
    home.configured();
    // The app is in the mapped channel, so the reply names no channel it cannot read.
    let w = world_on(home, &[public("C0AAAA", true)], None, None).await;
    assert_eq!(w.services.chat.status(), Status::Running);
    let (toml, before) = (w.file(), files(&w.home));
    let (b, a) = (bot("2"), app("2"));
    let auth = |token: &str| w.fake.seen().iter().filter(|x| x.path().ends_with("/auth.test") && x.authorization.as_deref() == Some(&*format!("Bearer {token}"))).count();
    let mut conn = connect_window(w.services.clone()).await;
    let mut asked = conn.ask(2, op::CHAT_CONNECT, connect_req(&b, &a)).await;
    asked.as_object_mut().and_then(|r| r.remove("error"));
    assert_eq!(asked, json!({"code": "other_app", "workspace": "T0AAAA"}), "a running app that is not told is asked about, and named by nothing");
    assert!(files(&w.home) == before, "a connect asked about wrote a file");
    let mut anyway = connect_req(&b, &a);
    anyway["replace_app"] = json!(true);
    assert_eq!(conn.ask(3, op::CHAT_CONNECT, anyway).await, json!({"workspace": "T0AAAA"}));
    assert_eq!(w.services.chat.status(), Status::Running, "the reloaded chat is not running");
    assert!(secrets_hold(&w.home, &b, &user("1"), &a), "the secrets file does not hold the new tokens");
    assert_eq!(w.file(), toml, "a connect rewrote an admitted chat.toml");
    assert_eq!(auth(&b), 3, "the new bot token: each connect's identify and the restarted service's own auth.test");
    let resolved = conn.ask(4, op::CHAT_RESOLVE, json!({"row": w.ids[0]})).await;
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

/// A `chat.workspaces` entry's id, name and whether it runs, without what it says of its Slack app.
fn plain(entry: &Value) -> Value {
    let mut entry = entry.clone();
    if let Some(e) = entry.as_object_mut() {
        e.retain(|k, _| ["id", "name", "running"].contains(&k.as_str()));
    }
    entry
}

async fn two() -> Two {
    two_named("Alpha Lab", "Beta Lab").await
}

/// `two` with the names Slack gives the workspaces; an empty one is a workspace Slack gave no usable name.
async fn two_named(a_name: &'static str, b_name: &'static str) -> Two {
    let teams = sot_chat::fake_slack::Teams::start(&[(A, a_name), (B, b_name)]).await;
    teams.channels(A, &[("C0AAAA01", "alpha-one", false, true), ("C0AAAA02", "alpha-two", false, true)]);
    teams.channels(B, &[("C0BBBB01", "beta-one", false, false)]);
    teams.members(A, &[sot_chat::fake_slack::Members::person("U0ADA", "Ada", "Ada Example")]);
    teams.members(B, &[sot_chat::fake_slack::Members::person("U0BEN", "Ben", "Ben Sample")]);
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
        assert_eq!(listed["default"], A, "{listed}");
        assert_eq!(
            listed["workspaces"].as_array().map(|l| l.iter().map(plain).collect::<Vec<_>>()),
            Some(vec![json!({"id": A, "name": "Alpha Lab", "running": true}), json!({"id": B, "name": "Beta Lab", "running": true})])
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

        // 8. A person's post in beta's channel goes out once under the second workspace's user token, its `@` names
        // looked up among beta's members alone (and only under beta's bot), the one not found named in the reply.
        let posted = conn.ask(13, op::CHAT_POST, json!({"target": tb, "kind": "person", "name": "tester", "text": "hello @Ben and @Ada"})).await;
        assert_eq!(posted["outcome"], "posted", "{posted}");
        assert_eq!(posted["unlinked"], json!([{"name": "\"@Ada\"", "why": "no_member"}]), "{posted}");
        assert_eq!(posted["mentioned"], json!(["\"Ben\" (\"Ben Sample\") @u0ben"]), "{posted}");
        let sent: Vec<_> = w.teams.fake().seen().into_iter().filter(|s| s.path().ends_with("/chat.postMessage")).collect();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].authorization.as_deref(), Some(&*format!("Bearer {}", w.teams.user(B))));
        assert_eq!(sent[0].json().unwrap()["text"], "hello <@U0BEN> and @Ada");
        assert_eq!((w.teams.calls(A, "xoxb", "/users.list").len(), w.teams.calls(B, "xoxb", "/users.list").len()), (0, 1));

        assert!(chat.hub().running_in(A) && chat.hub().running_in(B), "each workspace has its own owner");
        // 9. Alpha moves to A's other channel: A's listener starts again, B's does not (no new `auth.test` under its bot,
        // no new socket), and a message fed to B's socket while A restarts reaches the connection, B's watch told
        // nothing of A's reload; A's owner starts again and B's does not (its line in the log, read after the run).
        tracing::info!("{STEP_9}");
        let (a_starts, b_starts, b_sockets) =
            (w.teams.calls(A, "xoxb", "/auth.test").len(), w.teams.calls(B, "xoxb", "/auth.test").len(), w.teams.calls(B, "xapp", "/apps.connections.open").len());
        conn.send(16, op::CHAT_PAIR, json!({"row": w.ids[0], "channel": "C0AAAA02"})).await;
        w.teams.feed(B, w.teams.message(B, "C0BBBB01", 4, &stamp(4)));
        let (mut paired, mut put_d, mut told) = (None, None, Vec::new());
        let read = async {
            while paired.is_none() || put_d.is_none() {
                let (frame, _) = codec::read_frame(&mut conn.crx).await.unwrap();
                match (frame.kind, frame.op.as_str()) {
                    (Kind::Res, _) if frame.id == 16 => paired = Some(frame.payload),
                    (Kind::Evt, op::CHAT_CHANGED) if frame.payload["channel"] == "C0BBBB01" => put_d = Some(frame.payload),
                    (Kind::Evt, op::CHAT_STATUS) => told.push(frame.payload),
                    _ => {}
                }
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(20), read).await.expect("the pair's reply and B's message");
        assert_eq!(paired, Some(json!({})));
        assert_eq!(put_d.as_ref().unwrap()["put"][0]["ts"], stamp(4).as_str(), "{put_d:?}");
        assert!(!told.iter().any(|t| t.to_string().contains("C0BBBB01")), "B's watch was told of A's reload: {told:?}");
        until("A's new listener", || chat.status_of(A) == Status::Running && chat.hub().running_in(A)).await;
        assert_eq!(w.teams.calls(A, "xoxb", "/auth.test").len(), a_starts + 1, "A's listener started again");
        assert_eq!(
            (w.teams.calls(B, "xoxb", "/auth.test").len(), w.teams.calls(B, "xapp", "/apps.connections.open").len()),
            (b_starts, b_sockets),
            "B's listener was restarted by A's change"
        );
        let ta = w.target(0, A, "C0AAAA02");
        after_hello(&chat, &w.rows, &serde_json::from_value(ta.clone()).unwrap()).await;
        tracing::info!("{STEP_10}");
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
        seen.extend([first, second, listed, a_list, b_list, alpha, beta, put, put_b, posted, put_d.unwrap_or_default(), history, put_c].map(|v| v.to_string()));
    });
    holds_no_token("the daemon's log", &text, &bodies);
    for (n, reply) in seen.iter().enumerate() {
        holds_no_token(&format!("reply or event {n}"), reply, &bodies);
    }
    let step_9 = text.split(STEP_9).nth(1).and_then(|t| t.split(STEP_10).next()).expect("step 9's lines");
    let owner = |w: &str| format!("project chat status=the chat service is running workspace={w:?}");
    assert!(step_9.contains(&owner(A)), "A's owner did not start again at its reload");
    assert!(!step_9.contains(&owner(B)), "A's reload started B's owner again");
}

/// The done test's marks in its log, around step 9.
const STEP_9: &str = "the done test: step 9";
const STEP_10: &str = "the done test: step 10";

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
        assert_eq!(listed["workspaces"][0], json!({"id": A, "name": "Alpha Lab", "running": false}), "a workspace that does not run says nothing of its app: {listed}");
        assert_eq!(plain(&listed["workspaces"][1]), json!({"id": B, "name": "Beta Lab", "running": true}), "{listed}");
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

/// C12, L11 rule 3: a mapped answer names its workspace only while more than one is admitted, so a hub with one keeps
/// the title as it was; the name is the last one Slack gave, each row's its own workspace's, and a workspace admitted
/// with no name leaves the named one named and carries none itself.
#[test]
fn a_resolve_names_its_workspace_only_while_the_hub_serves_two() {
    logged(|| async {
        let w = two().await;
        let mut conn = connect_window(w.services.clone()).await;
        conn.ask(2, op::CHAT_CONNECT, w.connect_req(A)).await;
        assert_eq!(conn.ask(3, op::CHAT_PAIR, json!({"row": w.ids[0], "channel": "C0AAAA01"})).await, json!({}));
        let one = conn.ask(4, op::CHAT_RESOLVE, json!({"row": w.ids[0]})).await;
        assert_eq!((one["state"].as_str(), one.get("workspace_name")), (Some("mapped"), None), "{one}");
        conn.ask(5, op::CHAT_CONNECT, w.connect_req(B)).await;
        let two = conn.ask(6, op::CHAT_RESOLVE, json!({"row": w.ids[0]})).await;
        assert_eq!((two["state"].as_str(), two["workspace_name"].as_str()), (Some("mapped"), Some("Alpha Lab")), "{two}");
        // A row of the second workspace gets that workspace's name, and one that is not admitted has none to give.
        assert_eq!(conn.ask(7, op::CHAT_PAIR, w.target(1, B, "C0BBBB01")).await, json!({}));
        let beta = conn.ask(8, op::CHAT_RESOLVE, json!({"row": w.ids[1]})).await;
        assert_eq!(beta["workspace_name"].as_str(), Some("Beta Lab"), "{beta}");
        assert_eq!(w.services.chat.name_to_show("T0ZZZZ"), None);
    });

    // A second workspace admitted with no name is counted, so the first is named, and it carries no name.
    logged(|| async {
        let w = two_named("Alpha Lab", "").await;
        let mut conn = connect_window(w.services.clone()).await;
        conn.ask(2, op::CHAT_CONNECT, w.connect_req(A)).await;
        conn.ask(3, op::CHAT_CONNECT, w.connect_req(B)).await;
        assert_eq!(conn.ask(4, op::CHAT_PAIR, w.target(0, A, "C0AAAA01")).await, json!({}));
        assert_eq!(conn.ask(5, op::CHAT_PAIR, w.target(1, B, "C0BBBB01")).await, json!({}));
        let alpha = conn.ask(6, op::CHAT_RESOLVE, json!({"row": w.ids[0]})).await;
        let beta = conn.ask(7, op::CHAT_RESOLVE, json!({"row": w.ids[1]})).await;
        assert_eq!(alpha["workspace_name"].as_str(), Some("Alpha Lab"), "{alpha}");
        assert_eq!((beta["state"].as_str(), beta.get("workspace_name")), (Some("mapped"), None), "{beta}");
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

// ---- a mapped channel the connected Slack app is not in ----

const PRIVATE: &str = "G0PRIVATE1";
const PUBLIC: &str = "C0PUBLIC1";

/// Which app is in which channel, by (app, channel).
type Members = Arc<Mutex<Vec<(&'static str, String)>>>;
/// The channels whose `conversations.info` the new app asks and is not answered: with no status, never; with one, that
/// HTTP status and no body.
type Stalled = Arc<Mutex<Vec<(&'static str, Option<u16>)>>>;

/// `apps` for the private channel `#lab-private` and the public `#lab-general`, the old app in both.
async fn two_apps() -> (FakeSlack, Members, Stalled) {
    apps(vec![(PRIVATE.into(), "lab-private".into(), true), (PUBLIC.into(), "lab-general".into(), false)]).await
}

/// One workspace whose channels are `all` (id, name, private) with two Slack apps, `old` and `new`, told apart by the word
/// in their tokens' body: the old app is in every channel, the new app in none until `members` lists it. Slack's answers
/// to an app outside a channel, as its documentation gives them: a private channel is not listed, and its info, history
/// and a bot's post are `channel_not_found`; a public one is listed with `is_member: false`, its info answers, and its
/// history and a bot's post are `not_in_channel`. The person is in all. The new app's `conversations.info` of a channel
/// `stalled` lists is not answered as above.
async fn apps(all: Vec<(String, String, bool)>) -> (FakeSlack, Members, Stalled) {
    let members: Members = Arc::new(Mutex::new(all.iter().map(|(id, ..)| ("old", id.clone())).collect()));
    let stalled: Stalled = Arc::default();
    let (held, hung) = (members.clone(), stalled.clone());
    let fake = FakeSlack::start(Identity::Trusted, move |seen, port| {
        let method = seen.path().rsplit('/').next().unwrap_or_default().to_string();
        let auth = seen.authorization.clone().unwrap_or_default();
        let app: &'static str = if auth.contains("-new-") { "new" } else { "old" };
        let person = auth.starts_with("Bearer xoxp-");
        let wanted = seen.param("channel").or_else(|| seen.json().and_then(|j| j["channel"].as_str().map(str::to_string))).unwrap_or_default();
        let facts = |c: &str| all.iter().find(|f| f.0 == c);
        let member = |a: &str, c: &str| held.lock().unwrap().iter().any(|m| m.0 == a && m.1 == c);
        let refuse = |code: &str| Reply::json(&json!({"ok": false, "error": code}));
        let outside = |c: &str| refuse(if matches!(facts(c), Some((_, _, false))) { "not_in_channel" } else { "channel_not_found" });
        if seen.upgrade {
            let id: &'static str = if seen.param("ticket").as_deref() == Some("new") { "A0NEW" } else { "A0OLD" };
            return Reply::socket(move |mut peer| async move {
                peer.send(&json!({"type": "hello", "num_connections": 1, "connection_info": {"app_id": id}})).await;
                peer.drain().await;
            });
        }
        let (user, bot, app_id) = if app == "new" { ("U0NEW", "B0NEW", "A0NEW") } else { ("U0OLD", "B0OLD", "A0OLD") };
        let channel = |(id, name, private): &(String, String, bool)| json!({"id": id, "name": name, "is_private": private, "is_member": member(app, id)});
        match method.as_str() {
            "auth.test" if person => Reply::json(&json!({"ok": true, "team_id": "T0AAAA", "team": "Alpha Lab", "user_id": "U0PERSON"})),
            "auth.test" => Reply::json(&json!({"ok": true, "team_id": "T0AAAA", "team": "Alpha Lab", "user_id": user, "bot_id": bot})),
            // The old app's bot is named as the setup's manifest names it; the new one's answer carries no name.
            "bots.info" if app == "old" => Reply::json(&json!({"ok": true, "bot": {"id": bot, "app_id": app_id, "name": "Ship of Tools (lab-user)"}})),
            "bots.info" => Reply::json(&json!({"ok": true, "bot": {"id": bot, "app_id": app_id}})),
            "apps.connections.open" => Reply::json(&json!({"ok": true, "url": format!("wss://wss-primary.slack.com:{port}/link/?ticket={app}")})),
            "conversations.list" => {
                let listed: Vec<Value> = all.iter().filter(|(id, _, private)| !private || member(app, id)).map(channel).collect();
                Reply::json(&json!({"ok": true, "channels": listed, "response_metadata": {"next_cursor": ""}}))
            }
            "conversations.info" if app == "new" && hung.lock().unwrap().iter().any(|(c, _)| *c == wanted) => {
                let how = hung.lock().unwrap().iter().find(|(c, _)| *c == wanted).and_then(|(_, status)| *status);
                how.map_or(Reply::Hang, |code| Reply::Status { code, headers: Vec::new(), body: Vec::new() })
            }
            "conversations.info" => match facts(&wanted) {
                Some(f) if !f.2 || member(app, &f.0) => Reply::json(&json!({"ok": true, "channel": channel(f)})),
                _ => refuse("channel_not_found"),
            },
            "conversations.history" if member(app, &wanted) => Reply::json(&json!({"ok": true, "messages": [{"ts": "1700000001.000100", "user": "U0MEMBER", "text": "hello"}], "has_more": false})),
            "conversations.history" => outside(&wanted),
            "chat.postMessage" if person || member(app, &wanted) => Reply::json(&json!({"ok": true, "ts": "1700000999.000100"})),
            "chat.postMessage" => outside(&wanted),
            "users.info" => Reply::json(&json!({"ok": true, "user": {"profile": {"display_name": "Name"}}})),
            _ => refuse("unknown_method"),
        }
    })
    .await;
    (fake, members, stalled)
}

/// A window's connection to a hub whose one workspace `T0AAAA` maps each of `channels` from a folder of its own, each
/// folder a row's, in order, running the old app of `fake` (its codes' body `old`) with no user token held; with the
/// chat, and what must outlive the test.
struct OnApps {
    conn: Conn,
    rows: Vec<String>,
    chat: Arc<Chat>,
    _home: Home,
    _root: tempfile::TempDir,
}

async fn on_apps(fake: &FakeSlack, channels: &[&str]) -> OnApps {
    let on = on_apps_starting(fake, channels, starter(fake)).await;
    running(&on.chat).await;
    on
}

/// `on_apps` with the test's own `start`, returned while the chat may still be starting.
async fn on_apps_starting(fake: &FakeSlack, channels: &[&str], start: Starter<Service>) -> OnApps {
    let home = Home::new(Some(SELF));
    let folders: Vec<std::path::PathBuf> = (0..channels.len()).map(|n| home.folder(&format!("f{n}"))).collect();
    let listed: Vec<String> = channels.iter().map(|c| format!("\"{c}\"")).collect();
    let mappings: String = folders.iter().zip(channels).map(|(f, c)| format!("[[mapping]]\nfolder = '{}'\nchannel = \"{c}\"\n", f.display())).collect();
    home.write(CONFIG_FILE, &format!("workspace = \"T0AAAA\"\nchannels = [{}]\n{mappings}", listed.join(", ")));
    home.write(SECRETS_FILE, &format!("workspace = \"T0AAAA\"\nbot_token = \"{}\"\napp_token = \"{}\"\n", bot("old"), app("old")));
    window_on(fake, home, &folders, start).await
}

/// A window's connection to a hub with no chat file at all, whose checks and starts reach `fake`.
async fn on_nothing(fake: &FakeSlack) -> OnApps {
    window_on(fake, Home::new(Some(SELF)), &[], starter(fake)).await
}

/// The hub on `home` as it stands, each of `folders` a row's, booted with `start` and the fake's checks, and a window's
/// connection to it.
async fn window_on(fake: &FakeSlack, home: Home, folders: &[std::path::PathBuf], start: Starter<Service>) -> OnApps {
    let rows = Workspaces::new();
    let ids = folders.iter().enumerate().map(|(n, f)| rows.insert(row(&format!("r{n}"), f)).workspace_id.clone()).collect();
    let chat = Chat::boot(&home.env, &rows, &|p| config::read_file(p), &Registry::new(), start, Verifier::at(fake), HUB_CHECK, Timing::PRODUCT);
    let root = tempfile::tempdir().unwrap();
    let mut services = Services::for_test("t", root.path());
    (services.chat, services.workspaces) = (chat.clone(), rows);
    OnApps { conn: connect_window(services).await, rows: ids, chat, _home: home, _root: root }
}

/// The new app's three tokens (`two_apps`' word `new`), and whether the connect says to replace the app connected now.
fn the_new_app(replace: bool) -> Value {
    let mut req = connect_with(&bot("new"), &user("new"), &app("new"));
    if replace {
        req["replace_app"] = json!(true);
    }
    req
}

/// The calls of `method` the fake was asked under a bearer token whose body is `word`.
fn asked_by(fake: &FakeSlack, method: &str, word: &str) -> usize {
    fake.seen().iter().filter(|s| s.path().ends_with(method) && s.authorization.as_deref().is_some_and(|a| a.contains(&format!("-{word}-")))).count()
}

/// CHAT-CONNECT-MAKES-A-NEW-APP: a connect of another Slack app's tokens than the one the workspace runs with now (both
/// known by `bots.info`) is refused `other_app` with nothing written (every file of the config folder byte for byte) and
/// nothing restarted (no start under either app's tokens), its body naming the workspace, the app connected now by its id
/// and the name Slack gives its bot, and that the new app was told; the question is one `bots.info` under the new bot
/// token. The same request with `replace_app` replaces the app, as every connect did before.
#[tokio::test(flavor = "multi_thread")]
async fn another_apps_tokens_change_nothing_until_the_window_says_to_replace_the_connected_app() {
    let (fake, ..) = two_apps().await;
    let on = on_apps(&fake, &[PRIVATE, PUBLIC]).await;
    let mut conn = on.conn;
    let before = files(&on._home);
    // Each start of the old app's chat asks `auth.test` under its bot token (the connect's checks ask under the new ones).
    let old_starts = asked_by(&fake, "/auth.test", "old");
    let asked = conn.ask(3, op::CHAT_CONNECT, the_new_app(false)).await;
    holds_no_token("the refusal", &asked.to_string(), &["xoxb", "xoxp", "xapp", "444444444444"]);
    let mut body = asked.clone();
    assert_eq!(body.as_object_mut().and_then(|b| b.remove("error")), Some(json!(ChatCode::OtherApp.text())), "{asked}");
    assert_eq!(
        body,
        json!({"code": "other_app", "workspace": "T0AAAA", "name": "Alpha Lab", "app_id": "A0OLD", "app_name": "Ship of Tools (lab-user)", "new_app_known": true}),
        "the new app's bot has no name here, so the refusal names none"
    );
    assert!(files(&on._home) == before, "a connect asked about wrote a file");
    assert_eq!(asked_by(&fake, "/auth.test", "old"), old_starts, "a connect asked about restarted the old app's chat");
    assert_eq!(asked_by(&fake, "/bots.info", "new"), 1, "the question is one bots.info under the new bot token, and no start of its chat");
    let listed = conn.ask(4, op::CHAT_WORKSPACES, json!({})).await;
    assert_eq!((&listed["workspaces"][0]["running"], &listed["workspaces"][0]["app_id"]), (&json!(true), &json!("A0OLD")), "{listed}");
    // Told to replace it: the connect goes through as before, asking nothing more.
    assert_eq!(conn.ask(5, op::CHAT_CONNECT, the_new_app(true)).await, json!({"workspace": "T0AAAA", "name": "Alpha Lab"}));
    assert_ne!(files(&on._home), before, "the secrets were not written");
    assert_eq!(asked_by(&fake, "/bots.info", "new"), 3, "beside each connect's question, only the new app's chat's own start asks bots.info");
    let listed = conn.ask(6, op::CHAT_WORKSPACES, json!({})).await;
    assert_eq!((&listed["workspaces"][0]["running"], &listed["workspaces"][0]["app_id"]), (&json!(true), &json!("A0NEW")), "{listed}");
}

/// New tokens of the Slack app the workspace runs with now (the right repair of tokens Slack stopped accepting) are never
/// asked about: they are saved and the chat restarts with them.
#[tokio::test(flavor = "multi_thread")]
async fn the_connected_apps_new_tokens_are_never_asked_about() {
    let (fake, ..) = two_apps().await;
    let on = on_apps(&fake, &[PRIVATE, PUBLIC]).await;
    let mut conn = on.conn;
    let again = connect_with(&bot("old2"), &user("old2"), &app("old2"));
    assert_eq!(conn.ask(3, op::CHAT_CONNECT, again).await, json!({"workspace": "T0AAAA", "name": "Alpha Lab"}));
    let t: toml::Table = std::fs::read_to_string(on._home.config().join(SECRETS_FILE)).unwrap().parse().unwrap();
    assert_eq!(t["bot_token"].as_str(), Some(&*bot("old2")), "the new tokens were not saved");
    let listed = conn.ask(4, op::CHAT_WORKSPACES, json!({})).await;
    assert_eq!((&listed["workspaces"][0]["running"], &listed["workspaces"][0]["app_id"]), (&json!(true), &json!("A0OLD")), "{listed}");
}

/// A connect without `replace_app` writes only when no chat of its workspace runs or starts, or the running app is known
/// and is the tokens' app. So while the chat runs with an app its start could not tell (its `bots.info` failed), a connect
/// is asked about, nothing written, the refusal naming no app; asked about too while it runs with a known app (the
/// control); and the same tokens go through once the chat has ended because Slack refused its tokens, also in the moment
/// before its owner turned the unit off (held at `chat.owner.end`, the unit still `Running` the ended service).
#[tokio::test(flavor = "multi_thread")]
async fn a_connect_is_asked_about_while_a_chat_runs_untold_and_never_once_it_has_ended() {
    let home = Home::new(Some(SELF));
    home.configured();
    let w = world_on(home, &[public("C0PUB", true)], None, None).await;
    let mut conn = connect_window(w.services.clone()).await;
    // The start's bots.info failed: the running app is not told, so the connect is asked about and names none.
    assert_eq!(w.calls("/bots.info"), 1, "the start asked bots.info once");
    let (before, started) = (files(&w.home), w.starts());
    let mut asked = conn.ask(2, op::CHAT_CONNECT, connect_req(&bot("third"), &app("third"))).await;
    asked.as_object_mut().and_then(|r| r.remove("error"));
    assert_eq!(asked, json!({"code": "other_app", "workspace": "T0AAAA"}), "a running app that is not told was replaced unasked");
    assert!(files(&w.home) == before, "a connect asked about wrote a file");
    assert_eq!(w.starts() - asked_by(&w.fake, "/auth.test", "third"), started, "a connect asked about restarted the chat");
    assert_eq!(asked_by(&w.fake, "/bots.info", "third"), 1, "the connect's question");
    // Known, another app's tokens are asked about.
    let other = connect_req(&bot("other"), &app("other"));
    *w.slack.bot_app.lock().unwrap() = Some("A0OWN");
    let chat = w.services.chat.clone();
    w.home.rekey(SECRETS_FILE, "cccc");
    chat.reload_for(&chat.setup().await, &w.rows, "T0AAAA").await;
    assert_eq!(code(&conn.ask(3, op::CHAT_CONNECT, other.clone()).await), "other_app", "the control: a known app is asked about");
    // Slack refuses the bot token while the chat runs: it ends, and the same connect goes through, though the owner has not
    // yet turned the unit off.
    let owner = Holds::here().arm("chat.owner.end").key(Arc::as_ptr(chat.hub()) as usize).hold(1);
    *w.slack.list.lock().unwrap() = "token_revoked";
    conn.ask(4, op::CHAT_CHANNELS, json!({})).await;
    owner.reached_async(0).await;
    assert_eq!(chat.status(), Status::Running, "the unit no longer holds the ended service");
    *w.slack.list.lock().unwrap() = "ok";
    let connecting = tokio::spawn(async move {
        let reply = conn.ask(5, op::CHAT_CONNECT, other).await;
        (conn, reply)
    });
    // A connect asked about answers at once; one that goes through saves and reloads, whose stop waits for the owner.
    until("the connect's decision", || connecting.is_finished() || chat.status() == Status::Stopping).await;
    owner.release(0);
    let (_conn, reply) = tokio::time::timeout(std::time::Duration::from_secs(10), connecting).await.unwrap().unwrap();
    assert_eq!(reply, json!({"workspace": "T0AAAA"}), "an ended chat's app was asked about");
    assert_eq!(chat.status(), Status::Running);
}

/// A workspace's first connect is never asked about: no Slack app runs for it.
#[tokio::test(flavor = "multi_thread")]
async fn a_workspaces_first_connect_is_never_asked_about() {
    let w = world(None, &[]).await;
    *w.slack.bot_app.lock().unwrap() = Some("A0OWN");
    let mut conn = connect_window(w.services.clone()).await;
    assert_eq!(conn.ask(2, op::CHAT_CONNECT, connect_req(&bot("other"), &app("other"))).await, json!({"workspace": "T0AAAA"}));
    assert_eq!(w.services.chat.status(), Status::Running);
    assert_eq!(asked_by(&w.fake, "/bots.info", "other"), 2, "the connect's question and the new service's own start");
}

/// A hub whose workspace `T0AAAA` runs with the known Slack app `A0OWN` (its start's `bots.info` answered), on `checks`
/// (the fake's own when `None`).
async fn known_app(checks: Option<Verifier>) -> World {
    let home = Home::new(Some(SELF));
    home.configured();
    let w = world_on(home, &[public("C0PUB", true)], None, checks).await;
    *w.slack.bot_app.lock().unwrap() = Some("A0OWN");
    let chat = w.services.chat.clone();
    w.home.rekey(SECRETS_FILE, "cccc");
    chat.reload_for(&chat.setup().await, &w.rows, "T0AAAA").await;
    assert_eq!(chat.connected_app("T0AAAA").map(|a| a.id).as_deref(), Some("A0OWN"));
    w
}

/// CHAT-CONNECT-MAKES-A-NEW-APP: while the workspace runs with a known Slack app, a connect whose tokens' app Slack does
/// not tell (the connect's `bots.info` rate-limited, refused, a 500, a dropped connection, an answer naming no app) is
/// asked about all the same, never replacing the app unasked: refused `other_app`, nothing written and nothing restarted,
/// the body saying the new app was not told and naming none, and the log only the fixed code. With `replace_app` the same
/// tokens go through.
#[test]
fn a_connect_whose_tokens_app_slack_does_not_tell_is_asked_about_and_writes_nothing() {
    let mut replies = Vec::new();
    let text = logged(|| async {
        let w = known_app(None).await;
        // The chat's starts: `auth.test` under a bot token, but for the connect's own check of the new one.
        let starts = || w.starts() - asked_by(&w.fake, "/auth.test", "other");
        let (before, started) = (files(&w.home), starts());
        let mut conn = connect_window(w.services.clone()).await;
        let other = connect_req(&bot("other"), &app("other"));
        for how in ["ratelimited", "invalid_auth", "500", "drop", "noapp"] {
            *w.slack.other_bots.lock().unwrap() = how;
            let reply = conn.ask(3, op::CHAT_CONNECT, other.clone()).await;
            let mut body = reply.clone();
            body.as_object_mut().and_then(|b| b.remove("error"));
            assert_eq!(body, json!({"code": "other_app", "workspace": "T0AAAA", "app_id": "A0OWN"}), "bots.info answered {how}");
            assert_eq!(files(&w.home), before, "bots.info answered {how}: a connect asked about wrote a file");
            assert_eq!(starts(), started, "bots.info answered {how}: a connect asked about restarted the chat");
            replies.push(reply.to_string());
        }
        let mut anyway = other;
        anyway["replace_app"] = json!(true);
        assert_eq!(conn.ask(4, op::CHAT_CONNECT, anyway).await, json!({"workspace": "T0AAAA"}), "told to replace, the tokens go through");
        assert_ne!(files(&w.home), before);
    });
    assert!(text.contains("could not tell its tokens' Slack app"), "the capture saw no question");
    let bodies = ["444444444444", "555555555555", "666666666666"];
    holds_no_token("the daemon's log", &text, &bodies);
    for (n, reply) in replies.iter().enumerate() {
        holds_no_token(&format!("reply {n}"), reply, &bodies);
    }
}

/// Whether to ask is decided under the setup lock, from what is known there: a connect of another Slack app's tokens whose
/// checks ran while the workspace's app was not known (its start's `bots.info` failed), and whose lock a reload that
/// learned the app held meanwhile, is asked about, nothing written, and the app stays.
#[tokio::test(flavor = "multi_thread")]
async fn an_app_known_only_once_the_lock_is_taken_is_asked_about() {
    let home = Home::new(Some(SELF));
    home.configured();
    let w = world_on(home, &[public("C0PUB", true)], None, None).await;
    let chat = w.services.chat.clone();
    assert_eq!(chat.connected_app("T0AAAA"), None, "the start's bots.info failed");
    let mut conn = connect_window(w.services.clone()).await;
    let held = chat.setup().await;
    conn.send(2, op::CHAT_CONNECT, connect_req(&bot("other"), &app("other"))).await;
    // The App-Level Token's check is the connect's last before its question and the lock.
    until("the connect's checks", || asked_by(&w.fake, "/apps.connections.open", "other") == 1).await;
    *w.slack.bot_app.lock().unwrap() = Some("A0OWN");
    w.home.rekey(SECRETS_FILE, "cccc");
    chat.reload_for(&held, &w.rows, "T0AAAA").await;
    assert_eq!(chat.connected_app("T0AAAA").map(|a| a.id).as_deref(), Some("A0OWN"), "the reload learned the app");
    let before = files(&w.home);
    drop(held);
    let reply = conn.reply(2).await;
    assert_eq!(code(&reply), "other_app", "{reply}");
    assert!(files(&w.home) == before, "a connect asked about wrote a file");
    assert_eq!(chat.connected_app("T0AAAA").map(|a| a.id).as_deref(), Some("A0OWN"));
}

/// The calls of `bots.info` the fake was asked under the bot token the hub's secrets held at its boot (`Home::secrets`):
/// the running chat's own asks for its app.
fn own_asks(w: &World) -> usize {
    w.fake.seen().iter().filter(|s| s.path().ends_with("/bots.info") && s.authorization.as_deref().is_some_and(|a| a.ends_with("-aaaa"))).count()
}

/// A chat whose start could not tell its Slack app (its one `bots.info` failed then) does not leave every later connect
/// asked about: under the setup lock the connect asks for the running app once, through the chat's own token, and the
/// answer is kept for the service. So, once Slack answers: another app's codes get the question in its told form, naming
/// the running app, with one ask, and `chat.workspaces` names that app from then on; the same question again asks Slack
/// nothing more of the running app; and the running app's own new codes are saved with no question, at the first ask
/// too. While Slack still does not tell the running app, each connect asks again and is asked about, not told.
#[tokio::test(flavor = "multi_thread")]
async fn a_running_chat_whose_app_its_start_could_not_tell_is_asked_for_it_at_a_connect() {
    let untold = || async {
        let home = Home::new(Some(SELF));
        home.configured();
        let w = world_on(home, &[public("C0PUB", true)], None, None).await;
        assert_eq!((w.services.chat.connected_app("T0AAAA"), own_asks(&w)), (None, 1), "the start's bots.info failed");
        w
    };
    let w = untold().await;
    let mut conn = connect_window(w.services.clone()).await;
    let before = files(&w.home);
    let other = connect_req(&bot("other"), &app("other"));
    // Slack still does not tell the running app: each connect asks for it once more, and is asked about, not told.
    for asks in [2, 3] {
        let asked = conn.ask(2, op::CHAT_CONNECT, other.clone()).await;
        assert_eq!(refused_as(&asked, ChatCode::OtherApp), json!({"code": "other_app", "workspace": "T0AAAA"}));
        assert_eq!(own_asks(&w), asks, "a connect beside an untold running app did not ask for it once");
    }
    // Slack is well again, and both of the connect's questions name the other app: the told question, naming the running
    // app the connect's own ask learned.
    (*w.slack.bot_app.lock().unwrap(), *w.slack.hello_app.lock().unwrap()) = (Some("A0OWN"), Some("A0OTHER"));
    let told = json!({"code": "other_app", "workspace": "T0AAAA", "app_id": "A0OWN", "new_app_known": true});
    let asked = conn.ask(3, op::CHAT_CONNECT, other.clone()).await;
    assert_eq!(refused_as(&asked, ChatCode::OtherApp), told, "the running app was not asked for, or the question is not the told one");
    assert_eq!(own_asks(&w), 4);
    assert_eq!(w.services.chat.connected_app("T0AAAA").map(|a| a.id).as_deref(), Some("A0OWN"), "the answer was not kept for the service");
    let listed = conn.ask(4, op::CHAT_WORKSPACES, json!({})).await;
    assert_eq!((&listed["workspaces"][0]["running"], &listed["workspaces"][0]["app_id"]), (&json!(true), &json!("A0OWN")), "{listed}");
    // Kept: the same connect again is answered the same, with no ask more.
    assert_eq!(refused_as(&conn.ask(5, op::CHAT_CONNECT, other).await, ChatCode::OtherApp), told);
    assert_eq!(own_asks(&w), 4, "an app the service keeps was asked for again");
    assert!(files(&w.home) == before && w.services.chat.status() == Status::Running, "a connect asked about wrote a file or ended the chat");
    // The running app's own new codes: saved, no question.
    *w.slack.hello_app.lock().unwrap() = Some("A0OWN");
    assert_eq!(conn.ask(6, op::CHAT_CONNECT, connect_req(&bot("own"), &app("own"))).await, json!({"workspace": "T0AAAA"}));
    assert!(secrets_hold(&w.home, &bot("own"), &user("1"), &app("own")), "the running app's new codes were not saved");

    // The same codes at the first ask of a chat never asked before: saved, where every ask was refused alike.
    let w = untold().await;
    (*w.slack.bot_app.lock().unwrap(), *w.slack.hello_app.lock().unwrap()) = (Some("A0OWN"), Some("A0OWN"));
    let mut conn = connect_window(w.services.clone()).await;
    assert_eq!(conn.ask(2, op::CHAT_CONNECT, connect_req(&bot("own"), &app("own"))).await, json!({"workspace": "T0AAAA"}), "the running app's own codes were asked about");
    assert!(secrets_hold(&w.home, &bot("own"), &user("1"), &app("own")), "the running app's new codes were not saved");
    assert_eq!(w.services.chat.status(), Status::Running);
}

/// `setup::connect` run to its reply on a clock that is paused, which moves only to the next timer while nothing is ready:
/// how long the reply took on it, and the reply.
async fn on_a_paused_clock(chat: Arc<Chat>, rows: Workspaces, req: Value) -> (std::time::Duration, Value) {
    let paused = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().start_paused(true).build().unwrap();
        runtime.block_on(async move {
            let t0 = tokio::time::Instant::now();
            let out = crate::chat::setup::connect(2, req, &chat, &rows, true).await.unwrap();
            (t0.elapsed(), out[0].0.payload.clone())
        })
    });
    tokio::task::spawn_blocking(move || paused.join().unwrap()).await.unwrap()
}

/// The reply's worst case past the three checks, each of which is one read that ends the connect `failed` at its own
/// deadline: the two questions of the tokens' apps run at once, so a `bots.info` Slack never answers and a probe socket
/// that never says hello (here a check that holds for the probe's whole bound, `READ_DEADLINE`, and tells no app) together
/// hold the connect one read's deadline (15 s) and no longer, and then the person is asked, the app not told and nothing
/// written; a running chat whose app was never told is then asked for it under the lock, one read more (30 s in all when
/// Slack never answers that either); and a chat still starting is waited for under the lock, `SETUP_WAIT` (30 s) at most,
/// and then asked about with no app named (45 s in all). On a paused clock, the three checks answered at once.
#[tokio::test(flavor = "multi_thread")]
async fn stalled_questions_hold_the_reply_one_reads_deadline_each_and_a_start_the_setups_wait() {
    let (stalling, levers) = slack().await;
    *levers.bot_app.lock().unwrap() = Some("A0OWN");
    *levers.other_bots.lock().unwrap() = "hang";
    let deadline = sot_chat::api::READ_DEADLINE;
    let checks = || {
        let (http, base) = (stalling.http(), stalling.url("slack.com", "/api/"));
        let at_once = || sot_chat::api::Identified {
            workspace: "T0AAAA".into(),
            own: sot_chat::api::Identity { user_id: "U0BOT".into(), bot_id: Some("B0BOT".into()), app_id: None },
            name: None,
        };
        Verifier {
            identify: Box::new(move |_| Box::pin(async move { Ok(at_once()) })),
            app: Box::new(move |_| {
                Box::pin(async move {
                    tokio::time::sleep(deadline).await;
                    Ok(None)
                })
            }),
            bot_app: Box::new(move |token, bot| {
                let (http, base) = (http.clone(), base.clone());
                Box::pin(async move { sot_chat::api::bot_app_of_for_test(&http, token, &bot, base).await })
            }),
        }
    };
    let second = std::time::Duration::from_secs(1);
    // A chat that runs with a known app: the two stalled questions, at once.
    let w = known_app(Some(checks())).await;
    let before = files(&w.home);
    let (took, reply) = on_a_paused_clock(w.services.chat.clone(), w.rows.clone(), connect_req(&bot("other"), &app("other"))).await;
    assert_eq!(code(&reply), "other_app", "{reply}");
    assert_eq!((reply.get("app_id"), reply.get("new_app_known"), reply.get("new_app_name")), (Some(&json!("A0OWN")), None, None), "stalled questions tell no app: {reply}");
    assert!(took >= deadline && took < deadline + second, "the reply took {took:?} on the paused clock");
    assert!(files(&w.home) == before, "a connect asked about wrote a file");
    // A chat that runs with an app its start could not tell: the questions, then the ask for the running app, which Slack
    // never answers either, one read's deadline more; then the question with no app.
    let home = Home::new(Some(SELF));
    home.configured();
    let w = world_on(home, &[public("C0PUB", true)], None, Some(checks())).await;
    assert_eq!(w.services.chat.connected_app("T0AAAA"), None, "the start's bots.info failed");
    (*w.slack.bot_app.lock().unwrap(), *w.slack.own_bots.lock().unwrap()) = (Some("A0OWN"), "hang");
    let before = files(&w.home);
    let (took, reply) = on_a_paused_clock(w.services.chat.clone(), w.rows.clone(), connect_req(&bot("other"), &app("other"))).await;
    let mut body = reply.clone();
    body.as_object_mut().and_then(|r| r.remove("error"));
    assert_eq!(body, json!({"code": "other_app", "workspace": "T0AAAA"}), "a running app Slack never tells is asked about, not told");
    assert!(took >= 2 * deadline && took < 2 * deadline + second, "the reply took {took:?} on the paused clock");
    assert!(files(&w.home) == before && w.services.chat.status() == Status::Running, "a connect asked about wrote a file or ended the chat");
    // A chat whose start never ends: the questions, then the setup's wait for the start, then the question with no app.
    let home = Home::new(Some(SELF));
    home.configured();
    let (rows, before) = (Workspaces::new(), files(&home));
    let never: Starter<Service> = Box::new(|_, _, _| Box::pin(std::future::pending()));
    let chat = Chat::boot(&home.env, &rows, &|p| config::read_file(p), &Registry::new(), never, checks(), HUB_CHECK, Timing::PRODUCT);
    assert_eq!(chat.status(), Status::Starting);
    let (took, reply) = on_a_paused_clock(chat.clone(), rows, connect_req(&bot("other"), &app("other"))).await;
    let mut body = reply.clone();
    body.as_object_mut().and_then(|r| r.remove("error"));
    assert_eq!(body, json!({"code": "other_app", "workspace": "T0AAAA"}), "a chat still starting is asked about, its app not told");
    let worst = deadline + crate::chat::SETUP_WAIT;
    assert!(took >= worst && took < worst + second, "the reply took {took:?} on the paused clock");
    assert!(files(&home) == before && chat.status() == Status::Starting, "a connect asked about wrote a file or ended the start");
}

/// The connect's reply without its fixed text, and that the text is `code`'s.
fn refused_as(reply: &Value, code: ChatCode) -> Value {
    let mut body = reply.clone();
    assert_eq!(body.as_object_mut().and_then(|b| b.remove("error")), Some(json!(code.text())), "{reply}");
    body
}

/// No set of tokens that Slack tells to be two Slack apps' is written, whatever the request says and whatever runs: the
/// app-level token's app (its probe socket's hello) is compared with the bot token's (`bots.info`) on every connect. Each
/// set a window's keys can send beside a running chat is refused `mixed_apps`, with the wire's words and no body, nothing
/// written (every file of the config folder byte for byte), nothing restarted, and the old chat still running with its
/// app: the connected app's two codes beside the new app's App-Level Token (what a reflex paste over the question screen
/// sends, or one after Enter there, or one in a step opened again after Escape, a session change or a lost link); the
/// connected app's bot code beside the new app's other two (after the person chose to connect the new app and Slack
/// refused its bot code); the new app's two codes beside the connected app's App-Level Token; and each of them saying
/// `replace_app`. A first connect of such a set, with no chat to replace, is refused the same and writes nothing. No
/// probe socket is left open.
#[test]
fn a_set_of_two_slack_apps_tokens_is_never_written() {
    let mut replies = Vec::new();
    let text = logged(|| async {
        let (fake, ..) = two_apps().await;
        let on = on_apps(&fake, &[PRIVATE, PUBLIC]).await;
        let mut conn = on.conn;
        // Each start of the old app's chat asks `auth.test` under its saved bot token.
        let saved = format!("Bearer {}", bot("old"));
        let starts = || fake.seen().iter().filter(|s| s.path().ends_with("/auth.test") && s.authorization.as_deref() == Some(&*saved)).count();
        let (before, old_starts) = (files(&on._home), starts());
        // The connected app's own tokens under other bodies, so each check below is of a set never seen before.
        let sets = [
            ("the connected app's two codes and the new app's App-Level Token", bot("old-a"), user("old-a"), app("new")),
            ("the connected app's bot code and the new app's other two", bot("old-b"), user("new"), app("new")),
            ("the new app's two codes and the connected app's App-Level Token", bot("new"), user("new"), app("old-c")),
        ];
        for (what, b, u, a) in &sets {
            for replace in [false, true] {
                let mut req = connect_with(b, u, a);
                if replace {
                    req["replace_app"] = json!(true);
                }
                let reply = conn.ask(3, op::CHAT_CONNECT, req).await;
                assert_eq!(refused_as(&reply, ChatCode::MixedApps), json!({"code": "mixed_apps"}), "{what}, replace_app {replace}");
                assert!(files(&on._home) == before, "{what}, replace_app {replace}: a mixed set wrote a file");
                assert_eq!(starts(), old_starts, "{what}, replace_app {replace}: a mixed set restarted the chat");
                replies.push(reply.to_string());
            }
        }
        // The old chat still runs, with its app; the new app's chat never started (its own start would ask `bots.info`
        // beside the connects' questions, one for each connect whose bot token is the new app's).
        let listed = conn.ask(4, op::CHAT_WORKSPACES, json!({})).await;
        assert_eq!((&listed["workspaces"][0]["running"], &listed["workspaces"][0]["app_id"]), (&json!(true), &json!("A0OLD")), "{listed}");
        assert_eq!(on.chat.status(), Status::Running);
        assert_eq!(asked_by(&fake, "/bots.info", "new"), 2, "a start under the new app's bot token");
        sot_chat::fake_slack::eventually("the probes' sockets gone", || fake.sockets().0 == 1).await;
        assert_eq!(fake.sockets().2, 7, "the chat's one socket and one probe a connect");

        // A first connect of a mixed set: nothing runs, and nothing is written all the same.
        let first = on_nothing(&fake).await;
        let mut conn = first.conn;
        let before = files(&first._home);
        let reply = conn.ask(5, op::CHAT_CONNECT, connect_with(&bot("old"), &user("old"), &app("new"))).await;
        assert_eq!(refused_as(&reply, ChatCode::MixedApps), json!({"code": "mixed_apps"}), "a first connect");
        assert!(files(&first._home) == before, "a first connect of a mixed set wrote a file");
        assert_eq!(first.chat.status(), Status::Refused(config::ConfigError::Missing));
        replies.push(reply.to_string());
    });
    assert!(text.contains("another Slack app's than its bot token's"), "the capture saw no mixed set");
    let bodies = ["444444444444", "555555555555", "666666666666", "xoxb", "xoxp", "xapp"];
    holds_no_token("the daemon's log", &text, &bodies);
    for (n, reply) in replies.iter().enumerate() {
        holds_no_token(&format!("reply {n}"), reply, &bodies);
    }
}

/// One Slack app's three tokens still connect: a first time from no chat file (nothing to replace, so nothing asked),
/// again while that app's chat runs (both sides tell the running app), and its newly made tokens too; each is saved.
#[tokio::test(flavor = "multi_thread")]
async fn one_slack_apps_three_tokens_connect_a_first_time_and_again() {
    let (fake, ..) = two_apps().await;
    let on = on_nothing(&fake).await;
    let mut conn = on.conn;
    let connected = json!({"workspace": "T0AAAA", "name": "Alpha Lab"});
    assert_eq!(conn.ask(2, op::CHAT_CONNECT, the_new_app(false)).await, connected, "a first connect");
    assert!(secrets_hold(&on._home, &bot("new"), &user("new"), &app("new")), "the first connect's tokens were not saved");
    assert_eq!(on.chat.connected_app("T0AAAA").map(|a| a.id).as_deref(), Some("A0NEW"));
    assert_eq!(conn.ask(3, op::CHAT_CONNECT, the_new_app(false)).await, connected, "the same three again");
    let again = (bot("new-2"), user("new-2"), app("new-2"));
    assert_eq!(conn.ask(4, op::CHAT_CONNECT, connect_with(&again.0, &again.1, &again.2)).await, connected, "the app's new tokens");
    assert!(secrets_hold(&on._home, &again.0, &again.1, &again.2), "the app's new tokens were not saved");
    assert_eq!((on.chat.status(), on.chat.connected_app("T0AAAA").map(|a| a.id).as_deref()), (Status::Running, Some("A0NEW")));
}

/// A connect that comes while its workspace's chat is still starting waits for the start under the setup lock and is then
/// decided against the app that start told: another app's tokens are asked about, nothing written, and the app that was
/// starting runs.
#[tokio::test(flavor = "multi_thread")]
async fn a_connect_during_a_start_waits_for_it_and_is_asked_about() {
    let (fake, ..) = two_apps().await;
    let (gate, start) = (Arc::new(tokio::sync::Semaphore::new(0)), Arc::new(starter(&fake)));
    let opened = gate.clone();
    let held: Starter<Service> = Box::new(move |workspace, credentials, dir| {
        let (gate, start) = (opened.clone(), start.clone());
        Box::pin(async move {
            gate.acquire().await.unwrap().forget();
            start(workspace, credentials, dir).await
        })
    });
    let on = on_apps_starting(&fake, &[PRIVATE], held).await;
    let (mut conn, chat) = (on.conn, on.chat.clone());
    assert_eq!(chat.status(), Status::Starting);
    let before = files(&on._home);
    conn.send(3, op::CHAT_CONNECT, the_new_app(false)).await;
    // The connect's checks are done and it holds the setup lock, where it waits for the start.
    until("the connect's checks", || asked_by(&fake, "/bots.info", "new") == 1 && fake.sockets() == (0, 1, 1)).await;
    loop {
        if tokio::time::timeout(std::time::Duration::ZERO, chat.setup()).await.is_err() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(chat.status() == Status::Starting && files(&on._home) == before, "a connect during a start wrote, or ended the start");
    gate.add_permits(8);
    let reply = conn.reply(3).await;
    assert_eq!(
        refused_as(&reply, ChatCode::OtherApp),
        json!({"code": "other_app", "workspace": "T0AAAA", "name": "Alpha Lab", "app_id": "A0OLD", "app_name": "Ship of Tools (lab-user)", "new_app_known": true}),
        "the app that was starting was replaced unasked"
    );
    assert!(files(&on._home) == before, "a connect asked about wrote a file");
    assert_eq!((chat.status(), chat.connected_app("T0AAAA").map(|a| a.id).as_deref()), (Status::Running, Some("A0OLD")));
}

/// With a chat running, tokens whose two apps Slack does not both tell are not known to be its app's: asked about, nothing
/// written, though the bot token is the running app's; both told to be the running app, they are saved with no question;
/// and told to be two apps, they are a mixed set. With no chat to replace, tokens whose apps are not told are saved (the
/// listener ends `wrong_app` if they are two apps').
#[tokio::test(flavor = "multi_thread")]
async fn tokens_whose_apps_are_not_both_told_are_asked_about_only_while_a_chat_runs() {
    let w = known_app(None).await;
    let before = files(&w.home);
    let mut conn = connect_window(w.services.clone()).await;
    // The bot token is the running app's by `bots.info`; the probe socket's hello names no app.
    let own = |n: &str| connect_req(&bot(&format!("own{n}")), &app(&format!("own{n}")));
    let asked = conn.ask(2, op::CHAT_CONNECT, own("1")).await;
    assert_eq!(refused_as(&asked, ChatCode::OtherApp), json!({"code": "other_app", "workspace": "T0AAAA", "app_id": "A0OWN"}), "an App-Level Token whose app is not told");
    assert!(files(&w.home) == before, "a connect asked about wrote a file");
    // The hello names another app than the bot token's: a mixed set.
    *w.slack.hello_app.lock().unwrap() = Some("A0OTHER");
    assert_eq!(code(&conn.ask(3, op::CHAT_CONNECT, own("2")).await), "mixed_apps");
    assert!(files(&w.home) == before, "a mixed set wrote a file");
    // Both told, and the running app: saved, no question.
    *w.slack.hello_app.lock().unwrap() = Some("A0OWN");
    assert_eq!(conn.ask(4, op::CHAT_CONNECT, own("3")).await, json!({"workspace": "T0AAAA"}));
    assert!(secrets_hold(&w.home, &bot("own3"), &user("1"), &app("own3")), "the running app's new tokens were not saved");
    // No chat to replace: a first connect whose apps are not told (no `bots.info`, a hello that names none) is saved.
    let first = world(None, &[]).await;
    let mut conn = connect_window(first.services.clone()).await;
    assert_eq!(conn.ask(5, op::CHAT_CONNECT, connect_req(&bot("1"), &app("1"))).await, json!({"workspace": "T0AAAA"}));
    assert_eq!(first.services.chat.status(), Status::Running);
}

/// CHAT-CONNECT-MAKES-A-NEW-APP: `chat.workspaces` gives the Slack app a running workspace is connected to by the name
/// `bots.info` gave its bot at the start and by the app's id, so a window can say which app to keep and open its page;
/// after a connect that replaces it with another app it gives that app's (here no name: its `bots.info` gives none),
/// never the one before.
#[tokio::test(flavor = "multi_thread")]
async fn the_workspaces_read_names_the_connected_slack_app() {
    let (fake, ..) = two_apps().await;
    let on = on_apps(&fake, &[PRIVATE, PUBLIC]).await;
    let mut conn = on.conn;
    let listed = conn.ask(2, op::CHAT_WORKSPACES, json!({})).await;
    assert_eq!((&listed["workspaces"][0]["id"], &listed["workspaces"][0]["running"]), (&json!("T0AAAA"), &json!(true)), "{listed}");
    assert_eq!((&listed["workspaces"][0]["app_name"], &listed["workspaces"][0]["app_id"]), (&json!("Ship of Tools (lab-user)"), &json!("A0OLD")), "{listed}");
    let connected = conn.ask(3, op::CHAT_CONNECT, the_new_app(true)).await;
    assert_eq!(connected["workspace"], "T0AAAA", "{connected}");
    let listed = conn.ask(4, op::CHAT_WORKSPACES, json!({})).await;
    assert_eq!((&listed["workspaces"][0]["running"], &listed["workspaces"][0]["app_id"]), (&json!(true), &json!("A0NEW")), "{listed}");
    assert_eq!(listed["workspaces"][0].get("app_name"), None, "the app before is not named for the new one: {listed}");
}

/// The next `chat.status` on `conn` that names `channel` for a new snapshot, past every other frame.
async fn resync_of(conn: &mut Conn, channel: &str) -> Value {
    let wait = async {
        loop {
            let (frame, _) = codec::read_frame(&mut conn.crx).await.unwrap();
            if frame.kind == Kind::Evt && frame.op == op::CHAT_STATUS && frame.payload["resync"].as_array().is_some_and(|r| r.iter().any(|c| c == channel)) {
                return frame.payload;
            }
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), wait).await.expect("a chat.status naming the channel")
}

/// Asks with ids from `first` until `done` takes a reply, each ask a round trip to the daemon, so the wait is on its
/// state; within 10 s.
async fn ask_until(conn: &mut Conn, first: u64, name: &str, payload: Value, mut done: impl FnMut(&Value) -> bool) -> Value {
    let wait = async {
        for id in first.. {
            let answer = conn.ask(id, name, payload.clone()).await;
            if done(&answer) {
                return answer;
            }
        }
        unreachable!("the ids run out")
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), wait).await.expect("the daemon's state was reached")
}

/// A workspace connected again with another Slack app, which is in neither mapped channel (the earlier secrets held no user
/// token, so the Connect step had the person make one): once the connect's task has read them, `chat.workspaces` names
/// both channels for the workspace, with the names the first app had for them; the private channel's watch is refused `not_in_channel` instead of staying pending, and its history
/// and the public one's are `not_in_channel`, never `failed`; the new app's list lacks the private channel, and its pasted
/// link is `no_such_channel`, whose text says it may be private. Once the app is invited, a watch's snapshot succeeds and
/// the next watch is complete.
#[tokio::test(flavor = "multi_thread")]
async fn a_new_slack_app_outside_a_mapped_channel_is_told_not_in_channel_until_it_is_invited() {
    let (fake, members, _) = two_apps().await;
    let on = on_apps(&fake, &[PRIVATE, PUBLIC]).await;
    let (mut conn, alpha, beta) = (on.conn, on.rows[0].clone(), on.rows[1].clone());
    let target = |row: &str, channel: &str| json!({"row": row, "workspace": "T0AAAA", "channel": channel});
    let history = |row: &str, channel: &str| json!({"target": target(row, channel), "limit": 5});
    assert_eq!(conn.ask(2, op::CHAT_HISTORY, history(&alpha, PRIVATE)).await["records"][0]["text"], "hello", "the first app reads it");
    // The start's lookup of the mapped channels' names ends first.
    for row in [&alpha, &beta] {
        ask_until(&mut conn, 1000, op::CHAT_RESOLVE, json!({"row": row}), |r| r.get("name").is_some()).await;
    }
    let connected = conn.ask(3, op::CHAT_CONNECT, the_new_app(true)).await;
    assert_eq!(connected, json!({"workspace": "T0AAAA", "name": "Alpha Lab"}));
    let listed = ask_until(&mut conn, 20_000, op::CHAT_WORKSPACES, json!({}), |l| l["workspaces"][0].get("unread").is_some()).await;
    let unread = json!([{"id": PRIVATE, "name": "lab-private"}, {"id": PUBLIC, "name": "lab-general"}]);
    assert_eq!(listed["workspaces"][0]["unread"], unread, "{listed}");

    // The watch's snapshot is refused; a snapshot fast enough to fail before the reply is answered already, else the failure
    // is told by a resync and the next watch answers it.
    let watch = json!({"target": target(&alpha, PRIVATE)});
    let mut watched = conn.ask(10, op::CHAT_WATCH, watch.clone()).await;
    if watched == json!({"state": "pending"}) {
        resync_of(&mut conn, PRIVATE).await;
        watched = conn.ask(11, op::CHAT_WATCH, watch.clone()).await;
    }
    assert_eq!(code(&watched), "not_in_channel", "{watched}");
    assert_eq!(code(&conn.ask(200, op::CHAT_HISTORY, history(&alpha, PRIVATE)).await), "not_in_channel");
    assert_eq!(code(&conn.ask(201, op::CHAT_HISTORY, history(&beta, PUBLIC)).await), "not_in_channel");
    let listed = conn.ask(202, op::CHAT_CHANNELS, json!({})).await;
    assert_eq!(listed["channels"].as_array().map(|l| l.iter().map(|c| c["id"].clone()).collect::<Vec<_>>()), Some(vec![json!(PUBLIC)]), "{listed}");
    let pasted = conn.ask(203, op::CHAT_PAIR, json!({"row": alpha, "channel": PRIVATE})).await;
    assert_eq!(code(&pasted), "no_such_channel", "{pasted}");
    assert!(pasted["error"].as_str().is_some_and(|e| e.contains("or it is a private channel your Slack app is not in")), "{pasted}");

    // Invited: each watch begins a snapshot while the refusal stands, the first that succeeds ends it, and the next watch
    // is complete.
    members.lock().unwrap().push(("new", PRIVATE.into()));
    let watched = ask_until(&mut conn, 300, op::CHAT_WATCH, watch, |w| {
        let complete = *w == json!({"state": "complete"});
        assert!(complete || code(w) == "not_in_channel" || *w == json!({"state": "pending"}), "{w}");
        complete
    })
    .await;
    assert_eq!(watched, json!({"state": "complete"}), "the refusal ends with the first snapshot that succeeds");
    assert_eq!(conn.ask(9000, op::CHAT_HISTORY, history(&alpha, PRIVATE)).await["records"][0]["text"], "hello");
}

/// B1: the connect's reply waits on no Slack read of the mapped channels. With the new app's `conversations.info` of both
/// never answered, the reply comes within its checks' and its reload's own time, and the workspace's entry in
/// `chat.workspaces` says nothing of the channels its app cannot read while that is not known.
#[tokio::test(flavor = "multi_thread")]
async fn a_connect_replies_before_its_mapped_channels_are_read() {
    let (fake, _, stalled) = two_apps().await;
    stalled.lock().unwrap().extend([(PRIVATE, None), (PUBLIC, None)]);
    let on = on_apps(&fake, &[PRIVATE, PUBLIC]).await;
    let mut conn = on.conn;
    let asked = std::time::Instant::now();
    let connected = conn.ask(3, op::CHAT_CONNECT, the_new_app(true)).await;
    let took = asked.elapsed();
    assert_eq!(connected, json!({"workspace": "T0AAAA", "name": "Alpha Lab"}));
    assert!(took < std::time::Duration::from_secs(3), "the reply waited {took:?} for Slack's reads of the mapped channels");
    let listed = conn.ask(4, op::CHAT_WORKSPACES, json!({})).await;
    assert_eq!(listed["workspaces"][0].get("unread"), None, "not known yet: {listed}");
}

/// The reads of a connect's task end at their bound in time: a channel whose `conversations.info` is never answered is cut
/// off at the chat's wait (shortened here), and what was found by then is told, since a list that names a channel is true
/// however it ends; a cut that found none leaves the list not known, and so does a read Slack refused (here a rate limit on
/// the first channel's, which holds the second's too) that found none, so an empty list only ever means every mapped channel
/// was read and is readable.
#[tokio::test(flavor = "multi_thread")]
async fn the_reads_of_the_channels_an_app_cannot_read_end_at_their_bound() {
    // One wait covers every read of the connect by design, so the answered read of the other channel must finish inside
    // it (a read takes about 44 ms here): 1 s leaves it 20 times that.
    let wait = std::time::Duration::from_secs(1);
    let cases = [(PUBLIC, None, Some(json!([{"id": PRIVATE, "name": "lab-private"}]))), (PRIVATE, None, None), (PRIVATE, Some(429), None)];
    for (stall, status, told) in cases {
        let (fake, _, stalled) = two_apps().await;
        stalled.lock().unwrap().push((stall, status));
        let on = on_apps(&fake, &[PRIVATE, PUBLIC]).await;
        on.chat.set_unread_wait(wait);
        let arm = Holds::here().arm("chat.unread.found").key(Arc::as_ptr(&on.chat) as usize).hold(1);
        let mut conn = on.conn;
        conn.ask(3, op::CHAT_CONNECT, the_new_app(true)).await;
        let replied = std::time::Instant::now();
        arm.reached_async(0).await;
        let took = replied.elapsed();
        assert!(took < wait * 4, "{stall} stalled: the reads ended {took:?} after the reply, past their bound of {wait:?}");
        arm.release(0);
        let listed = conn.ask(4, op::CHAT_WORKSPACES, json!({})).await;
        assert_eq!(listed["workspaces"][0].get("unread"), told.as_ref(), "{stall} stalled ({status:?}): {listed}");
    }
}

/// What a connect's task keeps is bounded: at most `MAX_WATCHED` channels, each name within `MAX_NAME` bytes, however many
/// mapped channels the new app cannot read and however long Slack's names for them are.
#[tokio::test(flavor = "multi_thread")]
async fn the_channels_an_app_cannot_read_are_at_most_max_watched_with_bounded_names() {
    use sot_protocol::{chat::MAX_NAME, ops::MAX_WATCHED};
    let ids: Vec<String> = (0..=MAX_WATCHED).map(|n| format!("C0LONG{n:04}")).collect();
    let (fake, ..) = apps(ids.iter().map(|id| (id.clone(), format!("{}-{}", id.to_lowercase(), "x".repeat(300)), false)).collect()).await;
    let on = on_apps(&fake, &ids.iter().map(String::as_str).collect::<Vec<_>>()).await;
    let arm = Holds::here().arm("chat.unread.found").key(Arc::as_ptr(&on.chat) as usize).hold(1);
    let mut conn = on.conn;
    conn.ask(3, op::CHAT_CONNECT, the_new_app(true)).await;
    arm.reached_async(0).await;
    arm.release(0);
    let listed = conn.ask(4, op::CHAT_WORKSPACES, json!({})).await;
    let unread = listed["workspaces"][0]["unread"].as_array().cloned().unwrap_or_default();
    assert_eq!(unread.len(), MAX_WATCHED, "of {} channels the app cannot read", ids.len());
    for u in &unread {
        let name = u["name"].as_str().unwrap_or_default();
        assert!(!name.is_empty() && name.len() <= MAX_NAME, "{}: a name of {} bytes", u["id"], name.len());
    }
}

/// An older connect's task, ending after a newer connect's, keeps nothing: the list belongs to the service it was read for, so
/// the newer connect's known list (here empty: its app reads both channels) is not replaced by one for a service that has
/// stopped, which `chat.workspaces` would tell as not known. The older task is held between its reads and its keeping
/// (`chat.unread.read`) until the newer connect's list is kept (`chat.unread.found`).
#[tokio::test(flavor = "multi_thread")]
async fn an_older_connects_task_never_replaces_a_newer_connects_list() {
    let (fake, ..) = two_apps().await;
    let on = on_apps(&fake, &[PRIVATE, PUBLIC]).await;
    let key = Arc::as_ptr(&on.chat) as usize;
    let read = Holds::here().arm("chat.unread.read").key(key).hold(1);
    let found = Holds::here().arm("chat.unread.found").key(key).hold(0);
    let mut conn = on.conn;
    // The first connect: the new app, which is in neither channel; its reads end and its task is held before it keeps them.
    conn.ask(3, op::CHAT_CONNECT, the_new_app(true)).await;
    read.reached_async(0).await;
    // The second connect: an app in both, whose reload stops the first's service; its task keeps its list.
    let mut old = connect_with(&bot("old"), &user("old"), &app("old"));
    old["replace_app"] = json!(true);
    conn.ask(4, op::CHAT_CONNECT, old).await;
    found.reached_async(0).await;
    let listed = conn.ask(5, op::CHAT_WORKSPACES, json!({})).await;
    assert_eq!(listed["workspaces"][0]["unread"], json!([]), "the newer connect's list: {listed}");
    // The first connect's task goes on and keeps nothing.
    read.release(0);
    found.reached_async(1).await;
    let listed = conn.ask(6, op::CHAT_WORKSPACES, json!({})).await;
    assert_eq!(listed["workspaces"][0]["unread"], json!([]), "the older task replaced it: {listed}");
}

/// What a connect's task found is told only while the service it read through runs: a reload that starts the workspace
/// again (here a pairing that unmaps one project) says nothing of it, since that service may read other channels.
#[tokio::test(flavor = "multi_thread")]
async fn what_a_connects_task_found_goes_with_the_service_it_read_through() {
    let (fake, ..) = two_apps().await;
    let on = on_apps(&fake, &[PRIVATE, PUBLIC]).await;
    let arm = Holds::here().arm("chat.unread.found").key(Arc::as_ptr(&on.chat) as usize).hold(1);
    let (mut conn, public_row) = (on.conn, on.rows[1].clone());
    conn.ask(3, op::CHAT_CONNECT, the_new_app(true)).await;
    arm.reached_async(0).await;
    arm.release(0);
    let listed = conn.ask(4, op::CHAT_WORKSPACES, json!({})).await;
    assert_eq!(listed["workspaces"][0]["unread"].as_array().map(Vec::len), Some(2), "{listed}");
    assert_eq!(conn.ask(5, op::CHAT_PAIR, json!({"row": public_row, "channel": null})).await, json!({}));
    let listed = conn.ask(6, op::CHAT_WORKSPACES, json!({})).await;
    assert_eq!((&listed["workspaces"][0]["running"], listed["workspaces"][0].get("unread")), (&json!(true), None), "{listed}");
}
