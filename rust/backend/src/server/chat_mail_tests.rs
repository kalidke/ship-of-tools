//! The chat's delivery through the daemon's own composition, against the exported fake Slack with a live socket: a post
//! becomes marked mail in the mapped row's inbox, and the inboxes are the record of what was filed.

use super::chat_ops_tests::{running, starter};
use super::chat_watch_tests::{live, message, stamp, until, Levers};
use super::Services;
use crate::chat::tests::{Home, SELF};
use crate::chat::{Chat, Status, HUB_CHECK};
use crate::comm::registry::registry::{iso8601_utc_from_secs, unix_now_secs};
use crate::comm::test_home::TestHome;
use crate::lifecycle::retire::{retire, Participant, Registry};
use crate::rows::{Workspace, Workspaces};
use serde_json::{json, Value};
use sot_chat::config;
use sot_chat::delivery::Timing;
use sot_chat::fake_slack::{FakeSlack, History, Upload};
use sot_chat::service::{Service, Status as ServiceStatus};
use sot_platform::test_hold::Holds;
use sot_protocol::chat::Ts;
use sot_protocol::ops::{op, ChatChannel, ChatTarget};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

/// Short enough that a failed start read is tried again within a test.
const TIMING: Timing = Timing { read_pace: Duration::from_millis(10), sweep: Duration::from_secs(3600), backoff_first: Duration::from_millis(50), backoff_max: Duration::from_millis(200) };
const LONG: Duration = Duration::from_secs(30);

/// A hub with row alpha (handle `alpha-h`, a live session) mapped to `C0AAAA`, a comm folder this daemon appends to
/// itself, and a fake Slack with a live socket. The chat's environment names the test's comm folder, never the process's.
/// The test holds the registry's turn alone: a filing takes the registry lock within what is left of the inbox wait
/// (2 s), a bound another test's hold of the process-wide turn would spend (`registry::lock::TURN_TESTS`).
struct World {
    th: TestHome,
    /// The mapped folder, as the chat names it.
    folder: String,
    home: Home,
    rows: Workspaces,
    fake: FakeSlack,
    lv: Levers,
}

async fn world(delivers: bool) -> World {
    world_with(delivers.then_some("1.000000")).await
}

/// [`world`] delivering from the floor `floor`.
async fn world_at(floor: &str) -> World {
    world_with(Some(floor)).await
}

async fn world_with(floor: Option<&str>) -> World {
    let th = TestHome::exclusive();
    th.own_record();
    let mut home = Home::new(Some(SELF));
    home.env = std::mem::take(&mut home.env).with("SOT_COMM_HOME", &th.home).with("SOT_SOCKET", th.home.join("no-daemon.sock"));
    let folder = home.folder("a");
    let own = [folder.to_string_lossy().into_owned()];
    match floor {
        Some(floor) => home.delivering_toml_after(&own, floor),
        None => home.chat_toml(&own),
    }
    home.secrets();
    let rows = Workspaces::new();
    rows.insert(Workspace::from_label("claude-alpha", folder.clone(), false, "claude".into(), "alpha-h".into(), String::new()));
    let (fake, lv) = live().await;
    let w = World { th, folder: folder.to_string_lossy().into_owned(), home, rows, fake, lv };
    w.session(0);
    w
}

impl World {
    /// The registry's entry for `alpha-h`: this computer's, bound to alpha, last seen `age` seconds ago.
    fn session(&self, age: u64) {
        let id = self.rows.list()[0].workspace_id.clone();
        self.th.put("alpha-h", json!({"host": SELF, "workspace_id": id, "repo": "r", "root": "/", "last_seen": iso8601_utc_from_secs(unix_now_secs() - age)}));
    }

    /// The chat as the daemon boots it against `fake`, once its socket has said hello.
    async fn start(&self, fake: &FakeSlack) -> Arc<Chat> {
        self.start_in(fake, &Registry::new(), HUB_CHECK).await
    }

    /// [`World::start`] registered with `registry`, the hub fact asked every `check`.
    async fn start_in(&self, fake: &FakeSlack, registry: &Registry, check: Duration) -> Arc<Chat> {
        let read = |p: &std::path::Path| config::read_file(p);
        let chat = Chat::boot(&self.home.env, &self.rows, &read, registry, starter(fake), crate::chat::setup::Verifier::at(fake), check, TIMING);
        running(&chat).await;
        let alpha = self.rows.list()[0].workspace_id.clone();
        let target = ChatTarget { row: alpha, workspace: "T0AAAA".into(), channel: ChatChannel::try_from("C0AAAA".to_string()).unwrap() };
        super::chat_watch_tests::after_hello(&chat, &self.rows, &target).await;
        chat
    }

    /// The chat's running service, to watch it stop.
    fn service(&self, chat: &Chat) -> Arc<Service> {
        let target = ChatTarget { row: self.rows.list()[0].workspace_id.clone(), workspace: "T0AAAA".into(), channel: ChatChannel::try_from("C0AAAA".to_string()).unwrap() };
        chat.admit(&self.rows, &target).expect("the service runs")
    }

    /// The inbox of `alpha-h`: each line's message.
    fn inbox(&self) -> Vec<String> {
        let text = std::fs::read_to_string(self.th.home.join("inbox").join("alpha-h.jsonl")).unwrap_or_default();
        text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter_map(|v| v["msg"].as_str().map(str::to_string)).collect()
    }

    /// The `ts` each filing names, in the order filed.
    fn filed(&self) -> Vec<String> {
        self.inbox().iter().filter_map(|m| m.lines().nth(1)?.strip_prefix("ts=")?.split(' ').next().map(str::to_string)).collect()
    }

    /// The chat's state folder, on this daemon's disk.
    fn state_dir(&self) -> std::path::PathBuf {
        sot_platform::host::state_dir::daemon_state_dir_in(&self.home.env).join("chat")
    }

    /// The boundary the channel's state file holds, once a pass has committed one.
    fn boundary(&self) -> Option<String> {
        std::fs::read_to_string(self.state_dir().join("delivery-T0AAAA-C0AAAA")).ok().map(|t| t.trim().to_string())
    }

