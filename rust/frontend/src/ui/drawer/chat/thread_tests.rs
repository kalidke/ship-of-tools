use std::time::{Duration, Instant};

use cosmic_text::FontSystem;
use sot_protocol::chat::{ChatRecord, Ts};
use sot_protocol::ops::{ChatCode, ChatHistoryReq, ChatHistoryRes, ChatTarget, ChatWatchRes, ChatWatchState};

use super::drawer::ChatDrawer;
use super::owner::Out;
use super::row_channel_tests::answer;
use super::view::Step;
use crate::net::dial::HostKey;
use crate::net::transport::{ChatEvt, ChatReply, OutgoingReq};
use crate::ui::render::quad::ScreenRect;
use crate::ui::ResultRowIdentity;

fn host() -> HostKey {
    "a".to_string()
}

fn shown(row: &str) -> Option<ResultRowIdentity> {
    Some(ResultRowIdentity { host: host(), workspace_id: row.to_string() })
}

fn target() -> ChatTarget {
    serde_json::from_value(serde_json::json!({ "row": "ROW1", "workspace": "WS1", "channel": "CH1" })).unwrap()
}

/// The channel's message `n`: a top-level record, its text `message n`.
fn top(n: usize) -> ChatRecord {
    record(&format!("1700000{n:03}.000100"), &format!("1700000{n:03}.000100"), &format!("message {n}"))
}

/// Reply `n` of the thread whose parent is `parent`.
fn reply_to(parent: &Ts, n: usize) -> ChatRecord {
    record(&format!("1800000{n:03}.000100"), parent.as_str(), &format!("reply {n}"))
}

fn record(at: &str, thread: &str, text: &str) -> ChatRecord {
    serde_json::from_value(serde_json::json!({ "workspace": "WS1", "channel": "CH1", "ts": at, "thread_ts": thread,
        "author": { "id": "U1", "name": "Ada" }, "origin": "member", "text": text, "reply_count": 0, "files": [] })).unwrap()
}

fn page(records: &[ChatRecord], next: Option<&str>) -> ChatReply<ChatHistoryRes> {
    ChatReply::Done(serde_json::from_value(serde_json::json!({ "records": records, "next": next })).unwrap())
}

fn history(outs: &[Out]) -> ChatHistoryReq {
    match outs {
        [(_, OutgoingReq::ChatHistory { req })] => req.clone(),
        other => panic!("one history read expected, got {} requests", other.len()),
    }
}

fn reads(outs: &[Out]) -> usize {
    outs.iter().filter(|(_, r)| matches!(r, OutgoingReq::ChatHistory { .. })).count()
}

fn on(d: &mut ChatDrawer, req: &ChatHistoryReq, reply: ChatReply<ChatHistoryRes>, at: Instant) -> Vec<Out> {
    d.on_evt(&host(), ChatEvt::History { req: req.clone(), reply }, at)
}

/// A drawer whose shown row `ROW1` is watched on `CH1` and read whole: the channel holds `held`. The instant of the read.
fn channel(held: &[ChatRecord]) -> (ChatDrawer, Instant) {
    channel_with(held, None)
}

/// The same, with older history below the snapshot when `next` is given.
fn channel_with(held: &[ChatRecord], next: Option<&str>) -> (ChatDrawer, Instant) {
    let t0 = Instant::now();
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, t0);
    d.tick(shown("ROW1"), t0);
    d.tick(shown("ROW1"), t0 + Duration::from_millis(200));
    let mapped = serde_json::json!({ "state": "mapped", "workspace": "WS1", "channel": "CH1", "delivers": false });
    let at = t0 + Duration::from_millis(210);
    d.on_evt(&host(), answer("ROW1", mapped), at);
    let reply = ChatReply::Done(ChatWatchRes { state: ChatWatchState::Complete });
    let outs = d.on_evt(&host(), ChatEvt::Watched { target: target(), reply }, at);
    on(&mut d, &history(&outs), page(held, next), at);
    (d, at)
}

fn body() -> ScreenRect {
    ScreenRect { x: 0.0, y: 0.0, w: 400.0, h: 400.0 }
}

/// Presses the mark keys (`true` toward older messages), as a frame spends them, and opens the marked message's thread.
fn open(d: &mut ChatDrawer, fonts: &mut FontSystem, keys: &[bool]) -> Vec<Out> {
    d.layout(fonts, body(), 1.0);
    keys.iter().for_each(|older| d.mark(*older));
    d.layout(fonts, body(), 1.0);
    d.open_thread()
}

