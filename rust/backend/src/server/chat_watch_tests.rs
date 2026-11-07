//! The chat watches through real connections, against the exported fake Slack with a live socket.

use super::chat_ops_tests::{code, connect, rig, running, starter, watch, Conn, Rig, HOLD, OK};
use super::*;
use crate::chat::tests::{row, Home, SELF};
use crate::chat::watch::{Evt, Hub};
use crate::chat::{row_facts, Chat, Status};
use crate::lifecycle::retire::{Participant, Registry};
use serde_json::{json, Value};
use sot_chat::config::{self, CONFIG_FILE};
use sot_chat::fake_slack::{eventually, FakeSlack, Identity, Reply};
use sot_chat::service::Status as ServiceStatus;
use sot_protocol::ops::{ChatChannel, ChatTarget, ChatWatchState};
use std::future::Future;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// What the fake answers a history call with, besides `OK` and `HOLD`.
const FAIL: u8 = 3;
/// A bound no test relies on meeting: a wait that finishes is what a test asserts.
const LONG: Duration = Duration::from_secs(30);

async fn bounded<T>(what: &str, f: impl Future<Output = T>) -> T {
    tokio::time::timeout(LONG, f).await.unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

/// The fake's levers: its history answer, a gate the first history call waits on, the socket's feed and what it saw.
struct Levers {
    mode: Arc<AtomicU8>,
    gate: Arc<Mutex<Option<std::sync::mpsc::Receiver<()>>>>,
    feed: tokio::sync::mpsc::UnboundedSender<Value>,
    /// Envelopes the socket's client acknowledged.
    acked: Arc<AtomicUsize>,
    /// History calls that reached the fake, counted before the gate.
    histories: Arc<AtomicUsize>,
}

/// A fake Slack with a live socket: `auth.test` names the workspace, history answers an empty page, and the socket says
/// hello and then sends what the test feeds it.
async fn live() -> (FakeSlack, Levers) {
    let (feed, rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let rx = Arc::new(tokio::sync::Mutex::new(rx));
    let lv = Levers {
        mode: Arc::new(AtomicU8::new(OK)),
        gate: Arc::new(Mutex::new(None)),
        feed,
        acked: Arc::new(AtomicUsize::new(0)),
        histories: Arc::new(AtomicUsize::new(0)),
    };
    let (mode, gate, acked, histories) = (lv.mode.clone(), lv.gate.clone(), lv.acked.clone(), lv.histories.clone());
    let fake = FakeSlack::start(Identity::Trusted, move |seen, port| {
        let path = seen.path();
        if path.ends_with("/auth.test") {
            return Reply::json(&json!({"ok": true, "team_id": "T0AAAA", "user_id": "U0BOT", "bot_id": "B0BOT"}));
        }
        if path.ends_with("/apps.connections.open") {
            let url = format!("wss://wss-primary.slack.com:{port}/link/?ticket=sentinel-ticket");
            return Reply::json(&json!({"ok": true, "url": url}));
        }
        if path.ends_with("/conversations.history") {
            histories.fetch_add(1, Ordering::SeqCst);
            let gated = gate.lock().unwrap().take();
            if let Some(open) = gated {
                let _ = open.recv();
            }
            return match mode.load(Ordering::SeqCst) {
                HOLD => Reply::Hang,
                FAIL => Reply::json(&json!({"ok": false, "error": "internal_error"})),
                _ => Reply::json(&json!({"ok": true, "messages": [], "has_more": false})),
            };
        }
        if seen.upgrade {
            let (rx, acked) = (rx.clone(), acked.clone());
            return Reply::socket(move |mut peer| {
                let (rx, acked) = (rx.clone(), acked.clone());
                async move {
                    peer.send(&json!({"type": "hello", "num_connections": 1})).await;
                    let mut rx = rx.lock().await;
                    while let Some(envelope) = rx.recv().await {
                        peer.send(&envelope).await;
                        if peer.recv(Duration::from_secs(10)).await.is_some() {
                            acked.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }
            });
        }
        Reply::json(&json!({"ok": false, "error": "invalid_auth"}))
    })
    .await;
    (fake, lv)
}

fn stamp(n: u32) -> String {
    format!("17001{n:05}.000100")
}

/// A new message in `channel`, as Slack's socket delivers it.
fn message(channel: &str, n: u32) -> Value {
    json!({
        "type": "events_api",
        "envelope_id": format!("env-{n}"),
        "payload": {"team_id": "T0AAAA", "event": {"type": "message", "channel": channel, "user": "U0MEMBER", "ts": stamp(n), "text": "hello"}},
    })
}

fn alpha_target(rig: &Rig) -> ChatTarget {
    ChatTarget { row: rig.alpha.clone(), workspace: "T0AAAA".into(), channel: ChatChannel::try_from("C0AAAA".to_string()).unwrap() }
}

/// The rig, once the service's socket has said hello (so its first gap is latched before any watch begins).
async fn live_rig(fake: &FakeSlack) -> Rig {
    let rig = rig(fake).await;
    let service = rig.services.chat.admit(&rig.rows, &alpha_target(&rig)).expect("the service runs");
    eventually("the socket's hello", || matches!(service.status(), ServiceStatus::Running { socket, .. } if socket.connected)).await;
    rig
}

impl Conn {
    async fn frame(&mut self) -> Frame {
        bounded("a frame", codec::read_frame(&mut self.crx)).await.unwrap().0
    }

    /// The next evt frame of the connection: its op and payload.
    async fn evt(&mut self) -> (String, Value) {
        loop {
            let f = self.frame().await;
            if f.kind == Kind::Evt {
                return (f.op, f.payload);
            }
        }
    }

    /// The next `chat.changed`'s first put timestamp.
    async fn put_ts(&mut self) -> String {
        let (name, payload) = self.evt().await;
        assert_eq!(name, op::CHAT_CHANGED, "{payload}");
        payload["put"][0]["ts"].as_str().unwrap().to_string()
    }
}

/// Waits until the owner holds every channel's snapshot.
async fn complete(hub: &Arc<Hub>, channels: &[&str]) {
    let probe = hub.connect();
    eventually("the snapshots", || channels.iter().all(|c| probe.conn.state(c) == ChatWatchState::Complete)).await;
}

fn ok_watch(reply: &Value) -> bool {
    matches!(reply["state"].as_str(), Some("pending" | "complete"))
}

/// `n` rows with mapped folders (channels `C0000`...) and a spare row mapped to nothing.
async fn many(fake: &FakeSlack, n: usize) -> (Home, Services, Workspaces, Vec<(String, std::path::PathBuf)>, String, tempfile::TempDir) {
    let home = Home::new(Some(SELF));
    let folders: Vec<_> = (0..n).map(|i| home.folder(&format!("r{i}"))).collect();
    let channels: Vec<String> = (0..n).map(|i| format!("\"C{i:04}\"")).collect();
    let mut text = format!("workspace = \"T0AAAA\"\nchannels = [{}]\nmail_after = \"1.000000\"\n", channels.join(", "));
    for (i, f) in folders.iter().enumerate() {
        text += &format!("[[mapping]]\nfolder = '{}'\nchannel = \"C{i:04}\"\n", f.to_string_lossy());
    }
    home.write(CONFIG_FILE, &text);
    home.secrets();
    let rows = Workspaces::new();
    let ids = folders.iter().enumerate().map(|(i, f)| (rows.insert(row(&format!("r{i}"), f)).workspace_id.clone(), f.clone())).collect();
    let spare = rows.insert(row("spare", &home.folder("spare"))).workspace_id.clone();
    let chat = Chat::begin(&home.env, &row_facts(&rows), &|p| config::read_file(p), &Registry::new(), starter(fake));
    running(&chat).await;
    assert_eq!(chat.status(), Status::Running);
    let root = tempfile::tempdir().unwrap();
    let mut services = Services::for_test("t", root.path());
    services.chat = chat;
    services.workspaces = rows.clone();
    (home, services, rows, ids, spare, root)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_watch_is_admitted_and_bounded_per_connection() {
    let (fake, lv) = live().await;
    let n = sot_protocol::ops::MAX_WATCHED + 1;
    let (_home, services, rows, mut ids, spare, _root) = many(&fake, n).await;
    let hub = services.chat.hub().clone();
    let mut conn = connect(services.clone()).await;
    let mut id = 10;
    // The row's folder maps to C0000: another channel is a changed mapping.
    id += 1;
    assert_eq!(code(&conn.ask(id, op::CHAT_WATCH, watch(&ids[0].0, "C0001")).await), "stale_mapping", "remapped");
    // The anchor never acts.
    rows.set_default(&ids[0].0);
    id += 1;
    assert_eq!(code(&conn.ask(id, op::CHAT_WATCH, watch(&ids[0].0, "C0000")).await), "unmapped", "anchor");
    rows.set_default(&spare);
    // Removed, and replaced under a new id: the old id is gone.
    let (old, folder) = ids[n - 1].clone();
    rows.remove_by_id(&old).unwrap();
    ids[n - 1] = (rows.insert(row(&format!("r{}", n - 1), &folder)).workspace_id.clone(), folder);
    id += 1;
    assert_eq!(code(&conn.ask(id, op::CHAT_WATCH, watch(&old, &format!("C{:04}", n - 1))).await), "stale_row", "replaced");
    assert_eq!((lv.histories.load(Ordering::SeqCst), hub.slots().len()), (0, 0), "a refused watch reached Slack or held a slot");
    // The bound: a connection watches at most MAX_WATCHED channels, and a refused one holds nothing.
    for (i, (row_id, _)) in ids.iter().enumerate() {
        id += 1;
        let reply = conn.ask(id, op::CHAT_WATCH, watch(row_id, &format!("C{i:04}"))).await;
        if i < n - 1 {
            assert!(ok_watch(&reply), "watch {i}: {reply}");
        } else {
            assert_eq!(code(&reply), "too_many_watches", "{reply}");
        }
    }
    assert_eq!(hub.slots().len(), n - 1);
    // The daemon's model is bounded in all: a second connection may join a channel held, not add one.
    let mut other = connect(services.clone()).await;
    assert_eq!(code(&other.ask(2, op::CHAT_WATCH, watch(&ids[n - 1].0, &format!("C{:04}", n - 1))).await), "too_many_watches");
    assert!(ok_watch(&other.ask(3, op::CHAT_WATCH, watch(&ids[0].0, "C0000")).await));
    eventually("a snapshot for each channel", || lv.histories.load(Ordering::SeqCst) == n - 1).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn chat_events_reach_only_the_connections_that_watch() {
    let (fake, lv) = live().await;
    let rig = live_rig(&fake).await;
    let hub = rig.services.chat.hub().clone();
    let (mut a, mut b) = (connect(rig.services.clone()).await, connect(rig.services.clone()).await);
    assert!(ok_watch(&a.ask(2, op::CHAT_WATCH, watch(&rig.alpha, "C0AAAA")).await));
    assert!(ok_watch(&b.ask(2, op::CHAT_WATCH, watch(&rig.beta, "C0BBBB")).await));
    complete(&hub, &["C0AAAA", "C0BBBB"]).await;
    // Each event is read by the connection it is for before the next is sent, so a connection's frames are in order
    // with what it was sent: an event for the other channel, if it were written, would stand between its own two.
    lv.feed.send(message("C0AAAA", 1)).unwrap();
    let first = a.put_ts().await;
    lv.feed.send(message("C0BBBB", 2)).unwrap();
    let second = b.put_ts().await;
    lv.feed.send(message("C0AAAA", 3)).unwrap();
    let third = a.put_ts().await;
    lv.feed.send(message("C0BBBB", 4)).unwrap();
    let fourth = b.put_ts().await;
    assert_eq!((first, second, third, fourth), (stamp(1), stamp(2), stamp(3), stamp(4)));
    // A connection that never watched is written nothing.
    let never = hub.connect();
    let mut written = Vec::new();
    let channel = ChatChannel::try_from("C0AAAA".to_string()).unwrap();
    let evt = sot_protocol::ops::ChatChangedEvt { workspace: "T0AAAA".into(), channel, put: Vec::new(), deleted: Vec::new() };
    let wrote = super::super::events::write_chat_event(&mut written, Ok(Evt::Changed(evt)), &never.conn).await.unwrap();
    assert!(!wrote && written.is_empty(), "a connection that never watched was written a chat event");
}

#[tokio::test(flavor = "multi_thread")]
async fn unwatch_ends_only_this_connections_watch() {
    let (fake, lv) = live().await;
    let rig = live_rig(&fake).await;
    let hub = rig.services.chat.hub().clone();
    let (mut a, mut b) = (connect(rig.services.clone()).await, connect(rig.services.clone()).await);
    assert!(ok_watch(&a.ask(2, op::CHAT_WATCH, watch(&rig.alpha, "C0AAAA")).await));
    assert!(ok_watch(&a.ask(3, op::CHAT_WATCH, watch(&rig.beta, "C0BBBB")).await));
    assert!(ok_watch(&b.ask(2, op::CHAT_WATCH, watch(&rig.alpha, "C0AAAA")).await));
    complete(&hub, &["C0AAAA", "C0BBBB"]).await;
    assert_eq!(a.ask(4, op::CHAT_UNWATCH, watch(&rig.alpha, "C0AAAA")).await, json!({}));
    // The channel is still watched by the other connection, so the owner keeps it and tells that one alone.
    lv.feed.send(message("C0AAAA", 1)).unwrap();
    let reached = bounded("the other connection's chat.changed: its watch ended with this one's unwatch", b.put_ts()).await;
    assert_eq!(reached, stamp(1));
    // This connection was sent nothing for it: its next event is for the channel it still watches.
    lv.feed.send(message("C0BBBB", 2)).unwrap();
    assert_eq!(a.put_ts().await, stamp(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_connection_releases_its_watches() {
    let (fake, _lv) = live().await;
    let rig = live_rig(&fake).await;
    let hub = rig.services.chat.hub().clone();
    let (mut closed, mut dropped, mut kept) = (connect(rig.services.clone()).await, connect(rig.services.clone()).await, connect(rig.services.clone()).await);
    for conn in [&mut closed, &mut dropped, &mut kept] {
        assert!(ok_watch(&conn.ask(2, op::CHAT_WATCH, watch(&rig.alpha, "C0AAAA")).await));
    }
    assert_eq!(hub.slots().len(), 3);
    // A clean close, with no unwatch.
    let Conn { crx, ctx, task } = closed;
    drop((crx, ctx));
    task.await.unwrap().unwrap();
    assert_eq!(hub.slots().len(), 2, "a closed connection kept its slot");
    // A connection task dropped where it stands (any path that does not return through the loop's end).
    dropped.task.abort();
    let _ = (&mut dropped.task).await;
    assert_eq!(hub.slots().len(), 1, "a connection dropped mid-await kept its slot");
    drop(kept);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_watch_reply_precedes_its_first_event() {
    let (fake, lv) = live().await;
    let rig = live_rig(&fake).await;
    let hub = rig.services.chat.hub().clone();
    // The first snapshot is held at the fake until the hook opens it.
    let (open, gate) = std::sync::mpsc::channel();
    *lv.gate.lock().unwrap() = Some(gate);
    let open = Mutex::new(open);
    let open = Arc::new(open);
    let (feed, acked, histories, hub_for_hook) = (lv.feed.clone(), lv.acked.clone(), lv.histories.clone(), hub.clone());
    // Between the registration and the reply: wait for the snapshot to be begun, deliver a message while it is pending,
    // open the snapshot and wait until the event is published to the connections' receivers. The reply is not yet
    // written, so an event that overtakes it is one the loop wrote first.
    *hub.between.lock().unwrap() = Some(Arc::new(move || {
        let (feed, acked, histories, hub, open) = (feed.clone(), acked.clone(), histories.clone(), hub_for_hook.clone(), open.clone());
        Box::pin(async move {
            let mut events = hub.subscribe();
            eventually("the snapshot's call", || histories.load(Ordering::SeqCst) == 1).await;
            feed.send(message("C0AAAA", 1)).unwrap();
            eventually("the message's acknowledgement", || acked.load(Ordering::SeqCst) == 1).await;
            open.lock().unwrap().send(()).unwrap();
            while !matches!(bounded("the event", events.recv()).await, Ok(Evt::Changed(_))) {}
        })
    }));
    let mut conn = connect(rig.services.clone()).await;
    conn.send(2, op::CHAT_WATCH, watch(&rig.alpha, "C0AAAA")).await;
    let first = conn.frame().await;
    assert_eq!((first.kind, first.id), (Kind::Res, 2), "an event was written before the watch's reply: {first:?}");
    let (name, payload) = conn.evt().await;
    assert_eq!((name.as_str(), payload["put"][0]["ts"].as_str()), (op::CHAT_CHANGED, Some(stamp(1).as_str())), "the message made while the snapshot was pending was lost");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_watch_whose_row_went_away_is_refused_at_its_next_snapshot() {
    let (fake, lv) = live().await;
    let rig = live_rig(&fake).await;
    let hub = rig.services.chat.hub().clone();
    let mut conn = connect(rig.services.clone()).await;
    let gamma = rig.rows.insert(row("gamma", &rig.folders[2])).workspace_id.clone();
    let mut id = 1;
    for what in ["remapped", "anchor", "removed"] {
        id += 1;
        assert!(ok_watch(&conn.ask(id, op::CHAT_WATCH, watch(&rig.alpha, "C0AAAA")).await), "{what}: the watch began");
        complete(&hub, &["C0AAAA"]).await;
        let before = lv.histories.load(Ordering::SeqCst);
        match what {
            // The same id re-rooted onto folder b maps to C0BBBB, not what the watch expects.
            "remapped" => drop(rig.rows.insert(row("alpha", &rig.folders[1]))),
            "anchor" => rig.rows.set_default(&rig.alpha),
            _ => drop(rig.rows.remove_by_id(&rig.alpha).unwrap()),
        }
        // The window's resync (a repeat watch, as a chat.status asks) is admitted again.
        id += 1;
        let want = match what {
            "remapped" => "stale_mapping",
            "anchor" => "unmapped",
            _ => "stale_row",
        };
        assert_eq!(code(&conn.ask(id, op::CHAT_WATCH, watch(&rig.alpha, "C0AAAA")).await), want, "{what}");
        assert_eq!(lv.histories.load(Ordering::SeqCst), before, "{what}: the refused resync reached Slack");
        assert!(hub.slots().is_empty(), "{what}: the refused watch kept its slot");
        // Restore the row for the next case.
        match what {
            "remapped" => drop(rig.rows.insert(row("alpha", &rig.folders[0]))),
            "anchor" => rig.rows.set_default(&gamma),
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_snapshot_is_admitted_against_the_oldest_watch_whose_row_is_current() {
    let (fake, lv) = live().await;
    let rig = live_rig(&fake).await;
    let chat = rig.services.chat.clone();
    let target = |row: &str| ChatTarget { row: row.into(), workspace: "T0AAAA".into(), channel: ChatChannel::try_from("C0AAAA".to_string()).unwrap() };
    let stale = Mutex::new(std::collections::BTreeSet::new());
    // The oldest watch's row is gone: it is named stale, and the next watch's row is the one the snapshot runs under.
    let both = [(1, target("ws-gone-0")), (2, target(&rig.alpha))];
    assert!(crate::chat::watch::snapshot(&chat, &rig.rows, &both, "C0AAAA", &stale).await.is_ok());
    assert_eq!((stale.lock().unwrap().iter().copied().collect::<Vec<_>>(), lv.histories.load(Ordering::SeqCst)), (vec![1], 1));
    // Every watch's row gone: refused, named, and nothing is sent.
    let gone = [(1, target("ws-gone-0")), (2, target("ws-gone-1"))];
    assert_eq!(crate::chat::watch::snapshot(&chat, &rig.rows, &gone, "C0AAAA", &stale).await.err(), Some(sot_protocol::ops::ChatCode::StaleRow));
    assert_eq!((stale.lock().unwrap().len(), lv.histories.load(Ordering::SeqCst)), (2, 1), "a snapshot for a stale row reached Slack");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_service_stop_reaches_watchers_as_status() {
    let (fake, _lv) = live().await;
    let rig = live_rig(&fake).await;
    let hub = rig.services.chat.hub().clone();
    let mut conn = connect(rig.services.clone()).await;
    assert!(ok_watch(&conn.ask(2, op::CHAT_WATCH, watch(&rig.alpha, "C0AAAA")).await));
    let mut other = connect(rig.services.clone()).await;
    complete(&hub, &["C0AAAA"]).await;
    Participant::request_stop(&*rig.services.chat);
    assert!(Participant::wait_retired(&*rig.services.chat, LONG));
    let (name, payload) = bounded("a chat.status for the stop", conn.evt()).await;
    assert_eq!(name, op::CHAT_STATUS, "{payload}");
    assert!(matches!(payload["service"].as_str(), Some("stopping" | "stopped")), "{payload}");
    assert_eq!(payload["resync"], json!(["C0AAAA"]), "every watch is told to resync");
    // Their watches ended with the service, and none begins after it.
    eventually("the owner's end", || hub.ended()).await;
    assert!(hub.slots().is_empty());
    assert_eq!(code(&other.ask(5, op::CHAT_WATCH, watch(&rig.alpha, "C0AAAA")).await), "unavailable");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lagging_connection_is_told_it_lost_chat_events() {
    let hub = Hub::new();
    let watcher = hub.connect();
    let mut written = Vec::new();
    let lagged = Err(tokio::sync::broadcast::error::RecvError::Lagged(3));
    assert!(super::super::events::write_chat_event(&mut written, lagged, &watcher.conn).await.unwrap());
    let (frame, _) = codec::read_frame(&mut &written[..]).await.unwrap();
    assert_eq!((frame.kind, frame.op.as_str()), (Kind::Evt, op::BUS_LOST));
    assert_eq!((frame.payload["bus"].as_str(), frame.payload["skipped"].as_u64()), (Some(op::CHAT_CHANGED), Some(3)));
}

#[tokio::test(flavor = "multi_thread")]
async fn held_chat_watch_does_not_delay_ping() {
    let (fake, lv) = live().await;
    lv.mode.store(HOLD, Ordering::SeqCst);
    let rig = live_rig(&fake).await;
    let mut conn = connect(rig.services.clone()).await;
    conn.send(10, op::CHAT_WATCH, watch(&rig.alpha, "C0AAAA")).await;
    conn.send(11, op::PING, json!({})).await;
    conn.send(12, op::VERSION_QUERY, json!({})).await;
    // A snapshot run on the loop would hold these replies until the API gives up on the held call (`READ_DEADLINE`), and
    // the held call is observable only as that latency: the one time bound in these tests, a third of the deadline.
    let mut seen = Vec::new();
    let collect = async {
        while seen.len() < 3 {
            let f = conn.frame().await;
            if f.kind == Kind::Res {
                seen.push(f.id);
            }
        }
    };
    tokio::time::timeout(sot_chat::api::READ_DEADLINE / 3, collect).await.expect("a reply was held behind the snapshot");
    seen.sort_unstable();
    assert_eq!(seen, [10, 11, 12], "a reply was missing while the snapshot was held");
    // The snapshot is held at the fake still, and the connection answers.
    eventually("the held snapshot's call", || lv.histories.load(Ordering::SeqCst) == 1).await;
    assert_eq!(conn.ask(13, op::PING, json!({})).await["ok"], true);
    fake.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_snapshot_keeps_the_watch_and_asks_for_a_resync() {
    let (fake, lv) = live().await;
    lv.mode.store(FAIL, Ordering::SeqCst);
    let rig = live_rig(&fake).await;
    let hub = rig.services.chat.hub().clone();
    let mut conn = connect(rig.services.clone()).await;
    assert!(ok_watch(&conn.ask(2, op::CHAT_WATCH, watch(&rig.alpha, "C0AAAA")).await));
    let (name, payload) = conn.evt().await;
    assert_eq!((name.as_str(), payload["resync"].clone()), (op::CHAT_STATUS, json!(["C0AAAA"])), "{payload}");
    assert_eq!(hub.slots().len(), 1, "a failed snapshot released the watch");
    // The window answers the status with a watch; the snapshot is tried again and, now served, completes.
    lv.mode.store(OK, Ordering::SeqCst);
    assert!(ok_watch(&conn.ask(3, op::CHAT_WATCH, watch(&rig.alpha, "C0AAAA")).await));
    complete(&hub, &["C0AAAA"]).await;
    assert_eq!(lv.histories.load(Ordering::SeqCst), 2, "one snapshot for each watch");
}