    /// The inbox of `handle`, as it is on disk now: how many filings it holds.
    fn count_in(&self, handle: &str) -> usize {
        std::fs::read_to_string(self.th.home.join("inbox").join(format!("{handle}.jsonl"))).map_or(0, |t| t.lines().count())
    }

    async fn filed_count(&self, n: usize) {
        until("the filings", || self.inbox().len() >= n).await;
    }
}

/// An event as Slack sends it, under an envelope id of its own so that sending a post again is a new envelope.
fn send(lv: &Levers, n: u32, envelope: &str) {
    let mut event = message("C0AAAA", n);
    event["envelope_id"] = json!(envelope);
    lv.feed.send(event).unwrap();
}

/// Whether `check` holds within 5 s: a test that asserts a thing happens says so by this, not by a wait that runs out.
async fn soon(mut check: impl FnMut() -> bool) -> bool {
    for _ in 0..500 {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

/// A post as Slack sends it at `ts`, under an envelope id of its own.
fn send_at(lv: &Levers, ts: &str, envelope: &str) {
    let mut event = message("C0AAAA", 1);
    event["envelope_id"] = json!(envelope);
    event["payload"]["event"]["ts"] = json!(ts);
    lv.feed.send(event).unwrap();
}

/// The channel the fake's history plays: the posts `ns` of `stamp`.
fn posts(ns: &[u32]) -> History {
    ns.iter().fold(History::new(), |h, n| h.post(&stamp(*n)))
}

fn serve(lv: &Levers, channel: History) -> Arc<History> {
    let channel = Arc::new(channel);
    *lv.channel.lock().unwrap() = Some(channel.clone());
    channel
}

/// Retire the chat, wait until its service has stopped (its lock freed), and let it go.
fn stop(chat: Arc<Chat>) {
    Participant::request_stop(&*chat);
    assert!(Participant::wait_retired(&*chat, LONG));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_owner_runs_after_a_reload_with_no_watch() {
    let w = world(false).await;
    let chat = w.start(&w.fake).await;
    // The file now delivers: the reload's owner opens the delivery with no watch and no window.
    w.home.delivering_toml(&[w.folder.clone()]);
    chat.reload_for(&chat.setup().await, &w.rows, "T0AAAA").await;
    assert_eq!(chat.status(), Status::Running);
    let target = ChatTarget { row: w.rows.list()[0].workspace_id.clone(), workspace: "T0AAAA".into(), channel: ChatChannel::try_from("C0AAAA".to_string()).unwrap() };
    super::chat_watch_tests::after_hello(&chat, &w.rows, &target).await;
    until("the first socket's end", || w.fake.sockets().0 == 1).await;
    send(&w.lv, 1, "e1");
    w.filed_count(1).await;
    assert_eq!(w.filed(), vec![stamp(1)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_live_member_post_is_marked_mail_for_the_row_and_nothing_else_is() {
    let w = world(true).await;
    let _chat = w.start(&w.fake).await;
    // A bot's post is not mail; the member's posts around it are.
    send(&w.lv, 1, "e1");
    let mut bot = message("C0AAAA", 2);
    bot["payload"]["event"]["bot_id"] = json!("B0OTHER");
    w.lv.feed.send(bot).unwrap();
    send(&w.lv, 3, "e3");
    w.filed_count(2).await;
    assert_eq!(w.filed(), vec![stamp(1), stamp(3)]);
    let first = &w.inbox()[0];
    assert!(first.starts_with("[chat] From a channel member"), "{first}");
    assert!(first.contains("\"Name-U0MEMBER\""), "the author is named as a read names one: {first}");
    assert!(first.contains(&format!("ts={} thread={} channel=C0AAAA workspace=T0AAAA", stamp(1), stamp(1))), "{first}");
}

/// A file without `mail_after` opens no delivery: the trap (an inbox that cannot be read, which makes any read of the
/// inboxes log its name) is read by a delivering file and by none other, and nothing is filed.
#[tokio::test(flavor = "current_thread")]
async fn a_file_without_mail_after_reads_no_inbox_and_files_nothing() {
    let log = sot_platform::test_log::capture();
    for delivers in [false, true] {
        let w = world(delivers).await;
        std::fs::create_dir(w.th.home.join("inbox").join("trapnoread.jsonl")).unwrap();
        let _chat = w.start(&w.fake).await;
        send(&w.lv, 1, "e1");
        send(&w.lv, 2, "e2");
        until("both events taken", || w.lv.acked.load(Ordering::SeqCst) >= 2).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(w.inbox(), Vec::<String>::new());
        assert_eq!(log.text().contains("trapnoread"), delivers, "{}", log.text());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_filing_job_that_panics_still_answers_and_the_next_post_is_filed() {
    let w = world(true).await;
    let _chat = w.start(&w.fake).await;
    // A ts no other test sends: the seam is process-wide.
    crate::chat::watch::PANIC_ON.lock().unwrap().push(stamp(777));
    send(&w.lv, 777, "e1");
    send(&w.lv, 778, "e2");
    w.filed_count(1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(w.filed(), vec![stamp(778)], "the post whose job panicked is not filed, and the next is");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_after_live_filings_files_nothing_again() {
    let w = world(true).await;
    let chat = w.start(&w.fake).await;
    send(&w.lv, 1, "e1");
    w.filed_count(1).await;
    stop(chat);
    // The same post is sent again to a chat that starts on the same state and comm folders (against a new fake: the
    // first one's socket still holds the feed), then a new one: the new one is filed after the first in the queue's
    // order, so a repeat would be in the inbox by then.
    let (fake, lv) = live().await;
    let _chat = w.start(&fake).await;
    send(&lv, 1, "again-1");
    send(&lv, 2, "e2");
    w.filed_count(2).await;
    assert_eq!(w.filed(), vec![stamp(1), stamp(2)], "a post filed before the restart was filed again");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreadable_inbox_at_start_files_nothing_until_it_reads() {
    let w = world(true).await;
    // The other computer's filing of the first post is in an inbox that cannot be read now (a directory in its place).
    // The first post is in the channel's history only: a pass is the one thing that could file it again.
    serve(&w.lv, posts(&[1, 2]));
    let inbox = w.th.home.join("inbox").join("b.jsonl");
    let before = sot_chat::mail::body(&post_record(1));
    std::fs::write(&inbox, format!("{}\n", json!({"from": "#chat", "to": "b", "repo": "daemon", "msg": before, "ts": "2026-01-01T00:00:00Z"}))).unwrap();
    let kept = std::fs::read(&inbox).unwrap();
    std::fs::remove_file(&inbox).unwrap();
    std::fs::create_dir(&inbox).unwrap();
    let _chat = w.start(&w.fake).await;
    send(&w.lv, 2, "e2");
    until("the event taken", || w.lv.acked.load(Ordering::SeqCst) >= 1).await;
    // Several tries on the backoff go by, and nothing is filed and no pass is made.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!((w.inbox(), w.boundary()), (Vec::<String>::new(), None), "a post was filed or passed while an inbox could not be read");
    // Once it reads, the first pass files nothing the inbox holds, and files the other post once.
    std::fs::remove_dir(&inbox).unwrap();
    std::fs::write(&inbox, kept).unwrap();
    until("the first pass's commit", || w.boundary().is_some()).await;
    send(&w.lv, 3, "e3");
    until("the new post", || w.filed().contains(&stamp(3))).await;
    assert_eq!(w.filed(), vec![stamp(2), stamp(3)]);
}

/// A first run on `w` against `fake`: the channel holds posts 1 and 2, the socket sends 1, and the first pass files what
/// the socket did not; returns once the first pass has committed.
async fn first_run(w: &World) {
    let (fake, lv) = live().await;
    serve(&lv, posts(&[1, 2]));
    let chat = w.start(&fake).await;
    send(&lv, 1, "e1");
    w.filed_count(2).await;
    until("the first pass's commit", || w.boundary().is_some()).await;
    stop(chat);
}

/// A new chat on the same folders, against a fake that holds the same channel and then a new post; returns once its first
/// pass has committed a boundary other than `not` and the new post is filed.
async fn second_run(w: &World, not: Option<String>) -> Arc<Chat> {
    let (fake, lv) = live().await;
    serve(&lv, posts(&[1, 2]));
    let chat = w.start(&fake).await;
    until("the second run's first pass's commit", || w.boundary().is_some() && w.boundary() != not).await;
    // The pass committed below the posts, before any filing of its own would have returned. A new post, filed last, says
    // the pass's candidates (queued before it) have been filed, if they were going to be.
    send(&lv, 3, "e3");
    until("the new post", || w.filed().contains(&stamp(3))).await;
    chat
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_state_folder_after_live_filings_files_nothing_twice() {
    let w = world(true).await;
    first_run(&w).await;
    // The row's handle changed since, so its filings are in the inbox of one no row holds now; the state folder is new.
    let inbox = w.th.home.join("inbox");
    std::fs::rename(inbox.join("alpha-h.jsonl"), inbox.join("gone-h.jsonl")).unwrap();
    std::fs::remove_dir_all(w.state_dir()).unwrap();
    let _chat = second_run(&w, None).await;
    assert_eq!((w.count_in("alpha-h"), w.count_in("gone-h")), (1, 2), "the first pass of a new state folder filed a post again");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_state_file_older_than_the_inboxes_files_nothing_twice() {
    let w = world(true).await;
    first_run(&w).await;
    // The state folder restored from a copy that is older than the filings: its boundary lies below both posts.
    let old = Ts::parse("1700000000.000000").unwrap();
    sot_chat::state::write(&w.state_dir(), "T0AAAA", "C0AAAA", &old).unwrap();
    let _chat = second_run(&w, Some(old.as_str().to_string())).await;
    assert_eq!(w.count_in("alpha-h"), 3, "the first pass of an old state file filed a post again");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pass_with_no_recipient_reads_nothing_and_a_refused_post_is_found_once_the_row_returns() {
    let w = world(true).await;
    let channel = serve(&w.lv, posts(&[1, 2]));
    // The row's session has lapsed: the live post is refused, and the pass it owes is skipped before any read.
    w.session(10_000);
    let _chat = w.start(&w.fake).await;
    send(&w.lv, 1, "e1");
    until("the event taken", || w.lv.acked.load(Ordering::SeqCst) >= 1).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!((w.inbox(), w.boundary(), channel.asked().len()), (Vec::<String>::new(), None, 0), "a pass read the channel with no row to give its mail to");
    // The row returns: the next pass finds the refused post and the other, once each.
    w.session(0);
    assert!(soon(|| w.boundary().is_some()).await, "no pass completed once the row returned");
    send(&w.lv, 3, "e3");
    until("the new post", || w.filed().contains(&stamp(3))).await;
    assert_eq!(w.filed(), vec![stamp(1), stamp(2), stamp(3)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pass_job_that_panics_answers_skipped_and_the_next_pass_runs() {
    let w = world(true).await;
    serve(&w.lv, posts(&[1, 2]));
    crate::chat::watch::PANIC_PASS_IN.lock().unwrap().push(w.folder.clone());
    let _chat = w.start(&w.fake).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(w.boundary(), None);
    crate::chat::watch::PANIC_PASS_IN.lock().unwrap().retain(|f| *f != w.folder);
    assert!(soon(|| w.boundary().is_some()).await, "a pass that panicked left the delivery waiting on it");
}

/// A filing in flight, held in its job; the first post is sent and the test returns once the job is there.
async fn hold_a_filing(w: &World, chat: &Chat) -> (sot_platform::test_hold::Arm, Arc<Service>) {
    let arm = Holds::here().arm("chat.mail.filing").key(&w.folder).hold(1);
    let service = w.service(chat);
    send(&w.lv, 1, "e1");
    arm.reached_async(0).await;
    (arm, service)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_step_down_returns_the_filing_in_flight_before_the_service_stops() {
    let w = world(true).await;
    let chat = w.start_in(&w.fake, &Registry::new(), Duration::from_millis(50)).await;
    let (arm, service) = hold_a_filing(&w, &chat).await;
    // The topology names another computer the hub: the chat steps down at its next look, while the filing is held.
    w.home.list("elsewhere");
    assert!(soon(|| chat.status() == Status::Stopping).await, "the chat did not step down");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(matches!(service.status(), ServiceStatus::Running { .. }), "the service stopped under a filing in flight: {:?}", service.status());
    arm.release(0);
    assert!(soon(|| chat.status() == Status::NotHub).await, "the chat did not finish stepping down");
    assert!(matches!(service.status(), ServiceStatus::Stopped));
}

/// A service that ended on its own returns the filing in flight before the chat is off, so a reload, which waits for it,
/// starts a service whose start read sees the filing: the post is in the inbox once.
#[tokio::test(flavor = "multi_thread")]
async fn a_filing_in_flight_at_an_end_returns_before_a_reload_starts_the_next_service() {
    let w = world(true).await;
    *w.lv.bot_app.lock().unwrap() = Some("A0OWN".into());
    let chat = w.start(&w.fake).await;
    let (arm, service) = hold_a_filing(&w, &chat).await;
    serve(&w.lv, posts(&[1]));
    // An envelope of another app ends the service while the filing is held.
    let mut other = message("C0AAAA", 2);
    other["payload"]["api_app_id"] = json!("A0OTHER");
    other["envelope_id"] = json!("e2");
    w.lv.feed.send(other).unwrap();
    assert!(soon(|| service.ended() == Some("wrong_app")).await, "the service did not end");
    let reloading = tokio::spawn({
        let (chat, rows) = (chat.clone(), w.rows.clone());
        async move {
            chat.reload_for(&chat.setup().await, &rows, "T0AAAA").await;
            chat.status()
        }
    });
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(!reloading.is_finished(), "the reload went on while the filing was in flight");
    assert!(w.filed().is_empty());
    arm.release(0);
    assert_eq!(tokio::time::timeout(LONG, reloading).await.unwrap().unwrap(), Status::Running);
    assert!(soon(|| w.filed() == vec![stamp(1)]).await, "the filing in flight was not appended");
    // The new service's passes have had time to run over a history that holds the post.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(w.filed(), vec![stamp(1)], "the post was filed twice");
}

/// L9 rule 2 in the moment after a service ended on its own and before its owner turned the unit off (the owner held at
/// `chat.owner.end`, the unit still `Running` the ended service): a reload stops that unit as one that is not running,
/// waits for its filing in flight, and starts the workspace again; it neither keeps the ended service nor leaves the
/// workspace off.
#[tokio::test(flavor = "multi_thread")]
async fn a_reload_before_the_owner_sees_its_services_end_waits_for_the_filing_and_starts_it_again() {
    let w = world(true).await;
    *w.lv.bot_app.lock().unwrap() = Some("A0OWN".into());
    let chat = w.start(&w.fake).await;
    let (arm, service) = hold_a_filing(&w, &chat).await;
    serve(&w.lv, posts(&[1]));
    let owner = Holds::here().arm("chat.owner.end").key(Arc::as_ptr(chat.hub()) as usize).hold(1);
    // An envelope of another app ends the service while the filing is held.
    let mut other = message("C0AAAA", 2);
    other["payload"]["api_app_id"] = json!("A0OTHER");
    other["envelope_id"] = json!("e2");
    w.lv.feed.send(other).unwrap();
    assert!(soon(|| service.ended() == Some("wrong_app")).await, "the service did not end");
    owner.reached_async(0).await;
    assert_eq!(chat.status(), Status::Running, "the unit no longer holds the ended service");
    let reloading = tokio::spawn({
        let (chat, rows) = (chat.clone(), w.rows.clone());
        async move {
            chat.reload_for(&chat.setup().await, &rows, "T0AAAA").await;
            chat.status()
        }
    });
    // The reload either answers at once, having kept the unit, or begins its stop, which the held filing holds.
    until("the reload's decision", || reloading.is_finished() || chat.status() == Status::Stopping).await;
    assert!(!reloading.is_finished(), "the reload went on while the filing was in flight: it kept the unit of an ended service");
    owner.release(0);
    arm.release(0);
    assert_eq!(tokio::time::timeout(LONG, reloading).await.unwrap().unwrap(), Status::Running, "the workspace was not started again");
    assert_eq!(w.filed(), vec![stamp(1)], "the reload answered before the filing in flight was appended");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pass_in_flight_at_a_step_down_ends_with_the_delivery() {
    let w = world(true).await;
    serve(&w.lv, posts(&[1, 2]));
    let arm = Holds::here().arm("chat.mail.pass").key(&w.folder).hold(1);
    let chat = w.start_in(&w.fake, &Registry::new(), Duration::from_millis(50)).await;
    let service = w.service(&chat);
    arm.reached_async(0).await;
    w.home.list("elsewhere");
    assert!(soon(|| chat.status() == Status::NotHub).await, "the chat did not step down");
    // The held pass's job owns a clone of the service: an ended job gives it back, a detached one keeps it.
    assert!(soon(|| Arc::strong_count(&service) == 1).await, "{} holders of the service remain: a pass outlived the delivery", Arc::strong_count(&service));
    arm.release(0);
    assert_eq!((w.inbox(), w.boundary()), (Vec::<String>::new(), None));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_starts_read_in_flight_does_not_hold_the_end_and_nothing_is_filed_after_it() {
    let w = world(true).await;
    std::fs::create_dir(w.th.home.join("inbox").join("trapnoread.jsonl")).unwrap();
    let chat = w.start_in(&w.fake, &Registry::new(), Duration::from_millis(50)).await;
    let service = w.service(&chat);
    send(&w.lv, 1, "e1");
    until("the event taken", || w.lv.acked.load(Ordering::SeqCst) >= 1).await;
    w.home.list("elsewhere");
    assert!(soon(|| chat.status() == Status::NotHub).await, "the chat did not step down");
    assert!(matches!(service.status(), ServiceStatus::Stopped));
    // The inbox is readable now; nothing reads it for a chat that has ended.
    std::fs::remove_dir(w.th.home.join("inbox").join("trapnoread.jsonl")).unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(w.inbox(), Vec::<String>::new());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retirement_waits_for_the_filing_in_flight_and_reports_itself_incomplete_when_that_outlasts_its_bound() {
    let w = world(true).await;
    let registry = Registry::new();
    let chat = w.start_in(&w.fake, &registry, HUB_CHECK).await;
    let (arm, service) = hold_a_filing(&w, &chat).await;
    assert_eq!(retire(&registry, Duration::from_millis(300)).incomplete, vec!["chat"], "a retirement that outlasted its bound said it was complete");
    assert!(matches!(service.status(), ServiceStatus::Running { .. }), "the service stopped under a filing in flight");
    arm.release(0);
    assert!(soon(|| chat.status() == Status::Stopped).await, "the chat never stopped");
    assert_eq!(w.filed(), vec![stamp(1)], "the filing in flight was appended before the service stopped");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_channel_of_50000_roots_completes_its_passes_moves_the_boundary_and_files_an_old_threads_reply() {
    let w = world(true).await;
    // 50,000 roots since the floor; the state file's boundary is at the 49,999th; the newest root and a reply under the
    // first root, which lies at the floor's end of the walk, are the only mail past it.
    let at = |i: u64| format!("{}.000100", 1_700_200_000 + i);
    let (reply, newest) = ("1700250005.000100", "1700250010.000100");
    let mut channel = History::new().parent(&at(1), reply).reply(&at(1), reply);
    for i in 2..50_000 {
        channel = channel.post(&at(i));
    }
    let channel = serve(&w.lv, channel.post(newest));
    std::fs::create_dir_all(w.state_dir()).unwrap();
    let old = Ts::parse(&at(49_999)).unwrap();
    sot_chat::state::write(&w.state_dir(), "T0AAAA", "C0AAAA", &old).unwrap();
    let _chat = w.start(&w.fake).await;
    // A pass that reads in small pages reads on and on: it is stopped at the read count, not waited out.
    let done = || w.boundary().is_some_and(|b| b != old.as_str()) || channel.asked().len() > 60;
    assert!(soon(done).await, "no pass completed");
    let reads = channel.asked().len();
    assert!(reads <= 53, "{reads} reads for a channel of 50,000 roots");
    assert_eq!(w.boundary().as_deref(), Some("1700250008.000100"), "the boundary moves to the horizon, two seconds below the newest post");
    // A post sent after the commit, filed last, says the two the pass found were filed once.
    send_at(&w.lv, "1700250100.000100", "e-last");
    until("the last post", || w.filed().contains(&"1700250100.000100".to_string())).await;
    assert_eq!(w.filed(), vec![reply.to_string(), newest.to_string(), "1700250100.000100".to_string()]);
    assert!(w.inbox()[0].contains(&format!("in the thread of {}", at(1))), "{}", w.inbox()[0]);
}

/// The channel of the stage test: old parent Q (a bot's, so not mail) whose reply R is a member's, and a member's root P
/// that is the channel's newest message; with `s`, a member's root S after them. Every page says it is limited.
fn stage_channel(with_s: bool) -> History {
    let (q, r, p) = ("1700000050.000000", "1700000100.000000", "1700000100.000100");
    let parent = json!({"ts": q, "thread_ts": q, "bot_id": "B0OTHER", "text": "old", "reply_count": 1, "latest_reply": r});
    let h = History::new().raw(parent).reply(q, r).post(p).limited();
    if with_s { h.post("1700000200.000100") } else { h }
}

/// The inbox's lines of `alpha-h`, whole, as the wake's trigger counts them.
fn counted(w: &World) -> usize {
    let text = std::fs::read_to_string(w.th.home.join("inbox").join("alpha-h.jsonl")).unwrap_or_default();
    text.lines().filter(|l| crate::comm::mail::cursor::counts(l.as_bytes(), "alpha-h")).count()
}

#[tokio::test(flavor = "multi_thread")]
async fn with_the_window_closed_a_channel_post_becomes_marked_mail_and_the_rows_reply_reaches_its_thread() {
    let (q, r, p, s, t) = ("1700000050.000000", "1700000100.000000", "1700000100.000100", "1700000200.000100", "1700000300.000100");
    let w = world_at("1700000000.000000").await;
    // Made while no socket ran: the history holds P and the old thread Q with its reply R, and the first read of Q's
    // thread fails.
    serve(&w.lv, stage_channel(false));
    w.lv.replies_fail.store(true, Ordering::SeqCst);
    let chat = w.start(&w.fake).await;

    // 1. The first pass files P, although it lies above the horizon; Q's thread read fails, so R is not filed and no
    // boundary is committed above the floor.
    w.filed_count(1).await;
    // The first pass has ended once the next one reads the thread (a pass is owed again only after a failed one ends).
    assert!(soon(|| w.lv.replies_failed.load(Ordering::SeqCst) >= 2).await, "the failed pass was not tried again (a pass that commits through a failed read is not owed again)");
    assert_eq!(w.filed(), vec![p.to_string()]);
    assert_eq!(counted(&w), 1, "the filing is mail by the wake's own rule");
    let first = &w.inbox()[0];
    assert!(first.starts_with("[chat] From a channel member"), "{first}");
    assert!(first.contains(&format!("ts={p} thread={p} channel=C0AAAA workspace=T0AAAA")), "{first}");
    assert!(w.boundary().is_none_or(|b| b == "1700000000.000000"), "a boundary above the floor after a failed pass had ended: {:?}", w.boundary());

    // 2. After the backoff the next pass files R and completes although every page was limited: the boundary is H.
    w.lv.replies_fail.store(false, Ordering::SeqCst);
    w.filed_count(2).await;
    until("the commit at H", || w.boundary().as_deref() == Some("1700000098.000100")).await;
    assert_eq!(w.filed(), vec![p.to_string(), r.to_string()]);
    assert!(w.inbox()[1].contains(&format!("in the thread of {q}")), "{}", w.inbox()[1]);

    // 3. The socket's root S is filed at once.
    send_at(&w.lv, s, "e-s");
    w.filed_count(3).await;
    assert_eq!(w.filed(), vec![p.to_string(), r.to_string(), s.to_string()]);

    // 4. A restart on the same folders files nothing, and so does a new state folder after it. The history holds S
    // now, so each first pass ends at a boundary the one before did not reach.
    stop(chat);
    let (fake, lv) = live().await;
    serve(&lv, stage_channel(true));
    let chat = w.start(&fake).await;
    until("the restart's first pass", || w.boundary().as_deref() == Some("1700000198.000100")).await;
    stop(chat);
    assert_eq!(w.count_in("alpha-h"), 3, "a restart filed a post again");
    std::fs::remove_dir_all(w.state_dir()).unwrap();
    let (fake, lv) = live().await;
    serve(&lv, stage_channel(true));
    let chat = w.start(&fake).await;
    until("the new state folder's first pass", || w.boundary().is_some()).await;
    assert_eq!(w.count_in("alpha-h"), 3, "a new state folder filed a post again");

    // 5. A poll of alpha-h prints the three and says nothing remains.
    w.th.self_file(&w.th.nopane(), "alpha-h");
    w.th.live("alpha-h", &w.th.root());
    let polled = crate::comm::cli::poll(&w.th.env, &[]);
    assert_eq!(polled.code, 0, "{}", polled.stderr);
    for ts in [p, r, s] {
        assert!(polled.stdout.contains(&format!("ts={ts} ")), "the poll did not print {ts}: {}", polled.stdout);
    }
    assert_eq!(crate::comm::cli::poll(&w.th.env, &[]).stdout, "No new messages.\n");
    w.session(0);

    // 6. The row's reply, from a connection that is not a window: under its bound handle it reaches the fake once, into
    // the thread; under another name, and as a person's, it is refused and adds no Slack call.
    let root = tempfile::tempdir().unwrap();
    let mut services = Services::for_test("t", root.path());
    services.chat = chat.clone();
    services.workspaces = w.rows.clone();
    let mut conn = super::chat_ops_tests::connect(services).await;
    let alpha = w.rows.list()[0].workspace_id.clone();
    let target = super::chat_ops_tests::target(&alpha, "C0AAAA");
    let post = |kind: &str, name: &str| json!({"target": target, "thread": q, "kind": kind, "name": name, "text": "done"});
    let posts = |fake: &FakeSlack| fake.seen().iter().filter(|s| s.path().ends_with("/chat.postMessage")).filter_map(|s| s.json()).collect::<Vec<Value>>();
    let posted = conn.ask(10, sot_protocol::ops::op::CHAT_POST, post("row", "alpha-h")).await;
    assert_eq!(posted["outcome"], "posted", "{posted}");
    let sent = posts(&fake);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!((sent[0]["thread_ts"].as_str(), sent[0]["username"].as_str()), (Some(q), Some("alpha-h (row via Ship of Tools)")), "{}", sent[0]);
    assert_eq!(super::chat_ops_tests::code(&conn.ask(11, sot_protocol::ops::op::CHAT_POST, post("row", "someone-else")).await), "bad_request");
    assert_eq!(super::chat_ops_tests::code(&conn.ask(12, sot_protocol::ops::op::CHAT_POST, post("person", "alpha-h")).await), "bad_request");
    assert_eq!(posts(&fake).len(), 1, "a refused post reached Slack");
    // The socket then sends that row post, another bot's message and a member's root T: only T is the fourth line.
    let mut own = message("C0AAAA", 1);
    own["envelope_id"] = json!("e-own");
    own["payload"]["event"]["ts"] = json!("1700000250.000100");
    own["payload"]["event"]["bot_id"] = json!("B0BOT");
    own["payload"]["event"]["thread_ts"] = json!(q);
    lv.feed.send(own).unwrap();
    let mut other = message("C0AAAA", 2);
    other["envelope_id"] = json!("e-other");
    other["payload"]["event"]["ts"] = json!("1700000260.000100");
    other["payload"]["event"]["bot_id"] = json!("B0OTHER");
    lv.feed.send(other).unwrap();
    send_at(&lv, t, "e-t");
    w.filed_count(4).await;
    assert_eq!(w.filed(), vec![p.to_string(), r.to_string(), s.to_string(), t.to_string()]);
    stop(chat);
}

/// Goal 1 of the figures plan: a row's file in its folder reaches its thread through the three upload steps once, the
/// bytes unauthorized and the share under the row's label, and reads back as the row's own, never as mail.
#[tokio::test(flavor = "multi_thread")]
async fn a_rows_figure_reaches_its_thread_once() {
    let q = "1700000050.000000";
    let png: &[u8] = b"\x89PNG\r\n\x1a\n a row's synthetic figure";
    let w = world(true).await;
    std::fs::write(std::path::Path::new(&w.folder).join("fig.png"), png).unwrap();
    let chat = w.start(&w.fake).await;
    let root = tempfile::tempdir().unwrap();
    let mut services = Services::for_test("t", root.path());
    services.chat = chat.clone();
    services.workspaces = w.rows.clone();
    let mut conn = super::chat_ops_tests::connect(services).await;
    let target = super::chat_ops_tests::target(&w.rows.list()[0].workspace_id, "C0AAAA");
    let req = json!({"target": target, "thread": q, "kind": "row", "name": "alpha-h", "node_id": "files:fig.png", "comment": "the fit"});
    let posted = conn.ask(10, op::CHAT_POST_FILE, req).await;
    assert_eq!(posted["outcome"], "posted", "{posted}");
    let ts = posted["ts"].as_str().expect("the share's ts").to_string();

    // One of each step: the bytes are the file's and carry no authorization; the share names the channel, the thread
    // and the row's label.
    let seen = w.fake.seen();
    let step = |path: &str| seen.iter().filter(|s| s.path() == path).collect::<Vec<_>>();
    let (ask, bytes, share) = (step("/api/files.getUploadURLExternal"), step(Upload::new().path()), step("/api/files.completeUploadExternal"));
    assert_eq!((ask.len(), bytes.len(), share.len()), (1, 1, 1));
    assert_eq!((bytes[0].body.as_slice(), bytes[0].authorization.as_deref()), (png, None));
    let body = share[0].json().unwrap();
    assert_eq!((body["channel_id"].as_str(), body["thread_ts"].as_str()), (Some("C0AAAA"), Some(q)), "{body}");
    assert_eq!(body["initial_comment"], "alpha-h (row via Ship of Tools): the fit");

    // As Slack stores the share: the app's own file_share in the thread, its comment the text. Read back through
    // history it is the row's, under the row's name.
    let file = json!([{"id": "F0FILE", "name": "fig.png", "mimetype": "image/png", "size": png.len()}]);
    let stored = json!({"ts": ts, "thread_ts": q, "subtype": "file_share", "bot_id": "B0BOT", "user": "U0BOT", "text": body["initial_comment"], "files": file});
    let parent = json!({"ts": q, "thread_ts": q, "bot_id": "B0OTHER", "text": "the run", "reply_count": 1, "latest_reply": ts});
    serve(&w.lv, History::new().raw(parent).raw(stored.clone()));
    let page = conn.ask(11, op::CHAT_HISTORY, json!({"target": target, "thread": q, "limit": 5})).await;
    let records = page["records"].as_array().unwrap_or_else(|| panic!("{page}"));
    let record = records.iter().find(|r| r["ts"] == ts.as_str()).unwrap_or_else(|| panic!("{page}"));
    let read = (record["origin"].as_str(), record["author"]["name"].as_str(), record["text"].as_str());
    assert_eq!(read, (Some("row"), Some("alpha-h"), Some("the fit")), "{record}");

    // The socket sends the share and then a member's post: only the member's is mail.
    let mut event = message("C0AAAA", 1);
    event["envelope_id"] = json!("e-share");
    let mut shared = stored;
    shared["type"] = json!("message");
    shared["channel"] = json!("C0AAAA");
    event["payload"]["event"] = shared;
    w.lv.feed.send(event).unwrap();
    send(&w.lv, 2, "e2");
    w.filed_count(1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(w.filed(), vec![stamp(2)], "the row's figure was filed as mail");
    stop(chat);
}

/// The record a member's first post makes, as the daemon files it.
fn post_record(n: u32) -> sot_protocol::chat::ChatRecord {
    use sot_protocol::chat::{ChatAuthor, ChatRecord, Ts};
    let ts = Ts::parse(&stamp(n)).unwrap();
    ChatRecord {
        workspace: "T0AAAA".into(),
        channel: "C0AAAA".into(),
        ts: ts.clone(),
        thread_ts: ts,
        author: ChatAuthor { id: "U0MEMBER".into(), name: "Name-U0MEMBER".into() },
        text: "hello".into(),
        ..ChatRecord::fixture()
    }
}

/// A delivering file maps alpha to a channel of the default workspace `MA` and beta to one of a second, `MB`, each row
/// with a live handle; both workspaces run and both sockets said hello. The registry's turn is held alone, as
/// [`World`]'s is.
struct MailTwo {
    teams: sot_chat::fake_slack::Teams,
    th: TestHome,
    folders: [std::path::PathBuf; 2],
    chat: Arc<Chat>,
    rows: Workspaces,
    home: Home,
}

const MA: &str = "T0AAAA";
const MB: &str = "T0BBBB";

async fn mail_two() -> MailTwo {
    let (a, b) = (MA, MB);
    let teams = sot_chat::fake_slack::Teams::start(&[(a, "Alpha Lab"), (b, "Beta Lab")]).await;
    teams.channels(a, &[("C0AAAA01", "alpha-one", false, true)]);
    teams.channels(b, &[("C0BBBB01", "beta-one", false, true)]);
    let th = TestHome::exclusive();
    th.own_record();
    let mut home = Home::new(Some(SELF));
    home.env = std::mem::take(&mut home.env).with("SOT_COMM_HOME", &th.home).with("SOT_SOCKET", th.home.join("no-daemon.sock"));
    let (fa, fb) = (home.folder("a"), home.folder("b"));
    let floor = "1700000000.000000";
    home.write(
        config::CONFIG_FILE,
        &format!(
            "workspace = \"{a}\"\nchannels = [\"C0AAAA01\", \"C0BBBB01\"]\nmail_after = \"{floor}\"\n\
             [[mapping]]\nfolder = '{}'\nchannel = \"C0AAAA01\"\nmail_after = \"{floor}\"\n\
             [[mapping]]\nfolder = '{}'\nworkspace = \"{b}\"\nchannel = \"C0BBBB01\"\nmail_after = \"{floor}\"\n",
            fa.display(),
            fb.display()
        ),
    );
    let secrets = |w: &str| format!("workspace = \"{w}\"\nbot_token = \"{}\"\napp_token = \"{}\"\nuser_token = \"{}\"\n", teams.bot(w), teams.app_token(w), teams.user(w));
    home.write(config::SECRETS_FILE, &secrets(a));
    home.write(&format!("chat-secrets-{b}.toml"), &secrets(b));
    let rows = Workspaces::new();
    for (label, folder, handle) in [("claude-alpha", &fa, "alpha-h"), ("claude-beta", &fb, "beta-h")] {
        let id = rows.insert(Workspace::from_label(label, folder.clone(), false, "claude".into(), handle.into(), String::new())).workspace_id.clone();
        th.put(handle, json!({"host": SELF, "workspace_id": id, "repo": "r", "root": "/", "last_seen": iso8601_utc_from_secs(unix_now_secs())}));
    }
    let read = |p: &std::path::Path| config::read_file(p);
    let chat = Chat::boot(&home.env, &rows, &read, &Registry::new(), starter(teams.fake()), crate::chat::setup::Verifier::at(teams.fake()), HUB_CHECK, TIMING);
    running(&chat).await;
    until("both workspaces running", || chat.status_of(a) == Status::Running && chat.status_of(b) == Status::Running).await;
    let ids: Vec<String> = rows.list().iter().map(|w| w.workspace_id.clone()).collect();
    for (row, w, c) in [(&ids[0], a, "C0AAAA01"), (&ids[1], b, "C0BBBB01")] {
        let target = ChatTarget { row: row.clone(), workspace: w.into(), channel: ChatChannel::try_from(c.to_string()).unwrap() };
        super::chat_watch_tests::after_hello(&chat, &rows, &target).await;
    }
    MailTwo { teams, th, folders: [fa, fb], chat, rows, home }
}

impl MailTwo {
    /// The mail lines in `handle`'s inbox.
    fn inbox(&self, handle: &str) -> Vec<String> {
        let text = std::fs::read_to_string(self.th.home.join("inbox").join(format!("{handle}.jsonl"))).unwrap_or_default();
        text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter_map(|v| v["msg"].as_str().map(str::to_string)).collect()
    }

    /// Alpha's filing held in the slot (one arm of the hold point at a time, so alpha's alone), and beta's job at the slot
    /// behind it (its author looked up, which comes first).
    async fn beta_behind_alpha(&self) -> sot_platform::test_hold::Arm {
        let hold_a = Holds::here().arm("chat.mail.filing").key(self.folders[0].display()).hold(1);
        self.teams.feed(MA, self.teams.message(MA, "C0AAAA01", 1, "1700000100.000100"));
        hold_a.reached_async(0).await;
        self.teams.feed(MB, self.teams.message(MB, "C0BBBB01", 2, "1700000200.000100"));
        until("beta's name lookup", || !self.teams.calls(MB, "xoxb", "/users.info").is_empty()).await;
        hold_a
    }
}

/// L8 and C8: a member's post in each workspace's channel becomes one mail line in its own row's inbox, each workspace's
/// owner filing through the daemon's one filing slot: while alpha's filing holds it, beta's (its names already looked
/// up, so a slow Slack in one workspace holds no other's) waits, and takes it no sooner than `FILE_GAP` after alpha's
/// returned. No request crosses workspaces (the fake fails the test if one did).
#[tokio::test(flavor = "multi_thread")]
async fn two_workspaces_file_their_own_mail_one_filing_at_a_time() {
    let w = mail_two().await;
    let hold_a = w.beta_behind_alpha().await;
    let released = std::time::Instant::now();
    hold_a.release(0);
    // Beta's line lands only after alpha's filing returned and FILE_GAP passed; had beta not waited for alpha's slot, it
    // would be there at once.
    until("beta's post filed", || w.inbox("beta-h").len() == 1).await;
    let waited = released.elapsed();
    assert!(waited >= sot_chat::delivery::FILE_GAP, "beta's filing landed {waited:?} after alpha's was released, within FILE_GAP");
    assert_eq!(w.inbox("alpha-h").len(), 1, "alpha's filing returned");
    w.chat.request_stop();
    assert!(w.chat.wait_retired(LONG));
    // Retired: nothing more is filed, so the counts are final.
    let (alpha, beta) = (w.inbox("alpha-h"), w.inbox("beta-h"));
    assert_eq!((alpha.len(), beta.len()), (1, 1), "a post filed twice or to the other row");
    assert!(alpha[0].contains(&format!("ts=1700000100.000100 thread=1700000100.000100 channel=C0AAAA01 workspace={MA}")), "{}", alpha[0]);
    assert!(beta[0].contains(&format!("ts=1700000200.000100 thread=1700000200.000100 channel=C0BBBB01 workspace={MB}")), "{}", beta[0]);
}

/// B1 review S4, through the owner: beta's service ends on its own while its filing waits behind alpha's held one; beta's
/// owner drops that filing at once (it has filed nothing), so its end does not wait `MAIL_END` on another workspace's
/// filing, and alpha's filing goes on.
#[tokio::test(flavor = "multi_thread")]
async fn a_filing_waiting_behind_another_workspaces_ends_at_once_with_its_delivery() {
    let w = mail_two().await;
    let hold_a = w.beta_behind_alpha().await;
    // An envelope of another app ends beta's service (`wrong_app`).
    let mut other = w.teams.message(MB, "C0BBBB01", 3, "1700000300.000100");
    other["payload"]["api_app_id"] = json!("A0OTHER");
    w.teams.feed(MB, other);
    let begun = std::time::Instant::now();
    until("beta off", || w.chat.status_of(MB) == Status::Failed("wrong_app")).await;
    assert!(begun.elapsed() < crate::chat::watch::MAIL_END / 3, "beta's end waited on alpha's filing: {:?}", begun.elapsed());
    assert!(w.chat.hub().running_in(MA) && w.inbox("beta-h").is_empty(), "alpha's owner runs and beta's waiting filing filed nothing");
    hold_a.release(0);
    until("alpha's post filed", || w.inbox("alpha-h").len() == 1).await;
    w.chat.request_stop();
    assert!(w.chat.wait_retired(LONG));
    assert!(w.inbox("beta-h").is_empty());
}

/// L9 (B2): a reload that restarts alpha's workspace (its plan changed: its secrets file now holds no user token) leaves beta's running: beta's listener is not
/// started again, and a post fed to beta's socket while alpha restarts is filed once to beta's row.
#[tokio::test(flavor = "multi_thread")]
async fn a_kept_workspaces_delivery_files_through_anothers_reload() {
    let w = mail_two().await;
    let (b_starts, b_sockets) = (w.teams.calls(MB, "xoxb", "/auth.test").len(), w.teams.calls(MB, "xapp", "/apps.connections.open").len());
    let a_starts = w.teams.calls(MA, "xoxb", "/auth.test").len();
    let no_user = format!("workspace = \"{MA}\"\nbot_token = \"{}\"\napp_token = \"{}\"\n", w.teams.bot(MA), w.teams.app_token(MA));
    w.home.write(config::SECRETS_FILE, &no_user);
    let reloading = tokio::spawn({
        let (chat, rows) = (w.chat.clone(), w.rows.clone());
        async move { chat.reload_for(&chat.setup().await, &rows, MA).await }
    });
    w.teams.feed(MB, w.teams.message(MB, "C0BBBB01", 2, "1700000200.000100"));
    tokio::time::timeout(LONG, reloading).await.expect("the reload").unwrap();
    until("beta's post filed", || w.inbox("beta-h").len() == 1).await;
    assert_eq!(w.chat.status_of(MA), Status::Running);
    assert_eq!(w.teams.calls(MA, "xoxb", "/auth.test").len(), a_starts + 1, "alpha started again");
    assert_eq!(
        (w.teams.calls(MB, "xoxb", "/auth.test").len(), w.teams.calls(MB, "xapp", "/apps.connections.open").len()),
        (b_starts, b_sockets),
        "beta's listener was started again by alpha's change"
    );
    w.chat.request_stop();
    assert!(w.chat.wait_retired(LONG));
    assert_eq!((w.inbox("alpha-h").len(), w.inbox("beta-h").len()), (0, 1));
}