fn joined(d: &ChatDrawer) -> String {
    d.placed_text().join("|")
}

#[test]
fn a_thread_is_read_whole_and_its_replies_match_by_value() {
    let mut fonts = FontSystem::new();
    let held: Vec<ChatRecord> = (0..3).map(top).collect();
    let (mut d, at) = channel(&held);
    let at = |s: u64| at + Duration::from_secs(s);
    let parent = held[2].ts.clone();

    // Opening reads the thread whole: the marked message's replies by its cursor, one read at a time, the parent once.
    let outs = open(&mut d, &mut fonts, &[true]);
    let first = history(&outs);
    assert_eq!((first.thread.as_ref(), first.limit, first.cursor.as_deref(), &first.target), (Some(&parent), 20, None, &target()));
    assert_eq!(d.title(), " chat · CH1 · thread ");
    d.layout(&mut fonts, body(), 1.0);
    assert_eq!(d.placed_text().len(), 1, "the parent alone until a reply comes: {}", joined(&d));
    let rs: Vec<ChatRecord> = (0..2).map(|n| reply_to(&parent, n)).collect();
    // The page is out of order and holds another thread's reply: only this thread's replies join, by time.
    let with_parent = vec![held[2].clone(), rs[1].clone(), rs[0].clone(), reply_to(&held[0].ts, 9)];
    let outs = on(&mut d, &first, page(&with_parent, Some("c1")), at(2));
    let second = history(&outs);
    assert_eq!((second.thread.as_ref(), second.cursor.as_deref()), (Some(&parent), Some("c1")));
    let more = reply_to(&parent, 2);
    assert!(on(&mut d, &second, page(&[more], None), at(3)).is_empty());
    d.layout(&mut fonts, body(), 1.0);
    let text = d.placed_text();
    assert_eq!(text.len(), 4, "the parent and three replies: {}", joined(&d));
    assert!(text[0].contains("message 2") && text[1].contains("reply 0") && text[3].contains("reply 2"), "{}", joined(&d));

    // Escape closes it: the channel is back, and the mark is on the parent.
    assert!(d.close_thread());
    assert!(!d.close_thread(), "nothing to close");
    d.layout(&mut fonts, body(), 1.0);
    assert_eq!(d.marked(), Some(parent.clone()));
    assert_eq!(d.placed_text().len(), 3);
    assert_eq!(d.title(), " chat · CH1 ");

    // U7: a reply to a closed thread's read, or to another thread's, changes nothing.
    let outs = open(&mut d, &mut fonts, &[]);
    let a = history(&outs);
    assert!(d.close_thread());
    assert!(on(&mut d, &a, page(&[reply_to(&parent, 0)], None), at(11)).is_empty(), "a closed thread's reply");
    let other = held[1].ts.clone();
    let b = history(&open(&mut d, &mut fonts, &[true]));
    assert_eq!(b.thread.as_ref(), Some(&other));
    assert!(on(&mut d, &a, page(&[reply_to(&parent, 0)], None), at(13)).is_empty(), "another thread's reply");
    d.layout(&mut fonts, body(), 1.0);
    assert_eq!(d.placed_text().len(), 1, "{}", joined(&d));
    on(&mut d, &b, page(&[reply_to(&other, 5)], None), at(14));
    d.layout(&mut fonts, body(), 1.0);
    assert!(joined(&d).contains("reply 5") && !joined(&d).contains("reply 0"), "{}", joined(&d));
    assert!(d.close_thread());

    // A thread closed and opened again while its first read is in flight: the new read waits, the old reply fills nothing.
    let first = history(&open(&mut d, &mut fonts, &[false]));
    assert!(d.close_thread());
    assert_eq!(reads(&open(&mut d, &mut fonts, &[])), 0, "the new read waits for the old one");
    let outs = on(&mut d, &first, page(&[reply_to(&parent, 0)], None), at(22));
    assert_eq!(history(&outs), first, "the old reply leaves the set, then the new read goes");
    d.layout(&mut fonts, body(), 1.0);
    assert_eq!(d.placed_text().len(), 1, "the old reply filled nothing: {}", joined(&d));
    on(&mut d, &first, page(&[reply_to(&parent, 1)], None), at(23));
    d.layout(&mut fonts, body(), 1.0);
    assert!(joined(&d).contains("reply 1"), "{}", joined(&d));

    // A refused read is asked again after the pace, or after thirty seconds for `limited`, and the title names the code.
    assert!(d.close_thread());
    let req = history(&open(&mut d, &mut fonts, &[]));
    assert!(on(&mut d, &req, ChatReply::Refused(ChatCode::Busy), at(41)).is_empty());
    assert_eq!(d.title(), format!(" chat · CH1 · thread the thread is not read: {} ", ChatCode::Busy.text()));
    assert_eq!(reads(&d.tick(shown("ROW1"), at(45)).0), 0, "inside the pace");
    assert_eq!(history(&d.tick(shown("ROW1"), at(46)).0), req, "after the pace");
    assert!(on(&mut d, &req, ChatReply::Refused(ChatCode::Limited), at(47)).is_empty());
    assert_eq!(reads(&d.tick(shown("ROW1"), at(76)).0), 0, "limited waits longer");
    assert_eq!(history(&d.tick(shown("ROW1"), at(77)).0), req);

    // A link's end keeps the thread open; its walk starts again at `Connected`, from the first page.
    on(&mut d, &req, page(&[reply_to(&parent, 0)], None), at(78));
    assert!(d.on_connection(&host(), false, at(80)).iter().all(|(_, r)| !matches!(r, OutgoingReq::ChatHistory { .. })));
    assert_eq!(d.title(), " chat · CH1 · thread not connected ");
    let outs = d.on_connection(&host(), true, at(81));
    let again = outs.iter().filter(|(_, r)| matches!(r, OutgoingReq::ChatHistory { req } if req.thread.is_some())).count();
    assert_eq!(again, 1, "the walk starts again");
    d.layout(&mut fonts, body(), 1.0);
    assert_eq!(d.placed_text().len(), 1, "from the first page: {}", joined(&d));
    // A reply to the read a link's end left in flight comes to nothing.
    let lost = outs.iter().find_map(|(_, r)| if let OutgoingReq::ChatHistory { req } = r { Some(req.clone()) } else { None }).expect("the read");
    d.on_connection(&host(), false, at(82));
    on(&mut d, &lost, page(&[reply_to(&parent, 7)], None), at(83));
    d.layout(&mut fonts, body(), 1.0);
    assert_eq!(d.placed_text().len(), 1, "{}", joined(&d));

    // A row switch closes it, and so does hiding the drawer.
    d.tick(shown("ROW2"), at(90));
    assert!(!d.title().contains("thread"), "{}", d.title());
    let (mut d, _) = channel(&held);
    open(&mut d, &mut fonts, &[true]);
    d.hide();
    assert_eq!(d.title(), " chat · CH1 ");
}

