//! The setup ops that need a running chat (`chat.channels`, `chat.pair`) through real connections, against the exported
//! fake Slack: its channels, its join and a live socket.

use super::chat_ops_tests::{code, connect, connect_window, running, starter, Conn};
use super::chat_watch_tests::{after_hello, message, stamp, until};
use super::*;
use crate::chat::tests::{row, Home, SELF};
use crate::chat::{Chat, Status, HUB_CHECK};
use crate::lifecycle::retire::Registry;
use serde_json::{json, Value};
use sot_chat::config::{self, Writers, CONFIG_FILE, SECRETS_FILE};
use sot_chat::delivery::Timing;
use sot_chat::fake_slack::{FakeSlack, Identity, Reply};
use sot_chat::http::Secret;
use sot_protocol::ops::{ChatChannel, ChatTarget};
use std::sync::Mutex;

/// A channel of the fake workspace: id, name, private, whether the app is in it.
type Fact = (&'static str, &'static str, bool, bool);

/// What the fake Slack holds: its channels, and what a join answers (`ok`, Slack's error code, or `500` for an answer
/// that is no acknowledgement).
struct Slack {
    channels: Mutex<Vec<Fact>>,
    join: Mutex<&'static str>,
    feed: tokio::sync::mpsc::UnboundedSender<Value>,
}

fn channel_json((id, name, private, member): Fact) -> Value {
    json!({"id": id, "name": name, "is_private": private, "is_member": member})
}

/// A workspace `T0AAAA` whose list shows a private channel only to a member, whose info shows any, and whose socket
/// says hello and then sends what `feed` is given.
async fn slack() -> (FakeSlack, Arc<Slack>) {
    let (feed, rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let state = Arc::new(Slack { channels: Mutex::new(Vec::new()), join: Mutex::new("ok"), feed });
    let (held, rx) = (state.clone(), Arc::new(tokio::sync::Mutex::new(rx)));
    let fake = FakeSlack::start(Identity::Trusted, move |seen, port| {
        let path = seen.path();
        let known = |id: &str| held.channels.lock().unwrap().iter().copied().find(|c| c.0 == id);
        let wanted = seen.param("channel").or_else(|| seen.json().and_then(|j| j["channel"].as_str().map(str::to_string))).unwrap_or_default();
        if path.ends_with("/auth.test") {
            Reply::json(&json!({"ok": true, "team_id": "T0AAAA", "user_id": "U0BOT", "bot_id": "B0BOT"}))
        } else if path.ends_with("/apps.connections.open") {
            Reply::json(&json!({"ok": true, "url": format!("wss://wss-primary.slack.com:{port}/link/?ticket=sentinel-ticket")}))
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
    let (fake, slack) = slack().await;
    slack.channels.lock().unwrap().extend_from_slice(channels);
    let home = Home::new(Some(SELF));
    let folders = [home.folder("a"), home.folder("b")];
    if let Some(text) = chat_toml {
        home.write(CONFIG_FILE, &text);
        home.secrets();
    }
    let rows = Workspaces::new();
    let ids = [rows.insert(row("alpha", &folders[0])).workspace_id.clone(), rows.insert(row("beta", &folders[1])).workspace_id.clone()];
    let chat = Chat::boot(&home.env, &rows, &|p| config::read_file(p), &Registry::new(), starter(&fake), HUB_CHECK, Timing::PRODUCT);
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

    /// The services this world has started: one `auth.test` for each.
    fn starts(&self) -> usize {
        self.calls("/auth.test")
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

/// The ops after the files a connect leaves (the secrets, owner-only, and a fresh `chat.toml`, written through the
/// config's own writers and loaded by a reload): list, pair, watch, and a post reaches the window.
#[tokio::test(flavor = "multi_thread")]
async fn from_the_connects_files_a_window_pairs_and_reads_its_channel() {
    let w = world(None, &[public("C0PUB", false)]).await;
    let (bot, app) = (Secret::new("xoxb-1111-2222-aaaa"), Secret::new("xapp-1-A111-2222-bbbb"));
    let dir = w.home.config();
    config::write_secrets(&dir, &config::secrets("T0AAAA", &bot, &app).unwrap(), Writers::platform()).unwrap();
    config::write_config(&dir, &fresh(), Writers::platform()).unwrap();
    let chat = w.services.chat.clone();
    assert_eq!(chat.reload(&chat.setup().await, &w.rows).await, Status::Running);

    let mut conn = connect_window(w.services.clone()).await;
    let listed = conn.ask(2, op::CHAT_CHANNELS, json!({})).await;
    assert_eq!(listed["channels"][0]["id"], "C0PUB", "{listed}");
    let paired = conn.pair(&w.ids[0], Some("C0PUB")).await;
    assert_eq!(paired, json!({}));
    after_hello(&chat, &w.rows, &w.target(0, "C0PUB")).await;
    let watched = conn.ask(3, op::CHAT_WATCH, json!({"target": w.target(0, "C0PUB")})).await;
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

    // The files: the secrets keep their three keys and, where there are owners, are the owner's alone; `chat.toml` loads.
    let secrets: toml::Table = std::fs::read_to_string(dir.join(SECRETS_FILE)).unwrap().parse().unwrap();
    assert_eq!(secrets.keys().map(String::as_str).collect::<Vec<_>>(), ["app_token", "bot_token", "workspace"]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(dir.join(SECRETS_FILE)).unwrap().permissions().mode() & 0o077, 0, "the secrets are the owner's alone");
    }
    let loaded = config::Config::load(&dir, &|p| config::read_file(p), &|f| std::fs::canonicalize(f).ok().map(|c| c.to_string_lossy().into_owned())).unwrap();
    assert_eq!(loaded.mappings().collect::<Vec<_>>(), [(w.folders[0].to_str().unwrap(), "C0PUB")]);
    for reply in [&listed, &paired, &watched, &put] {
        let text = reply.to_string();
        assert!(!text.contains("xoxb-") && !text.contains("xapp-"), "a reply or an event held a token");
    }
}
