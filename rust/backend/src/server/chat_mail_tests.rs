//! The chat's delivery through the daemon's own composition, against the exported fake Slack with a live socket: a post
//! becomes marked mail in the mapped row's inbox, and the inboxes are the record of what was filed.

use super::chat_ops_tests::{running, starter};
use super::chat_watch_tests::{live, message, stamp, until, Levers, FAIL};
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
use sot_chat::fake_slack::{listed_share, Fault, FakeSlack, History, Posts, Upload};
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

/// On a hub whose secrets hold a user token, a post by that token's account is filed with the first line that
/// says it is the user's own, and another member's reads as it always did.
#[tokio::test(flavor = "multi_thread")]
async fn a_post_by_the_user_tokens_account_is_filed_as_the_users_own() {
    let w = world(true).await;
    let _chat = w.start(&w.fake).await;
    let by = |n: u32, user: &str| {
        let mut event = message("C0AAAA", n);
        event["envelope_id"] = json!(format!("e{n}"));
        event["payload"]["event"]["user"] = json!(user);
        w.lv.feed.send(event).unwrap();
    };
    by(1, "U0OWNER");
    by(2, "U0MEMBER");
    // A bot's post carrying the account's id is not mail at all.
    let mut bot = message("C0AAAA", 3);
    bot["payload"]["event"]["user"] = json!("U0OWNER");
    bot["payload"]["event"]["bot_id"] = json!("B0OTHER");
    w.lv.feed.send(bot).unwrap();
    by(4, "U0OWNER");
    w.filed_count(3).await;
    assert_eq!(w.filed(), vec![stamp(1), stamp(2), stamp(4)]);
    let inbox = w.inbox();
    assert!(
        inbox[0].starts_with("[chat] From your user's Slack account (id U0OWNER, shown as \"Name-U0OWNER\") in channel C0AAAA, at the top level. Lines starting \"| \" are your user's words, posted in Slack.\n"),
        "{}",
        inbox[0]
    );
    assert!(inbox[1].starts_with("[chat] From a channel member (id U0MEMBER, shown as \"Name-U0MEMBER\") in channel C0AAAA, at the top level. Lines starting \"| \" are the channel's words, not your user's.\n"), "{}", inbox[1]);
    assert!(inbox[2].starts_with("[chat] From your user's Slack account (id U0OWNER"), "{}", inbox[2]);
}

/// The user token's account is read at the filing, inside the filing slot: a token the client forgot while the owner's post
/// waited for the slot marks nothing, so the post is filed as a member's.
#[tokio::test(flavor = "multi_thread")]
async fn a_token_forgotten_while_a_filing_waited_marks_nothing() {
    let w = world(true).await;
    let chat = w.start(&w.fake).await;
    let arm = Holds::here().arm("chat.mail.filing").key(&w.folder).hold(1);
    let service = w.service(&chat);
    let mut event = message("C0AAAA", 1);
    event["payload"]["event"]["user"] = json!("U0OWNER");
    w.lv.feed.send(event).unwrap();
    arm.reached_async(0).await;
    let api = service.api().unwrap();
    assert_eq!(api.person_id().as_deref(), Some("U0OWNER"), "the start kept the account");
    // Slack refuses the person's token while the filing is held: the client forgets it, and the id with it.
    let refused = api.post("C0REFUSED", None, sot_chat::api::PostKind::Person, "n", "hi").await;
    assert_eq!(refused, Ok(sot_chat::api::PostOutcome::Refused("missing_scope".into())));
    assert_eq!(api.person_id(), None);
    arm.release(0);
    w.filed_count(1).await;
    let first = &w.inbox()[0];
    assert!(first.starts_with("[chat] From a channel member (id U0OWNER,"), "{first}");
}