#[test]
fn a_thread_shares_the_bound_with_the_channel_and_says_what_it_leaves_out() {
    let mut fonts = FontSystem::new();
    let (mut d, at) = channel(&(0..20).map(top).collect::<Vec<_>>());
    // 256 channel records, then the oldest one's thread.
    for chunk in (20..256).collect::<Vec<usize>>().chunks(20) {
        let put: Vec<ChatRecord> = chunk.iter().map(|n| top(*n)).collect();
        let changed = serde_json::from_value(serde_json::json!({ "workspace": "WS1", "channel": "CH1", "put": put, "deleted": [] })).unwrap();
        d.on_evt(&host(), ChatEvt::Changed(changed), at);
    }
    assert_eq!(d.held(), 256);
    let parent = top(0).ts;
    let mut outs = open(&mut d, &mut fonts, &[true; 256]);
    assert_eq!(history(&outs).thread.as_ref(), Some(&parent), "the oldest message's thread");
    let mut seen = 0;
    for p in 0..9 {
        let req = history(&outs);
        let page_replies: Vec<ChatRecord> = (p * 20..p * 20 + 20).map(|n| reply_to(&parent, n)).collect();
        outs = on(&mut d, &req, page(&page_replies, Some("more")), at + Duration::from_secs(p as u64 + 1));
        seen += 20;
        let held = d.held();
        assert!(held >= 100 && held == 256 - d.thread_len(), "the channel gives way below its newest 100: {held} with {seen} read");
        if reads(&outs) == 0 {
            break;
        }
    }
    assert_eq!(reads(&outs), 0, "the walk stops at the bound");
    assert_eq!((d.thread_len(), d.held()), (156, 100));
    assert!(d.title().ends_with("thread the rest of this thread is not shown "), "{}", d.title());
    d.layout(&mut fonts, body(), 1.0);
    assert!(joined(&d).contains("reply 154") && !joined(&d).contains("reply 155"), "the first replies stay: {}", joined(&d));

    // A channel snapshot never replaces thread replies.
    d.on_evt(&host(), ChatEvt::Unreadable, at + Duration::from_secs(20));
    let reply = ChatReply::Done(ChatWatchRes { state: ChatWatchState::Complete });
    let outs = d.on_evt(&host(), ChatEvt::Watched { target: target(), reply }, at + Duration::from_secs(21));
    let snapshot: Vec<ChatRecord> = (200..220).map(top).collect();
    on(&mut d, &history(&outs), page(&snapshot, None), at + Duration::from_secs(22));
    d.layout(&mut fonts, body(), 1.0);
    assert_eq!(d.thread_len(), 156);
    assert!(joined(&d).contains("reply 154"), "{}", joined(&d));

    // The parent gave way: closing passes the mark to a held record by the view's rule.
    assert!(d.close_thread());
    d.layout(&mut fonts, body(), 1.0);
    let mark = d.marked().expect("a mark");
    assert!(mark != parent && d.placed_text().len() > 1, "the mark passed on");

    // The thread's share goes back to the channel (the reader follows the newest, so no anchor holds the trim): it grows past 100 again.
    d.latest();
    d.layout(&mut fonts, body(), 1.0);
    for chunk in (300..400).collect::<Vec<usize>>().chunks(20) {
        let put: Vec<ChatRecord> = chunk.iter().map(|n| top(*n)).collect();
        let changed = serde_json::from_value(serde_json::json!({ "workspace": "WS1", "channel": "CH1", "put": put, "deleted": [] })).unwrap();
        d.on_evt(&host(), ChatEvt::Changed(changed), at);
    }
    assert_eq!(d.held(), 120);
}

