//! The chat's delivery through the daemon's own composition, against the exported fake Slack with a live socket: a post
//! becomes marked mail in the mapped row's inbox, and the inboxes are the record of what was filed.

use super::chat_ops_tests::{running, starter};
use super::chat_watch_tests::{live, message, stamp, until, Levers};
use crate::chat::tests::{Home, SELF};
use crate::chat::{Chat, HUB_CHECK};
use crate::comm::registry::registry::{iso8601_utc_from_secs, unix_now_secs};
use crate::comm::test_home::TestHome;
use crate::lifecycle::retire::{Participant, Registry};
use crate::rows::{Workspace, Workspaces};
use serde_json::{json, Value};
use sot_chat::config;
use sot_chat::delivery::Timing;
use sot_chat::fake_slack::FakeSlack;
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
    let alpha = rows.insert(Workspace::from_label("claude-alpha", folder, false, "claude".into(), "alpha-h".into(), String::new()));
    th.put("alpha-h", json!({"host": SELF, "workspace_id": alpha.workspace_id, "repo": "r", "root": "/", "last_seen": iso8601_utc_from_secs(unix_now_secs())}));
    let (fake, lv) = live().await;
    World { th, home, rows, fake, lv }
}

impl World {
    /// The chat as the daemon boots it against `fake`, once its socket has said hello.
    async fn start(&self, fake: &FakeSlack) -> Arc<Chat> {
        let read = |p: &std::path::Path| config::read_file(p);
        let chat = Chat::boot(&self.home.env, &self.rows, &read, &Registry::new(), starter(fake), HUB_CHECK, TIMING);
        running(&chat).await;
        let alpha = self.rows.list()[0].workspace_id.clone();
        let target = ChatTarget { row: alpha, workspace: "T0AAAA".into(), channel: ChatChannel::try_from("C0AAAA".to_string()).unwrap() };
        super::chat_watch_tests::after_hello(&chat, &self.rows, &target).await;
        chat
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
    let inbox = w.th.home.join("inbox").join("b.jsonl");
    let before = sot_chat::mail::body(&post_record(1));
    std::fs::write(&inbox, format!("{}\n", json!({"from": "#chat", "to": "b", "repo": "daemon", "msg": before, "ts": "2026-01-01T00:00:00Z"}))).unwrap();
    let kept = std::fs::read(&inbox).unwrap();
    std::fs::remove_file(&inbox).unwrap();
    std::fs::create_dir(&inbox).unwrap();
    let _chat = w.start(&w.fake).await;
    send(&w.lv, 1, "e1");
    send(&w.lv, 2, "e2");
    until("both events taken", || w.lv.acked.load(Ordering::SeqCst) >= 2).await;
    // Several tries on the backoff go by, and nothing is filed.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(w.inbox(), Vec::<String>::new(), "a post was filed while an inbox could not be read");
    // Once it reads, the post it holds is not filed again, and the other is.
    std::fs::remove_dir(&inbox).unwrap();
    std::fs::write(&inbox, kept).unwrap();
    w.filed_count(1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(w.filed(), vec![stamp(2)]);
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