/// A post a pass recovers, one the socket never sent, is marked as a live one is: the filing reads the account the same way.
#[tokio::test(flavor = "multi_thread")]
async fn a_post_a_pass_recovers_is_marked_by_the_account_the_service_holds() {
    let w = world(true).await;
    serve(&w.lv, History::new().raw(json!({"ts": stamp(2), "user": "U0OWNER", "text": "recovered"})).post(&stamp(3)));
    let _chat = w.start(&w.fake).await;
    w.filed_count(2).await;
    let inbox = w.inbox();
    let mine = inbox.iter().find(|m| m.contains(&format!("ts={} ", stamp(2)))).expect("the owner's post is filed");
    assert!(mine.starts_with("[chat] From your user's Slack account (id U0OWNER,"), "{mine}");
    let other = inbox.iter().find(|m| m.contains(&format!("ts={} ", stamp(3)))).expect("the member's post is filed");
    assert!(other.starts_with("[chat] From a channel member (id U0MEM,"), "{other}");
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
    // The other computer's filing of the first post is in an inbox that cannot be read now (a directory in its place), and
    // so is a second inbox. The first post is in the channel's history only: a pass is the one thing that could file it
    // again. The start's read is all or nothing (`lines_from` fails at an inbox it cannot read and keeps nothing), so the
    // first inbox is restored in two calls while the second is still unreadable: a read that lands between them, with the
    // first absent, still fails whole, and one removal of the second makes every inbox readable at once.
    serve(&w.lv, posts(&[1, 2]));
    let inbox = w.th.home.join("inbox").join("b.jsonl");
    let before = sot_chat::mail::body(&post_record(1), None, None);
    std::fs::write(&inbox, format!("{}\n", json!({"from": "#chat", "to": "b", "repo": "daemon", "msg": before, "ts": "2026-01-01T00:00:00Z"}))).unwrap();
    let kept = std::fs::read(&inbox).unwrap();
    std::fs::remove_file(&inbox).unwrap();
    std::fs::create_dir(&inbox).unwrap();
    let unreadable = w.th.home.join("inbox").join("c.jsonl");
    std::fs::create_dir(&unreadable).unwrap();
    let _chat = w.start(&w.fake).await;
    send(&w.lv, 2, "e2");
    until("the event taken", || w.lv.acked.load(Ordering::SeqCst) >= 1).await;
    // Several tries on the backoff go by, and nothing is filed and no pass is made.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!((w.inbox(), w.boundary()), (Vec::<String>::new(), None), "a post was filed or passed while an inbox could not be read");
    // Once it reads, the first pass files nothing the inbox holds, and files the other post once.
    std::fs::remove_dir(&inbox).unwrap();
    std::fs::write(&inbox, kept).unwrap();
    std::fs::remove_dir(&unreadable).unwrap();
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

    // As Slack lists the share: the app's own message in the thread with no subtype, its label and comment the text.
    // Read back through history it is the row's, under the row's name.
    let file = json!({"id": "F0FILE", "name": "fig.png", "mimetype": "image/png"});
    let stored = listed_share(&ts, Some(q), "B0BOT", "U0BOT", body["initial_comment"].as_str().unwrap(), &file);
    let parent = json!({"ts": q, "thread_ts": q, "bot_id": "B0OTHER", "text": "the run", "reply_count": 1, "latest_reply": ts});
    serve(&w.lv, History::new().raw(parent).raw(stored.clone()));
    let page = conn.ask(11, op::CHAT_HISTORY, json!({"target": target, "thread": q, "limit": 5})).await;
    let records = page["records"].as_array().unwrap_or_else(|| panic!("{page}"));
    let record = records.iter().find(|r| r["ts"] == ts.as_str()).unwrap_or_else(|| panic!("{page}"));
    let read = (record["origin"].as_str(), record["author"]["name"].as_str(), record["text"].as_str());
    assert_eq!(read, (Some("row"), Some("alpha-h"), Some("the fit")), "{record}");

    // The socket sends the share and then a member's post: only the member's is mail. What Slack's event of a share
    // carries is unmeasured, so it is sent under the `file_share` subtype and again with none.
    let mut event = message("C0AAAA", 1);
    event["envelope_id"] = json!("e-share");
    let mut shared = stored;
    shared["type"] = json!("message");
    shared["subtype"] = json!("file_share");
    shared["channel"] = json!("C0AAAA");
    let mut bare = shared.clone();
    bare.as_object_mut().unwrap().remove("subtype");
    bare["ts"] = json!("1700000800.000200");
    event["payload"]["event"] = shared;
    w.lv.feed.send(event).unwrap();
    let mut event = message("C0AAAA", 1);
    event["envelope_id"] = json!("e-share-bare");
    event["payload"]["event"] = bare;
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

/// A mail names its channel by name when the service's name cache holds it at the filing (the start's lookup of the mapped
/// channels, no call at filing time), and by id alone when it does not.
#[tokio::test(flavor = "multi_thread")]
async fn a_filed_mail_names_its_channel_when_the_name_cache_holds_it() {
    let w = world(true).await;
    *w.lv.channel_name.lock().unwrap() = Some("build-notes".into());
    let chat = w.start(&w.fake).await;
    let service = w.service(&chat);
    let api = service.api().unwrap();
    until("the start's lookup of the mapped channel's name", || api.cached_name("C0AAAA").is_some()).await;
    let asked = || w.fake.seen().iter().filter(|s| s.path().ends_with("/conversations.info")).count();
    let before = asked();
    send(&w.lv, 1, "e1");
    w.filed_count(1).await;
    assert_eq!(asked(), before, "a filing makes no call for the name");
    assert!(w.inbox()[0].contains(" in channel #build-notes (C0AAAA), at the top level. "), "{}", w.inbox()[0]);
    assert_eq!(w.filed(), vec![stamp(1)], "the second line is the filing's identity as before");
    stop(chat);
}

/// A row's connection to the running chat, the temp root its services stand on, and the row's id.
async fn session(w: &World, chat: &Arc<Chat>) -> (super::chat_ops_tests::Conn, tempfile::TempDir, String) {
    let root = tempfile::tempdir().unwrap();
    let mut services = Services::for_test("t", root.path());
    services.chat = chat.clone();
    services.workspaces = w.rows.clone();
    (super::chat_ops_tests::connect(services).await, root, w.rows.list()[0].workspace_id.clone())
}

/// A session's `chat.read` as the verb sends it.
fn read_of(row: &str, channel: &str, name: &str, thread: Option<&str>, before: Option<&str>) -> Value {
    let mut req = json!({"target": super::chat_ops_tests::target(row, channel), "name": name});
    for (key, value) in [("thread", thread), ("before", before)] {
        if let Some(value) = value {
            req[key] = json!(value);
        }
    }
    req
}

/// The reads the fake's history and replies answered, by path.
fn reads(fake: &FakeSlack) -> Vec<sot_chat::fake_slack::Seen> {
    fake.seen().into_iter().filter(|s| s.path().ends_with("/conversations.history") || s.path().ends_with("/conversations.replies")).collect()
}

fn lines_of(reply: &Value) -> Vec<String> {
    reply["lines"].as_array().unwrap_or_else(|| panic!("{reply}")).iter().map(|l| l.as_str().unwrap().to_string()).collect()
}

/// The app's own file share, as Slack lists it (no subtype): a row's label and comment, the file, in `thread` when given.
fn stored_share(ts: &str, thread: Option<&str>, comment: &str, file: &str, name: &str) -> Value {
    listed_share(ts, thread, "B0BOT", "U0BOT", &format!("alpha-h (row via Ship of Tools): {comment}"), &json!({"id": file, "name": name, "mimetype": "image/png"}))
}

/// A session's file posts, listed by Slack as shares with no subtype, are read back as the session's own under its
/// handle, with the label left out of the words and the file's id on the line: with a comment, and with the label alone.
#[tokio::test(flavor = "multi_thread")]
async fn a_session_reads_its_own_file_posts_back_as_its_own() {
    let q = "1700000050.000000";
    let w = world(false).await;
    std::fs::write(std::path::Path::new(&w.folder).join("fig.png"), b"\x89PNG\r\n\x1a\n a row's synthetic figure").unwrap();
    let chat = w.start(&w.fake).await;
    let (mut conn, _root, alpha) = session(&w, &chat).await;
    let target = super::chat_ops_tests::target(&alpha, "C0AAAA");

    let with = conn.ask(10, op::CHAT_POST_FILE, json!({"target": target, "thread": q, "kind": "row", "name": "alpha-h", "node_id": "files:fig.png", "comment": "the fit"})).await;
    let bare = conn.ask(11, op::CHAT_POST_FILE, json!({"target": target, "thread": q, "kind": "row", "name": "alpha-h", "node_id": "files:fig.png"})).await;
    let (with_at, bare_at) = (with["ts"].as_str().expect("a share's ts").to_string(), "1700000800.000200");
    assert_eq!(bare["outcome"], "posted", "{bare}");

    // The fake answers every share under one time, so the second is listed under a time of the test's own. Each is
    // listed with the text its post sent.
    let sent: Vec<Value> = w.fake.seen().into_iter().filter(|s| s.path() == "/api/files.completeUploadExternal").map(|s| s.json().unwrap()).collect();
    let file = json!({"id": "F0FILE", "name": "fig.png", "mimetype": "image/png"});
    let text = |i: usize| sent[i]["initial_comment"].as_str().unwrap().to_string();
    let parent = json!({"ts": q, "thread_ts": q, "user": "U0MEM", "text": "the run", "reply_count": 2, "latest_reply": bare_at});
    let first = listed_share(&with_at, Some(q), "B0BOT", "U0BOT", &text(0), &file);
    let second = listed_share(bare_at, Some(q), "B0BOT", "U0BOT", &text(1), &file);
    serve(&w.lv, History::new().raw(parent).raw(first).raw(second));
    let read = conn.ask(12, op::CHAT_READ, read_of(&alpha, "C0AAAA", "alpha-h", Some(q), None)).await;
    let found: Vec<String> = lines_of(&read).into_iter().filter(|l| l.contains("F0FILE")).collect();
    assert_eq!(
        found,
        [
            format!("{with_at} reply thread={q} session \"alpha-h\" files=F0FILE:\"fig.png\" | the fit"),
            format!("{bare_at} reply thread={q} session \"alpha-h\" files=F0FILE:\"fig.png\" |"),
        ]
    );
    stop(chat);
}

/// A session's read of its channel is one scan of the newest fifty messages, oldest first, its deleted parent one
/// `not-shown` line, and it says where the older ones begin: the next read, before that time, holds the rest and none twice.
#[tokio::test(flavor = "multi_thread")]
async fn a_session_reads_its_own_channel_in_one_scan_and_is_told_where_older_ones_begin() {
    let w = world(false).await;
    let chat = w.start(&w.fake).await;
    let tombstone = json!({"ts": stamp(59), "subtype": "tombstone", "thread_ts": stamp(59), "latest_reply": stamp(90)});
    let channel = (1..=58).fold(History::new(), |h, n| h.post(&stamp(n))).raw(tombstone).raw(stored_share(&stamp(60), None, "the fit", "F0FILE", "fit.png"));
    serve(&w.lv, channel);
    let (mut conn, _root, alpha) = session(&w, &chat).await;

    let first = conn.ask(10, op::CHAT_READ, read_of(&alpha, "C0AAAA", "alpha-h", None, None)).await;
    let seen = reads(&w.fake);
    assert_eq!(seen.len(), 1, "one read of Slack");
    let params: Vec<(&str, Option<String>)> = ["oldest", "inclusive", "limit", "cursor", "ts"].iter().map(|k| (*k, seen[0].param(k))).collect();
    assert_eq!(params, [("oldest", Some("1.000000".into())), ("inclusive", Some("false".into())), ("limit", Some("50".into())), ("cursor", None), ("ts", None)]);
    assert!(seen[0].path().ends_with("/conversations.history") && seen[0].param("latest").is_some());

    let lines = lines_of(&first);
    assert_eq!(lines.len(), 50, "{lines:?}");
    assert!(lines[0].starts_with(&format!("{} top person \"Name-U0MEM\" files=none | text {}", stamp(11), stamp(11))), "{}", lines[0]);
    assert_eq!(lines[48], format!("{} top not-shown newest-reply={}", stamp(59), stamp(90)));
    assert_eq!(lines[49], format!("{} top session \"alpha-h\" files=F0FILE:\"fit.png\" | the fit", stamp(60)));
    assert_eq!(lines.iter().filter(|l| l.contains("not-shown")).count(), 1);
    assert_eq!((first["more"].as_str(), first.get("limited")), (Some(stamp(11).as_str()), None), "{first}");

    // The second read, before the oldest the first held, holds the other ten and says nothing is older.
    let second = conn.ask(11, op::CHAT_READ, read_of(&alpha, "C0AAAA", "alpha-h", None, Some(&stamp(11)))).await;
    let rest = lines_of(&second);
    assert_eq!(rest.len(), 10, "{rest:?}");
    assert_eq!(second.get("more"), None, "{second}");
    let seen = reads(&w.fake);
    assert_eq!((seen.len(), seen[1].param("latest")), (2, Some(stamp(11))));
    let times: Vec<&str> = lines.iter().chain(&rest).map(|l| l.split(' ').next().unwrap()).collect();
    let mut once = times.clone();
    once.sort_unstable();
    once.dedup();
    assert_eq!((once.len(), times.len()), (60, 60), "a message was read twice or missed");
    stop(chat);
}

/// A thread read lists the parent first and then the replies, and a thread Slack does not find is empty, never a refusal.
#[tokio::test(flavor = "multi_thread")]
async fn a_session_reads_a_thread_and_one_slack_does_not_find_is_empty() {
    let w = world(false).await;
    let chat = w.start(&w.fake).await;
    let (q, r1, r2) = ("1700000050.000000", "1700000060.000000", "1700000070.000000");
    let parent = json!({"ts": q, "thread_ts": q, "user": "U0MEM", "text": "the run", "reply_count": 2, "latest_reply": r2});
    serve(&w.lv, History::new().raw(parent).reply(q, r1).raw(stored_share(r2, Some(q), "the fit", "F0FILE", "fit.png")));
    let (mut conn, _root, alpha) = session(&w, &chat).await;

    let found = conn.ask(10, op::CHAT_READ, read_of(&alpha, "C0AAAA", "alpha-h", Some(q), None)).await;
    let seen = reads(&w.fake);
    assert!(seen[0].path().ends_with("/conversations.replies") && seen[0].param("ts").as_deref() == Some(q), "{:?}", seen[0].path());
    assert_eq!(
        lines_of(&found),
        [
            format!("{q} top replies=2 person \"Name-U0MEM\" files=none | the run"),
            format!("{r1} reply thread={q} person \"Name-U0MEM\" files=none | text {r1}"),
            format!("{r2} reply thread={q} session \"alpha-h\" files=F0FILE:\"fit.png\" | the fit"),
        ]
    );
    assert_eq!(found.get("more"), None, "{found}");

    // A thread Slack answers `thread_not_found` reads as no message, with no refusal.
    let none = conn.ask(11, op::CHAT_READ, read_of(&alpha, "C0AAAA", "alpha-h", Some("1700000999.000100"), None)).await;
    assert_eq!(none, json!({"lines": []}));
    assert_eq!(reads(&w.fake).len(), 2);
    stop(chat);
}

/// A thread of more than 50 replies is followed through its `more` time: the thread's first message heads every page, and
/// each reply is on one page only.
#[tokio::test(flavor = "multi_thread")]
async fn a_thread_followed_through_its_more_time_lists_each_reply_once_and_its_first_message_on_every_page() {
    let w = world(false).await;
    let chat = w.start(&w.fake).await;
    let q = "1700000050.000000";
    let reply = |n: u32| format!("17000001{n:02}.000100");
    let parent = json!({"ts": q, "thread_ts": q, "user": "U0MEM", "text": "the run", "reply_count": 60, "latest_reply": reply(60)});
    serve(&w.lv, (1..=60).fold(History::new().raw(parent), |h, n| h.reply(q, &reply(n))));
    let (mut conn, _root, alpha) = session(&w, &chat).await;

    let first = conn.ask(10, op::CHAT_READ, read_of(&alpha, "C0AAAA", "alpha-h", Some(q), None)).await;
    let page_one = lines_of(&first);
    assert_eq!((page_one.len(), first["more"].as_str()), (51, Some(reply(11).as_str())), "{first}");
    let second = conn.ask(11, op::CHAT_READ, read_of(&alpha, "C0AAAA", "alpha-h", Some(q), first["more"].as_str())).await;
    let page_two = lines_of(&second);
    assert_eq!((page_two.len(), second.get("more")), (11, None), "{second}");
    for page in [&page_one, &page_two] {
        assert!(page[0].starts_with(&format!("{q} top replies=60 person ")), "the first message heads the page: {}", page[0]);
    }
    let replies: Vec<&str> = page_one.iter().chain(&page_two).map(|l| l.split(' ').next().unwrap()).filter(|t| *t != q).collect();
    let mut once = replies.clone();
    once.sort_unstable();
    once.dedup();
    assert_eq!((once.len(), replies.len()), (60, 60), "a reply was read twice or missed");
    stop(chat);
}

/// Slack's history refuses a zero lower bound (`invalid_ts_oldest`, measured on 2026-11-26), so no read of a session's
/// channel, with or without a bound, asks from zero, and each is answered as a read and not as a refusal.
#[tokio::test(flavor = "multi_thread")]
async fn a_channel_read_never_sends_slack_a_zero_lower_bound() {
    let w = world(false).await;
    let chat = w.start(&w.fake).await;
    serve(&w.lv, (1..=5).fold(History::new(), |h, n| h.post(&stamp(n))));
    let (mut conn, _root, alpha) = session(&w, &chat).await;
    for (id, before) in [(10, None), (11, Some(stamp(3))), (12, Some("1.000001".to_string()))] {
        let reply = conn.ask(id, op::CHAT_READ, read_of(&alpha, "C0AAAA", "alpha-h", None, before.as_deref())).await;
        assert!(reply.get("lines").is_some(), "before {before:?}: {reply}");
    }
    let seen = reads(&w.fake);
    assert_eq!(seen.len(), 3);
    assert!(seen.iter().all(|s| s.param("oldest").as_deref() == Some("1.000000")), "{:?}", seen.iter().map(|s| s.param("oldest")).collect::<Vec<_>>());
    stop(chat);
}

/// A read Slack refuses is logged once with Slack's own code, the row, the channel and the thread, and never a message's
/// text; a thread Slack does not find reads as empty and logs nothing.
#[test]
fn a_refused_read_is_logged_once_with_slacks_code_and_never_a_message() {
    let text = super::chat_setup_tests::logged(|| async {
        let w = world(false).await;
        let chat = w.start(&w.fake).await;
        let (mut conn, _root, alpha) = session(&w, &chat).await;
        // History answers `internal_error` and replies `unknown_method`: each a refusal by Slack.
        w.lv.mode.store(FAIL, Ordering::SeqCst);
        let channel = conn.ask(10, op::CHAT_READ, read_of(&alpha, "C0AAAA", "alpha-h", None, None)).await;
        assert_eq!(super::chat_ops_tests::code(&channel), "failed", "{channel}");
        let thread = conn.ask(11, op::CHAT_READ, read_of(&alpha, "C0AAAA", "alpha-h", Some("1700000100.000100"), None)).await;
        assert_eq!(super::chat_ops_tests::code(&thread), "failed", "{thread}");
        // A thread Slack does not find is no refusal.
        serve(&w.lv, History::new().post(&stamp(1)).raw(json!({"ts": stamp(2), "user": "U0MEM", "text": "ZQTEXTZ"})));
        let none = conn.ask(12, op::CHAT_READ, read_of(&alpha, "C0AAAA", "alpha-h", Some("1700000999.000100"), None)).await;
        assert_eq!(none["lines"], json!([]), "{none}");
        stop(chat);
    });
    let refused: Vec<&str> = text.lines().filter(|l| l.contains("a session's read was refused")).collect();
    assert_eq!(refused.len(), 2, "{text}");
    for (line, id, why, thread) in [(refused[0], 10, "internal_error", ""), (refused[1], 11, "unknown_method", "1700000100.000100")] {
        assert!(line.contains(" WARN "), "{line}");
        for field in [format!("id={id}"), "op=\"chat.read\"".into(), format!("why=\"{why}\""), "channel=\"C0AAAA\"".into(), format!("thread=\"{thread}\""), "row=".into()] {
            assert!(line.contains(&field), "{field} is missing from {line}");
        }
    }
    for secret in ["ZQTEXTZ", "xoxb-"] {
        assert!(!text.contains(secret), "a log line holds {secret}: {text}");
    }
}

/// A name that is not the row's bound handle, another project's channel, a row that is gone and a request that is none
/// are each refused before Slack is asked.
#[tokio::test(flavor = "multi_thread")]
async fn a_read_under_another_name_or_of_another_projects_channel_reaches_no_slack_call() {
    let w = world(false).await;
    let chat = w.start(&w.fake).await;
    let (mut conn, _root, alpha) = session(&w, &chat).await;
    let code = |reply: &Value| super::chat_ops_tests::code(reply).to_string();
    for (n, name) in ["someone-else", "ALPHA-H", " alpha-h", ""].into_iter().enumerate() {
        let reply = conn.ask(10 + n as u64, op::CHAT_READ, read_of(&alpha, "C0AAAA", name, None, None)).await;
        assert_eq!((code(&reply), reply.get("lines")), ("bad_request".to_string(), None), "name {name:?}: {reply}");
    }
    let other = conn.ask(20, op::CHAT_READ, read_of(&alpha, "C0BBBB", "alpha-h", None, None)).await;
    assert_eq!(code(&other), "stale_mapping");
    let gone = conn.ask(21, op::CHAT_READ, read_of("ws-gone-0000", "C0AAAA", "alpha-h", None, None)).await;
    assert_eq!(code(&gone), "stale_row");
    let not_a_time = conn.ask(22, op::CHAT_READ, json!({"target": super::chat_ops_tests::target(&alpha, "C0AAAA"), "name": "alpha-h", "before": "17.5"})).await;
    assert_eq!(code(&not_a_time), "bad_request");
    assert_eq!(reads(&w.fake).len(), 0, "a refused read reached Slack");
    stop(chat);
}

/// Slack's share of a file named no message time, so `sot-fe chat post --file` printed only the file's id; the read of the
/// thread names the id beside the message's time, and that time is the delete's.
#[tokio::test(flavor = "multi_thread")]
async fn a_file_post_that_names_no_time_is_found_by_a_read_and_deleted() {
    let q = "1700000050.000000";
    let w = world(false).await;
    w.lv.no_share_ts.store(true, Ordering::SeqCst);
    std::fs::write(std::path::Path::new(&w.folder).join("fig.png"), b"\x89PNG\r\n\x1a\n a row's synthetic figure").unwrap();
    let chat = w.start(&w.fake).await;
    let (mut conn, _root, alpha) = session(&w, &chat).await;
    let target = super::chat_ops_tests::target(&alpha, "C0AAAA");

    let posted = conn.ask(10, op::CHAT_POST_FILE, json!({"target": target, "thread": q, "kind": "row", "name": "alpha-h", "node_id": "files:fig.png", "comment": "the fit"})).await;
    assert_eq!(posted, json!({"outcome": "posted", "file": "F0FILE"}), "the share named no time");

    // As Slack lists that share: the app's own message with no subtype in the thread, under a time the post never heard.
    let at = "1700000800.000300";
    let parent = json!({"ts": q, "thread_ts": q, "user": "U0MEM", "text": "the run", "reply_count": 1, "latest_reply": at});
    serve(&w.lv, History::new().raw(parent).raw(stored_share(at, Some(q), "the fit", "F0FILE", "fig.png")));
    let read = conn.ask(11, op::CHAT_READ, read_of(&alpha, "C0AAAA", "alpha-h", Some(q), None)).await;
    let found: Vec<String> = lines_of(&read).into_iter().filter(|l| l.contains("F0FILE")).collect();
    assert_eq!(found, [format!("{at} reply thread={q} session \"alpha-h\" files=F0FILE:\"fig.png\" | the fit")]);

    let time = found[0].split(' ').next().unwrap();
    let deleted = conn.ask(12, op::CHAT_DELETE, json!({"target": target, "name": "alpha-h", "ts": time})).await;
    assert_eq!(deleted, json!({"outcome": "deleted"}));
    let sent = w.fake.seen().into_iter().rfind(|s| s.path().ends_with("/chat.delete")).and_then(|s| s.json()).unwrap();
    assert_eq!(sent, json!({"channel": "C0AAAA", "ts": at}));
    stop(chat);
}

/// The change's done test, through the daemon's `chat.delete_post`: a session's file post is deleted with its file, files
/// first and the message last; a delete whose `files.delete` Slack performed but whose answer was lost is unknown, and the
/// same ask again finishes it with the same files; and a file also shared in another channel stays, and is said to.
#[tokio::test(flavor = "multi_thread")]
async fn a_sessions_post_is_deleted_with_its_own_files_and_a_second_ask_finishes_a_half_done_one() {
    let (t1, t2, t3) = ("1700000100.000100", "1700000100.000200", "1700000100.000300");
    let w = world(false).await;
    let chat = w.start(&w.fake).await;
    let posts = Arc::new(
        Posts::new("U0BOT", "B0BOT")
            .share("C0AAAA", t1, None, "F0AAAA")
            .share("C0AAAA", t2, None, "F0BBBB")
            .share("C0AAAA", t3, None, "F0CCCC")
            .also_shared("F0CCCC", "C0OTHER", "1700000100.000900"),
    );
    *w.lv.posts.lock().unwrap() = Some(posts.clone());
    let (mut conn, _root, alpha) = session(&w, &chat).await;
    let ask = |n: u64, ts: &str| json!({"target": super::chat_ops_tests::target(&alpha, "C0AAAA"), "name": "alpha-h", "ts": ts, "n": n});
    // The five methods the fake plays, in the order the daemon sent them.
    let sent = |w: &World| -> Vec<String> {
        const PLAYED: [&str; 5] = ["conversations.history", "conversations.replies", "files.info", "files.delete", "chat.delete"];
        w.fake.seen().iter().filter_map(|s| s.path().strip_prefix("/api/").map(String::from)).filter(|m| PLAYED.contains(&m.as_str())).collect()
    };

    // A file post: the message and its file are both gone, and Slack was asked in that order.
    let done = conn.ask(10, op::CHAT_DELETE_POST, ask(10, t1)).await;
    assert_eq!((done["outcome"].as_str(), done["files"]["deleted"].clone()), (Some("deleted"), json!(["F0AAAA"])), "{done}");
    assert_eq!(done["files"]["kept"], json!([]));
    assert_eq!(sent(&w), ["conversations.history", "files.info", "files.delete", "chat.delete"]);
    assert!(!posts.holds_message("C0AAAA", t1) && !posts.holds_file("F0AAAA"), "the fake holds neither the message nor the file");

    // A file delete Slack performed whose answer was lost: unknown, with the message standing; the same ask finishes it.
    posts.fault("files.delete", Fault::DropAfter);
    let lost = conn.ask(11, op::CHAT_DELETE_POST, ask(11, t2)).await;
    assert_eq!(lost, json!({"outcome": "unknown"}), "{lost}");
    assert!(!posts.holds_file("F0BBBB") && posts.holds_message("C0AAAA", t2));
    let again = conn.ask(12, op::CHAT_DELETE_POST, ask(12, t2)).await;
    assert_eq!((again["outcome"].as_str(), again["files"]["deleted"].clone()), (Some("deleted"), json!(["F0BBBB"])), "{again}");
    assert!(!posts.holds_message("C0AAAA", t2));

    // A file also shared in another channel is kept, and the post goes.
    let kept = conn.ask(13, op::CHAT_DELETE_POST, ask(13, t3)).await;
    assert_eq!(kept["outcome"], "deleted", "{kept}");
    assert_eq!((kept["files"]["deleted"].clone(), kept["files"]["kept"].clone()), (json!([]), json!([{"file": "F0CCCC", "why": "shared_elsewhere"}])));
    assert!(!posts.holds_message("C0AAAA", t3) && posts.holds_file("F0CCCC"));
    stop(chat);
}

/// A reply's file post, with the share Slack was measured to list for one (one entry at the reply's own time, the thread's
/// time beside it), is deleted whole through the daemon when its thread is given: asked with no thread nothing is found and
/// only the channel's read is sent.
#[tokio::test(flavor = "multi_thread")]
async fn a_replys_file_post_is_deleted_whole_with_the_share_slack_lists_for_it() {
    let (q, at) = ("1700000100.000100", "1700000100.000300");
    let w = world(false).await;
    let chat = w.start(&w.fake).await;
    let posts = Arc::new(Posts::new("U0BOT", "B0BOT").bot_post("C0AAAA", q, None).share("C0AAAA", at, Some(q), "F0REPLY"));
    *w.lv.posts.lock().unwrap() = Some(posts.clone());
    let (mut conn, _root, alpha) = session(&w, &chat).await;
    let target = super::chat_ops_tests::target(&alpha, "C0AAAA");
    let sent = |w: &World| -> Vec<String> {
        w.fake.seen().iter().filter_map(|s| s.path().strip_prefix("/api/").map(String::from)).filter(|m| m == "conversations.history" || m == "conversations.replies" || m.starts_with("files.") || m == "chat.delete").collect()
    };
    let bare = conn.ask(10, op::CHAT_DELETE_POST, json!({"target": target, "name": "alpha-h", "ts": at})).await;
    assert_eq!(bare, json!({"outcome": "gone"}), "{bare}");
    assert_eq!(sent(&w), ["conversations.history"]);
    assert!(posts.holds_file("F0REPLY") && posts.holds_message("C0AAAA", at));
    let done = conn.ask(11, op::CHAT_DELETE_POST, json!({"target": target, "name": "alpha-h", "ts": at, "thread": q})).await;
    assert_eq!(done, json!({"outcome": "deleted", "files": {"deleted": ["F0REPLY"], "kept": []}}), "{done}");
    assert_eq!(sent(&w)[1..], ["conversations.replies", "files.info", "files.delete", "chat.delete"]);
    assert!(!posts.holds_file("F0REPLY") && !posts.holds_message("C0AAAA", at) && posts.holds_message("C0AAAA", q), "the reply and its file go, the parent stays");
    stop(chat);
}