/// The channel is read again whole after a loss: `held` is the new snapshot.
fn resnapshot(d: &mut ChatDrawer, held: &[ChatRecord], next: Option<&str>, at: Instant) {
    d.on_evt(&host(), ChatEvt::Unreadable, at);
    let reply = ChatReply::Done(ChatWatchRes { state: ChatWatchState::Complete });
    let outs = d.on_evt(&host(), ChatEvt::Watched { target: target(), reply }, at);
    let page = serde_json::from_value(serde_json::json!({ "records": held, "next": next })).unwrap();
    d.on_evt(&host(), ChatEvt::History { req: history(&outs), reply: ChatReply::Done(page) }, at);
}

#[test]
fn thread_view_compose_and_back_keep_target() {
    let mut fonts = FontSystem::new();
    let held: Vec<ChatRecord> = (0..3).map(top).collect();
    let (mut d, at) = channel(&held);
    let parent = held[1].ts.clone();

    // Nothing is marked, so nothing opens.
    d.layout(&mut fonts, body(), 1.0);
    d.open_thread();
    assert_eq!((d.title(), d.placed_text().len()), (" chat · CH1 ".to_string(), 3));

    // The marked message's thread shows its parent alone; opening another thread meanwhile is nothing.
    open(&mut d, &mut fonts, &[true, true]);
    d.layout(&mut fonts, body(), 1.0);
    assert_eq!(d.title(), " chat · CH1 · thread ");
    assert!(d.placed_text().len() == 1 && d.placed_text()[0].contains("message 1"), "{:?}", d.placed_text());
    d.mark(true);
    d.layout(&mut fonts, body(), 1.0);
    d.open_thread();
    assert!(d.placed_text()[0].contains("message 1"), "{:?}", d.placed_text());

    // A link's end keeps it open.
    d.on_connection(&host(), false, at);
    assert_eq!(d.title(), " chat · CH1 · thread not connected ", "the link's words stay over a thread");
    d.on_connection(&host(), true, at);
    assert!(d.title().starts_with(" chat · CH1 · thread "));

    // Escape closes it: the channel's view is back, with its mark on the parent.
    assert!(d.close_thread());
    assert!(!d.close_thread(), "nothing to close");
    d.layout(&mut fonts, body(), 1.0);
    assert_eq!((d.marked(), d.placed_text().len()), (Some(parent), 3));
    assert!(!d.title().contains("thread"));

    // A row switch closes it, and so does hiding the drawer.
    d.open_thread();
    assert!(d.title().contains("· thread"));
    d.tick(shown("ROW2"), at + Duration::from_secs(1));
    assert_eq!(d.title(), " chat · reading… ");
    let (mut d, _) = channel(&held);
    open(&mut d, &mut fonts, &[true]);
    d.hide();
    assert_eq!(d.title(), " chat · CH1 ");

    // The channel's place is kept: a reader scrolled up is still there after the thread closes.
    let (mut d, _) = channel(&(0..20).map(top).collect::<Vec<_>>());
    d.layout(&mut fonts, body(), 1.0);
    d.mark(true);
    d.layout(&mut fonts, body(), 1.0);
    d.scroll(true, Step::Page);
    d.layout(&mut fonts, body(), 1.0);
    assert!(!d.following());
    d.open_thread();
    d.layout(&mut fonts, body(), 1.0);
    assert!(d.following(), "the thread has a view of its own");
    d.close_thread();
    d.layout(&mut fonts, body(), 1.0);
    assert!(!d.following(), "the channel's position returns");

    // The parent gave way under a resync while its thread was open: closing passes the mark to the nearest newer message.
    let (mut d, at) = channel(&held);
    open(&mut d, &mut fonts, &[true, true]);
    resnapshot(&mut d, &[held[0].clone(), held[2].clone()], None, at);
    d.close_thread();
    d.layout(&mut fonts, body(), 1.0);
    assert_eq!((d.marked(), d.placed_text().len()), (Some(held[2].ts.clone()), 2));

    // `no messages` is the channel's: not over a thread whose channel was emptied meanwhile.
    let (mut d, at) = channel(&held);
    open(&mut d, &mut fonts, &[true]);
    resnapshot(&mut d, &[], None, at);
    assert!(!d.no_messages(), "the thread is shown");
    d.close_thread();
    assert!(d.no_messages());

    // The channel's older-pages words are not shown over a thread.
    let (mut d, at) = channel(&held);
    open(&mut d, &mut fonts, &[true]);
    resnapshot(&mut d, &[], Some("older"), at);
    assert_eq!(d.title(), " chat · CH1 · thread ");
    d.close_thread();
    assert_eq!(d.title(), " chat · CH1 older messages could not be read ");

    // A deleted parent opens as deleted.
    let (mut d, at) = channel(&held);
    let gone = serde_json::from_value(serde_json::json!({ "workspace": "WS1", "channel": "CH1", "put": [], "deleted": [held[1].ts] })).unwrap();
    d.on_evt(&host(), ChatEvt::Changed(gone), at);
    open(&mut d, &mut fonts, &[true, true]);
    d.layout(&mut fonts, body(), 1.0);
    assert!(d.placed_text()[0].ends_with("(message deleted)"), "{:?}", d.placed_text());

    // The thread's view never moves the channel's reading place: the owner holds no anchor (the channel's oldest give way) and asks for no page.
    let (mut d, at) = channel_with(&(0..20).map(top).collect::<Vec<_>>(), Some("older"));
    d.layout(&mut fonts, body(), 1.0);
    d.mark(true);
    d.layout(&mut fonts, body(), 1.0);
    d.scroll(true, Step::Page);
    d.layout(&mut fonts, body(), 1.0);
    assert!(d.owner_anchor().is_some(), "scrolled up");
    d.open_thread();
    d.layout(&mut fonts, body(), 1.0);
    assert_eq!(d.owner_anchor(), None, "the thread's frames set no anchor");
    assert!(d.tick(shown("ROW1"), at + Duration::from_secs(1)).0.is_empty(), "no older page is asked for");

    // A reader at the oldest record when the thread opens: no channel page is asked for while it is open.
    let (mut d, at) = channel_with(&held, Some("older"));
    open(&mut d, &mut fonts, &[true]);
    for s in 1..16 {
        assert!(d.tick(shown("ROW1"), at + Duration::from_secs(s)).0.is_empty(), "tick {s}");
    }
    d.close_thread();
    d.layout(&mut fonts, body(), 1.0);
    assert_eq!(history(&d.tick(shown("ROW1"), at + Duration::from_secs(20)).0).upper.is_some(), true, "the channel pages again once the thread is closed");
}

