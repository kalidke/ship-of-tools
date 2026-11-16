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
    let state = Arc::new(Slack { channels: Mutex::new(Vec::new()), join: Mutex::new("ok"), auth: Mutex::new("ok"), user: Mutex::new("ok"), user_team: Mutex::new("T0AAAA"), open: Mutex::new("ok"), feed });
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
        } else if path.ends_with("/conversations.list") {
            let listed: Vec<Value> = held.channels.lock().unwrap().iter().filter(|c| !c.2 || c.3).copied().map(channel_json).collect();
            Reply::json(&json!({"ok": true, "channels": listed, "response_metadata": {"next_cursor": ""}}))
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
            let rx = rx.clone();
            Reply::socket(move |mut peer| {
                let rx = rx.clone();
                async move {
                    peer.send(&json!({"type": "hello", "num_connections": 1})).await;
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
            Reply::json(&json!({"ok": false, "error": "invalid_auth"}))
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
        until("the snapshot", || probe.conn.state("C0PUB") == sot_protocol::ops::ChatWatchState::Complete).await;
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
        assert_eq!(loaded.mappings().collect::<Vec<_>>(), [(w.folders[0].to_str().unwrap(), "C0PUB")]);
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
async fn a_connect_naming_another_workspace_is_refused() {
    let home = Home::new(Some(SELF));
    home.write(CONFIG_FILE, "workspace = \"T0OTHER\"\nchannels = []\n");
    let w = world_on(home, &[], None, None).await;
    let before = files(&w.home);
    let mut conn = connect_window(w.services.clone()).await;
    assert_eq!(code(&conn.ask(2, op::CHAT_CONNECT, connect_req(&bot("1"), &app("1"))).await), "other_workspace");
    assert_eq!(files(&w.home), before);
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
    assert_eq!(resolve(world_on(home, &[], Some(failing), None).await).await, unconfigured("start_failed"));
    // A refused chat.toml stays `invalid`: a connect never rewrites it.
    let home = Home::new(Some(SELF));
    home.write(CONFIG_FILE, "workspace = [[[");
    assert_eq!(resolve(world_on(home, &[], None, None).await).await, json!({"state": "invalid"}));
    // The row before this: a row that is gone is `stale_row`, whatever the chat's state.
    let w = world(None, &[]).await;
    w.rows.remove_by_id(&w.ids[0]);
    assert_eq!(code(&resolve(w).await), "stale_row");
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

/// A hub whose secrets file was written before the user token (three keys) runs: resolve says `no_user_token`, a
/// person's post is refused with no `chat.postMessage` sent, a row's post goes out, and the load warned once.
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
        let resolved = conn.ask(2, op::CHAT_RESOLVE, json!({"row": w.ids[0]})).await;
        assert_eq!(resolved, json!({"state": "unconfigured", "reason": "no_user_token"}));
        let sent = |w: &World| w.calls("/chat.postMessage");
        let person = json!({"target": w.target(0, "C0AAAA"), "kind": "person", "name": "tester", "text": "hello"});
        let refused = conn.ask(3, op::CHAT_POST, person).await;
        assert_eq!((refused["outcome"].as_str(), refused["code"].as_str()), (Some("refused"), Some("no_user_token")), "{refused}");
        assert_eq!(sent(&w), 0, "a person's post reached Slack with no user token");
        let row = json!({"target": w.target(0, "C0AAAA"), "kind": "row", "name": "alpha-h", "text": "done"});
        let posted = conn.ask(4, op::CHAT_POST, row).await;
        assert_eq!(posted["outcome"], "posted", "{posted}");
        assert_eq!(sent(&w), 1, "a row's post is the app's, and goes out");
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
        until("the snapshot", || probe.conn.state("C0PUB") == sot_protocol::ops::ChatWatchState::Complete).await;

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
