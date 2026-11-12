//! The chat's delivery through the daemon's own composition, against the exported fake Slack with a live socket: a post
//! becomes marked mail in the mapped row's inbox, and the inboxes are the record of what was filed.

use super::chat_ops_tests::{running, starter};
use super::chat_watch_tests::{live, message, stamp, until, Levers};
use crate::chat::tests::{Home, SELF};
use crate::chat::{Chat, Status, HUB_CHECK};
use crate::comm::registry::registry::{iso8601_utc_from_secs, unix_now_secs};
use crate::comm::test_home::TestHome;
use crate::lifecycle::retire::{retire, Participant, Registry};
use crate::rows::{Workspace, Workspaces};
use serde_json::{json, Value};
use sot_chat::config;
use sot_chat::delivery::Timing;
use sot_chat::fake_slack::{FakeSlack, History};
use sot_chat::service::{Service, Status as ServiceStatus};
use sot_platform::test_hold::Holds;
use sot_protocol::chat::Ts;
use sot_protocol::ops::{ChatChannel, ChatTarget};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

/// Short enough that a failed start read is tried again within a test.
const TIMING: Timing = Timing { read_pace: Duration::from_millis(10), sweep: Duration::from_secs(3600), backoff_first: Duration::from_millis(50), backoff_max: Duration::from_millis(200) };
const LONG: Duration = Duration::from_secs(30);

/// A hub with row alpha (handle `alpha-h`, a live session) mapped to `C0AAAA`, a comm folder this daemon appends to
/// itself, and a fake Slack with a live socket. The chat's environment names the test's comm folder, never the process's.
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
    let th = TestHome::new();
    th.own_record();
    let mut home = Home::new(Some(SELF));
    home.env = std::mem::take(&mut home.env).with("SOT_COMM_HOME", &th.home).with("SOT_SOCKET", th.home.join("no-daemon.sock"));
    let folder = home.folder("a");
    let own = [folder.to_string_lossy().into_owned()];
    if delivers { home.delivering_toml(&own) } else { home.chat_toml(&own) }
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
        let chat = Chat::boot(&self.home.env, &self.rows, &read, registry, starter(fake), check, TIMING);
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