/// A reply repeated inside one page, or across pages, shows once.
#[test]
fn a_reply_repeated_in_a_page_or_across_pages_shows_once() {
    let mut fonts = FontSystem::new();
    let held: Vec<ChatRecord> = (0..2).map(top).collect();
    let (mut d, at) = channel(&held);
    let parent = held[1].ts.clone();
    let first = history(&open(&mut d, &mut fonts, &[true]));
    let (r0, r1) = (reply_to(&parent, 0), reply_to(&parent, 1));
    let next = history(&on(&mut d, &first, page(&[r1.clone(), r0.clone(), r1.clone()], Some("c")), at));
    assert_eq!(d.thread_len(), 3, "a repeat inside the page");
    on(&mut d, &next, page(&[r0, r1], None), at);
    assert_eq!(d.thread_len(), 3, "a repeat across pages");
}

/// A thread lives only under its channel's watch: another channel's watch, or none, closes it.
#[test]
fn a_thread_closes_when_its_channels_watch_changes_or_stops() {
    let mut fonts = FontSystem::new();
    let held: Vec<ChatRecord> = (0..2).map(top).collect();
    for (reply, why) in [
        (serde_json::json!({ "state": "mapped", "workspace": "WS1", "channel": "CH2", "delivers": false }), "another channel"),
        (serde_json::json!({ "state": "unmapped" }), "unmapped"),
    ] {
        let (mut d, at) = channel(&held);
        open(&mut d, &mut fonts, &[true]);
        assert_eq!(d.thread_len(), 1);
        d.on_evt(&host(), answer("ROW1", reply), at);
        assert_eq!(d.thread_len(), 0, "{why}");
        assert!(!d.title().contains("thread"), "{why}: {}", d.title());
    }
}

/// A read held behind an equal one does not go out after a link change that dropped it.
#[test]
fn a_thread_read_held_behind_an_equal_one_is_cancelled_by_a_link_change() {
    let mut fonts = FontSystem::new();
    let held: Vec<ChatRecord> = (0..2).map(top).collect();
    let (mut d, at) = channel(&held);
    let first = history(&open(&mut d, &mut fonts, &[true]));
    assert!(d.close_thread());
    assert_eq!(reads(&open(&mut d, &mut fonts, &[])), 0, "held behind the first");
    d.on_connection(&host(), false, at);
    let outs = d.on_connection(&host(), true, at);
    assert_eq!(outs.iter().filter(|(_, r)| matches!(r, OutgoingReq::ChatHistory { req } if req.thread.is_some())).count(), 1);
    assert_eq!(reads(&on(&mut d, &first, page(&[], None), at)), 0, "the cancelled read does not go out");
}

/// Each phrase after `thread` is its own, and an unsupported reply has its words.
#[test]
fn the_threads_title_keeps_its_phrases_apart_and_says_an_unsupported_reply() {
    let mut fonts = FontSystem::new();
    let held: Vec<ChatRecord> = (0..2).map(top).collect();
    let (mut d, at) = channel(&held);
    let req = history(&open(&mut d, &mut fonts, &[true]));
    on(&mut d, &req, ChatReply::Refused(ChatCode::Busy), at);
    d.on_connection(&host(), false, at);
    assert_eq!(d.title(), format!(" chat · CH1 · thread the thread is not read: {} · not connected ", ChatCode::Busy.text()));
    d.close_thread();
    let (mut d, at) = channel(&held);
    let req = history(&open(&mut d, &mut fonts, &[true]));
    on(&mut d, &req, ChatReply::Unsupported, at);
    assert_eq!(d.title(), " chat · CH1 · thread this daemon has no chat: update it ");
}

/// Another host's link change leaves the thread's read alone.
#[test]
fn another_hosts_link_change_leaves_the_thread_alone() {
    let mut fonts = FontSystem::new();
    let held: Vec<ChatRecord> = (0..2).map(top).collect();
    let (mut d, at) = channel(&held);
    let parent = held[1].ts.clone();
    let req = history(&open(&mut d, &mut fonts, &[true]));
    let other = "b".to_string();
    assert!(d.on_connection(&other, false, at).is_empty());
    assert_eq!(reads(&d.on_connection(&other, true, at)), 0, "no thread read goes out");
    on(&mut d, &req, page(&[reply_to(&parent, 0)], None), at);
    assert_eq!(d.thread_len(), 2, "the read in flight still fills the thread");
}

/// A restart forgets a refusal: its words go, and it is not asked again.
#[test]
fn a_restart_clears_a_refusal() {
    let mut fonts = FontSystem::new();
    let held: Vec<ChatRecord> = (0..2).map(top).collect();
    let (mut d, at) = channel(&held);
    let req = history(&open(&mut d, &mut fonts, &[true]));
    on(&mut d, &req, ChatReply::Refused(ChatCode::Busy), at);
    assert!(d.title().contains("not read"));
    d.on_connection(&host(), false, at);
    d.on_connection(&host(), true, at);
    assert!(!d.title().contains("not read"), "{}", d.title());
    assert_eq!(reads(&d.tick(shown("ROW1"), at + Duration::from_secs(10)).0), 0, "the old read is not asked again");
}
