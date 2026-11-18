use std::time::{Duration, Instant};

use cosmic_text::FontSystem;
use sot_protocol::chat::{ChatRecord, Ts};
use sot_protocol::ops::{ChatCode, ChatHistoryReq, ChatHistoryRes, ChatPostFileRes, ChatPostReq, ChatPostRes, ChatTarget, ChatWatchRes, ChatWatchState};

use super::compose::{compose_rows, Edit};
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

pub(super) fn shown(row: &str) -> Option<ResultRowIdentity> {
    Some(ResultRowIdentity { host: host(), workspace_id: row.to_string() })
}

fn target() -> ChatTarget {
    serde_json::from_value(serde_json::json!({ "row": "ROW1", "workspace": "WS1", "channel": "CH1" })).unwrap()
}

/// The channel's message `n`: a top-level record, its text `message n`.
pub(super) fn top(n: usize) -> ChatRecord {
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
pub(super) fn channel(held: &[ChatRecord]) -> (ChatDrawer, Instant) {
    channel_with(held, None)
}

/// The same, with older history below the snapshot when `next` is given.
fn channel_with(held: &[ChatRecord], next: Option<&str>) -> (ChatDrawer, Instant) {
    channel_on(held, next, false)
}

/// The same, on a hub whose hello says (`as_person`) whether it posts as the person.
fn channel_on(held: &[ChatRecord], next: Option<&str>, as_person: bool) -> (ChatDrawer, Instant) {
    let t0 = Instant::now();
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, as_person, t0);
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

pub(super) fn body() -> ScreenRect {
    ScreenRect { x: 0.0, y: 0.0, w: 400.0, h: 400.0 }
}

/// Presses the mark keys (`true` toward older messages), as a frame spends them, and opens the marked message's thread.
fn open(d: &mut ChatDrawer, fonts: &mut FontSystem, keys: &[bool]) -> Vec<Out> {
    d.layout(fonts, body(), 1.0, body());
    keys.iter().for_each(|older| d.mark(*older, Instant::now(), Duration::ZERO));
    d.layout(fonts, body(), 1.0, body());
    d.open_thread()
}

pub(super) fn joined(d: &ChatDrawer) -> String {
    d.placed_text().join("|")
}

#[test]
fn a_thread_opening_or_closing_leaves_nothing_owed() {
    let mut fonts = FontSystem::new();
    // A body of 100: a page is a third of it, so two pages do not reach the oldest of twenty records.
    let short = || ScreenRect { h: 100.0, ..body() };
    let (mut d, _) = channel(&(0..20).map(top).collect::<Vec<_>>());
    d.layout(&mut fonts, short(), 1.0, short());
    d.mark(true, Instant::now(), Duration::ZERO);
    d.layout(&mut fonts, short(), 1.0, short());
    // One page at once puts the reader off the newest; the next is eased over a length no frame of the test outlasts.
    d.scroll(true, Step::Page, Instant::now(), Duration::ZERO);
    d.layout(&mut fonts, short(), 1.0, short());
    d.scroll(true, Step::Page, Instant::now(), Duration::from_secs(60));
    d.layout(&mut fonts, short(), 1.0, short());
    assert!(d.moving() && !d.following());
    d.open_thread();
    d.layout(&mut fonts, short(), 1.0, short());
    assert!(!d.moving(), "the thread's view owes nothing");
    d.close_thread();
    d.layout(&mut fonts, short(), 1.0, short());
    assert!(!d.moving() && !d.following(), "the channel's view returns where it was, owing nothing");
}

#[test]
fn left_closes_the_thread_only_over_an_empty_draft() {
    let mut fonts = FontSystem::new();
    let held: Vec<ChatRecord> = (0..3).map(top).collect();
    let (mut d, _) = channel(&held);
    let parent = held[2].ts.clone();
    let row = shown("ROW1");
    // No thread open: nothing closes, and the mark stays.
    open(&mut d, &mut fonts, &[true]);
    assert!(d.close_thread());
    assert!(!d.leave_thread(row.as_ref()), "no thread to close");
    // A draft with anything in it, a space included, keeps the thread open.
    open(&mut d, &mut fonts, &[]);
    for text in ["hi", " "] {
        d.edit(row.as_ref(), Edit::Text(text)).unwrap();
        assert!(!d.leave_thread(row.as_ref()), "{text:?}");
        assert_eq!(d.title(), " chat · CH1 · thread ", "{text:?}");
        while d.draft().is_some_and(|(t, _)| !t.is_empty()) {
            d.edit(row.as_ref(), Edit::Backspace).unwrap();
        }
    }
    // An empty draft: it closes as Escape does, the channel back with the mark on the parent.
    assert!(d.leave_thread(row.as_ref()));
    d.layout(&mut fonts, body(), 1.0, body());
    assert_eq!((d.marked(), d.title(), d.placed_text().len()), (Some(parent), " chat · CH1 ".to_string(), 3));
    assert!(!d.leave_thread(row.as_ref()), "nothing to close");
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
    d.layout(&mut fonts, body(), 1.0, body());
    assert_eq!(d.placed_text().len(), 1, "the parent alone until a reply comes: {}", joined(&d));
    let rs: Vec<ChatRecord> = (0..2).map(|n| reply_to(&parent, n)).collect();
    // The page is out of order and holds another thread's reply: only this thread's replies join, by time.
    let with_parent = vec![held[2].clone(), rs[1].clone(), rs[0].clone(), reply_to(&held[0].ts, 9)];
    let outs = on(&mut d, &first, page(&with_parent, Some("c1")), at(2));
    let second = history(&outs);
    assert_eq!((second.thread.as_ref(), second.cursor.as_deref()), (Some(&parent), Some("c1")));
    let more = reply_to(&parent, 2);
    assert!(on(&mut d, &second, page(&[more], None), at(3)).is_empty());
    d.layout(&mut fonts, body(), 1.0, body());
    let text = d.placed_text();
    assert_eq!(text.len(), 4, "the parent and three replies: {}", joined(&d));
    assert!(text[0].contains("message 2") && text[1].contains("reply 0") && text[3].contains("reply 2"), "{}", joined(&d));

    // Escape closes it: the channel is back, and the mark is on the parent.
    assert!(d.close_thread());
    assert!(!d.close_thread(), "nothing to close");
    d.layout(&mut fonts, body(), 1.0, body());
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
    d.layout(&mut fonts, body(), 1.0, body());
    assert_eq!(d.placed_text().len(), 1, "{}", joined(&d));
    on(&mut d, &b, page(&[reply_to(&other, 5)], None), at(14));
    d.layout(&mut fonts, body(), 1.0, body());
    assert!(joined(&d).contains("reply 5") && !joined(&d).contains("reply 0"), "{}", joined(&d));
    assert!(d.close_thread());

    // A thread closed and opened again while its first read is in flight: the new read waits, the old reply fills nothing.
    let first = history(&open(&mut d, &mut fonts, &[false]));
    assert!(d.close_thread());
    assert_eq!(reads(&open(&mut d, &mut fonts, &[])), 0, "the new read waits for the old one");
    let outs = on(&mut d, &first, page(&[reply_to(&parent, 0)], None), at(22));
    assert_eq!(history(&outs), first, "the old reply leaves the set, then the new read goes");
    d.layout(&mut fonts, body(), 1.0, body());
    assert_eq!(d.placed_text().len(), 1, "the old reply filled nothing: {}", joined(&d));
    on(&mut d, &first, page(&[reply_to(&parent, 1)], None), at(23));
    d.layout(&mut fonts, body(), 1.0, body());
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
    assert!(d.on_connection(&host(), false, false, at(80)).iter().all(|(_, r)| !matches!(r, OutgoingReq::ChatHistory { .. })));
    assert_eq!(d.title(), " chat · CH1 · thread no connection to a ");
    let outs = d.on_connection(&host(), true, false, at(81));
    let again = outs.iter().filter(|(_, r)| matches!(r, OutgoingReq::ChatHistory { req } if req.thread.is_some())).count();
    assert_eq!(again, 1, "the walk starts again");
    d.layout(&mut fonts, body(), 1.0, body());
    assert_eq!(d.placed_text().len(), 1, "from the first page: {}", joined(&d));
    // A reply to the read a link's end left in flight comes to nothing.
    let lost = outs.iter().find_map(|(_, r)| if let OutgoingReq::ChatHistory { req } = r { Some(req.clone()) } else { None }).expect("the read");
    d.on_connection(&host(), false, false, at(82));
    on(&mut d, &lost, page(&[reply_to(&parent, 7)], None), at(83));
    d.layout(&mut fonts, body(), 1.0, body());
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
    d.layout(&mut fonts, body(), 1.0, body());
    assert!(joined(&d).contains("reply 154") && !joined(&d).contains("reply 155"), "the first replies stay: {}", joined(&d));

    // A channel snapshot never replaces thread replies.
    d.on_evt(&host(), ChatEvt::Unreadable, at + Duration::from_secs(20));
    let reply = ChatReply::Done(ChatWatchRes { state: ChatWatchState::Complete });
    let outs = d.on_evt(&host(), ChatEvt::Watched { target: target(), reply }, at + Duration::from_secs(21));
    let snapshot: Vec<ChatRecord> = (200..220).map(top).collect();
    on(&mut d, &history(&outs), page(&snapshot, None), at + Duration::from_secs(22));
    d.layout(&mut fonts, body(), 1.0, body());
    assert_eq!(d.thread_len(), 156);
    assert!(joined(&d).contains("reply 154"), "{}", joined(&d));

    // The parent gave way: closing passes the mark to a held record by the view's rule.
    assert!(d.close_thread());
    d.layout(&mut fonts, body(), 1.0, body());
    let mark = d.marked().expect("a mark");
    assert!(mark != parent && d.placed_text().len() > 1, "the mark passed on");

    // The thread's share goes back to the channel (the reader follows the newest, so no anchor holds the trim): it grows past 100 again.
    d.latest();
    d.layout(&mut fonts, body(), 1.0, body());
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
    d.layout(&mut fonts, body(), 1.0, body());
    d.open_thread();
    assert_eq!((d.title(), d.placed_text().len()), (" chat · CH1 ".to_string(), 3));

    // The marked message's thread shows its parent alone; opening another thread meanwhile is nothing.
    open(&mut d, &mut fonts, &[true, true]);
    d.layout(&mut fonts, body(), 1.0, body());
    assert_eq!(d.title(), " chat · CH1 · thread ");
    assert!(d.placed_text().len() == 1 && d.placed_text()[0].contains("message 1"), "{:?}", d.placed_text());
    d.mark(true, Instant::now(), Duration::ZERO);
    d.layout(&mut fonts, body(), 1.0, body());
    d.open_thread();
    assert!(d.placed_text()[0].contains("message 1"), "{:?}", d.placed_text());

    // A link's end keeps it open.
    d.on_connection(&host(), false, false, at);
    assert_eq!(d.title(), " chat · CH1 · thread no connection to a ", "the link's words stay over a thread");
    d.on_connection(&host(), true, false, at);
    assert!(d.title().starts_with(" chat · CH1 · thread "));

    // Escape closes it: the channel's view is back, with its mark on the parent.
    assert!(d.close_thread());
    assert!(!d.close_thread(), "nothing to close");
    d.layout(&mut fonts, body(), 1.0, body());
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
    d.layout(&mut fonts, body(), 1.0, body());
    d.mark(true, Instant::now(), Duration::ZERO);
    d.layout(&mut fonts, body(), 1.0, body());
    d.scroll(true, Step::Page, Instant::now(), Duration::ZERO);
    d.layout(&mut fonts, body(), 1.0, body());
    assert!(!d.following());
    d.open_thread();
    d.layout(&mut fonts, body(), 1.0, body());
    assert!(d.following(), "the thread has a view of its own");
    d.close_thread();
    d.layout(&mut fonts, body(), 1.0, body());
    assert!(!d.following(), "the channel's position returns");

    // The parent gave way under a resync while its thread was open: closing passes the mark to the nearest newer message.
    let (mut d, at) = channel(&held);
    open(&mut d, &mut fonts, &[true, true]);
    resnapshot(&mut d, &[held[0].clone(), held[2].clone()], None, at);
    d.close_thread();
    d.layout(&mut fonts, body(), 1.0, body());
    assert_eq!((d.marked(), d.placed_text().len()), (Some(held[2].ts.clone()), 2));

    // `no messages` is the channel's: not over a thread whose channel was emptied meanwhile.
    let (mut d, at) = channel(&held);
    open(&mut d, &mut fonts, &[true]);
    resnapshot(&mut d, &[], None, at);
    assert!(d.body().is_none(), "the thread is shown");
    d.close_thread();
    assert!(d.body().is_some_and(|b| b.lines(40, 5) == ["no messages"]));

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
    d.layout(&mut fonts, body(), 1.0, body());
    assert!(d.placed_text()[0].ends_with("(message deleted)"), "{:?}", d.placed_text());

    // The thread's view never moves the channel's reading place: the owner holds no anchor (the channel's oldest give way) and asks for no page.
    let (mut d, at) = channel_with(&(0..20).map(top).collect::<Vec<_>>(), Some("older"));
    d.layout(&mut fonts, body(), 1.0, body());
    d.mark(true, Instant::now(), Duration::ZERO);
    d.layout(&mut fonts, body(), 1.0, body());
    d.scroll(true, Step::Page, Instant::now(), Duration::ZERO);
    d.layout(&mut fonts, body(), 1.0, body());
    assert!(d.owner_anchor().is_some(), "scrolled up");
    d.open_thread();
    d.layout(&mut fonts, body(), 1.0, body());
    assert_eq!(d.owner_anchor(), None, "the thread's frames set no anchor");
    assert!(unasked(&d.tick(shown("ROW1"), at + Duration::from_secs(1)).0), "no older page is asked for");

    // A reader at the oldest record when the thread opens: no channel page is asked for while it is open.
    let (mut d, at) = channel_with(&held, Some("older"));
    open(&mut d, &mut fonts, &[true]);
    for s in 1..16 {
        assert!(unasked(&d.tick(shown("ROW1"), at + Duration::from_secs(s)).0), "tick {s}");
    }
    d.close_thread();
    d.layout(&mut fonts, body(), 1.0, body());
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
    d.on_connection(&host(), false, false, at);
    let outs = d.on_connection(&host(), true, false, at);
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
    d.on_connection(&host(), false, false, at);
    assert_eq!(d.title(), format!(" chat · CH1 · thread the thread is not read: {} · no connection to a ", ChatCode::Busy.text()));
    d.close_thread();
    let (mut d, at) = channel(&held);
    let req = history(&open(&mut d, &mut fonts, &[true]));
    on(&mut d, &req, ChatReply::Unsupported, at);
    assert_eq!(d.title(), " chat · CH1 · thread Ship of Tools on the computer that runs your agents has no chat: update it ");
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
    assert!(d.on_connection(&other, false, false, at).is_empty());
    assert_eq!(reads(&d.on_connection(&other, true, false, at)), 0, "no thread read goes out");
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
    d.on_connection(&host(), false, false, at);
    d.on_connection(&host(), true, false, at);
    assert!(!d.title().contains("not read"), "{}", d.title());
    assert_eq!(reads(&d.tick(shown("ROW1"), at + Duration::from_secs(10)).0), 0, "the old read is not asked again");
}

/// Types `text` into ROW1's draft and presses Enter: the one post that goes out.
fn type_and_send(d: &mut ChatDrawer, text: &str, at: Instant) -> ChatPostReq {
    d.edit(shown("ROW1").as_ref(), Edit::Text(text)).unwrap();
    send(d, at)
}

fn send(d: &mut ChatDrawer, at: Instant) -> ChatPostReq {
    match d.send(shown("ROW1").as_ref(), Some("Ada"), at).unwrap().as_slice() {
        [(_, OutgoingReq::ChatPost { req })] => req.clone(),
        other => panic!("one chat.post expected, got {} requests", other.len()),
    }
}

fn made(d: &mut ChatDrawer, req: &ChatPostReq, at: Instant) -> Vec<Out> {
    let ts = serde_json::from_value(serde_json::json!("1900000000.000100")).unwrap();
    d.on_evt(&host(), ChatEvt::Posted { req: req.clone(), reply: ChatReply::Done(ChatPostRes::Posted { ts }) }, at)
}

/// Three days after the parent `top(1)` (22:13 UTC on the 14th), in UTC, so the compose line's stamp is the same on every day
/// the test runs.
const PINNED_NOW: i64 = 1_700_200_000;
const PARENT_STAMP: &str = "reply in thread of Nov 14 10:13 PM";

fn foot(d: &ChatDrawer) -> Vec<String> {
    compose_rows(&d.compose_text_at(false, PINNED_NOW, chrono::FixedOffset::east_opt(0).unwrap()).unwrap(), 60, 6)
}

/// The foot's last row: the thread's line, or the draft's own last row when nothing is said of where it goes.
fn to_line(d: &ChatDrawer) -> String {
    foot(d).pop().unwrap()
}

/// What the foot says of where a draft goes, for each kind of draft and each view of the drawer: silent only when the
/// destination is what the title shows (no thread in the draft, none open, the draft's channel the watched one).
#[test]
fn the_foot_is_silent_only_when_the_title_shows_the_drafts_destination() {
    #[derive(Clone, Copy, Debug)]
    enum Draft {
        None,
        Channel,
        Thread,
    }
    #[derive(Clone, Copy, Debug)]
    enum View {
        Channel,
        SameThread,
        OtherThread,
    }
    let held: Vec<ChatRecord> = (0..3).map(top).collect();
    let mut fonts = FontSystem::new();
    // `remap`: the row is answered with another channel after the draft is begun, which closes any thread, so only the
    // channel view is left to look at.
    for draft in [Draft::None, Draft::Channel, Draft::Thread] {
        for (view, remap) in [(View::Channel, false), (View::SameThread, false), (View::OtherThread, false), (View::Channel, true)] {
            let (mut d, at) = channel(&held);
            let row = shown("ROW1");
            match draft {
                Draft::None => {}
                Draft::Channel => d.edit(row.as_ref(), Edit::Text("hi")).unwrap(),
                Draft::Thread => {
                    open(&mut d, &mut fonts, &[true, true]);
                    d.edit(row.as_ref(), Edit::Text("hi")).unwrap();
                    if !matches!(view, View::SameThread) {
                        d.close_thread();
                    }
                }
            }
            match view {
                View::Channel => {}
                View::SameThread if matches!(draft, Draft::Thread) => {}
                View::SameThread => drop(open(&mut d, &mut fonts, &[true, true])),
                View::OtherThread => drop(open(&mut d, &mut fonts, &[true])),
            }
            if remap {
                let other = serde_json::json!({ "state": "mapped", "workspace": "WS1", "channel": "CH2", "delivers": false });
                d.on_evt(&host(), answer("ROW1", other), at + Duration::from_secs(1));
            }
            let rows = foot(&d);
            // Nothing typed and nothing to say of the destination: the foot says what to type and names the chord instead.
            if matches!((draft, view), (Draft::None, View::Channel)) {
                assert!(rows[0].starts_with("> ") && rows[0].contains("type a message") && rows.join(" ").contains("Shift+k changes this project's channel"), "{rows:?}");
                continue;
            }
            let text = if matches!(draft, Draft::None) { "> " } else { "> hi" };
            let (head, tail) = rows.split_first().unwrap();
            assert_eq!(head, text, "{draft:?} in {view:?}");
            let said = tail.first().map(String::as_str);
            let case = format!("{draft:?} in {view:?}, remapped {remap}: {rows:?}");
            let thread_line = |s: &str| s.starts_with("reply in thread of") && s.ends_with(" in CH1") == remap;
            match (draft, view, remap) {
                (Draft::None, View::Channel, _) => unreachable!("{case}: said above"),
                (Draft::Channel, View::Channel, false) => assert_eq!(said, None, "{case}"),
                (Draft::Channel, _, _) => assert_eq!(said, Some("to CH1"), "{case}"),
                (Draft::None | Draft::Thread, _, _) => assert!(said.is_some_and(thread_line), "{case}"),
            }
            assert!(tail.len() <= 1, "{case}");
        }
    }
}

/// A channel the window has a name for is spelled in the foot as the title spells it.
#[test]
fn the_foot_spells_the_channel_as_the_title_does() {
    let mut fonts = FontSystem::new();
    let held: Vec<ChatRecord> = (0..3).map(top).collect();
    let (mut d, at) = channel(&held);
    let named = serde_json::json!({ "state": "mapped", "workspace": "WS1", "channel": "CH1", "delivers": false, "name": "general" });
    d.on_evt(&host(), answer("ROW1", named), at);
    d.edit(shown("ROW1").as_ref(), Edit::Text("hi")).unwrap();
    open(&mut d, &mut fonts, &[true, true]);
    assert_eq!((d.title(), foot(&d)), (" chat · #general · thread ".to_string(), vec!["> hi".to_string(), "to #general".to_string()]));
}

#[test]
fn a_draft_posts_to_the_destination_it_was_begun_in() {
    let mut fonts = FontSystem::new();
    let held: Vec<ChatRecord> = (0..3).map(top).collect();
    let parent = held[1].ts.clone();

    // Begun in the thread, then the thread closed and the link lost and back: one post, to that thread.
    let (mut d, at) = channel(&held);
    open(&mut d, &mut fonts, &[true, true]);
    d.edit(shown("ROW1").as_ref(), Edit::Text("hi")).unwrap();
    assert!(to_line(&d).starts_with(PARENT_STAMP), "{}", to_line(&d));
    d.close_thread();
    assert!(to_line(&d).starts_with(PARENT_STAMP), "the draft keeps its thread through the close");
    // A row switch away and back, and the watch ended, leave it as it was.
    d.tick(shown("ROW2"), at + Duration::from_secs(1));
    d.tick(shown("ROW1"), at + Duration::from_secs(2));
    d.on_evt(&host(), ChatEvt::Resolved { row: "ROW1".into(), reply: ChatReply::Refused(ChatCode::Unmapped) }, at + Duration::from_secs(3));
    assert!(to_line(&d).starts_with(PARENT_STAMP), "the draft keeps its thread through a row switch and a lost watch");
    d.on_connection(&host(), false, false, at);
    d.on_connection(&host(), true, false, at);
    let req = send(&mut d, at);
    assert_eq!((req.thread.as_ref(), req.target.channel.as_str(), req.text.as_str()), (Some(&parent), "CH1", "hi"));
    assert!(d.send(shown("ROW1").as_ref(), Some("Ada"), at).is_err(), "the post waits: no second one");
    assert_eq!(reads(&made(&mut d, &req, at)), 0, "the thread is closed: nothing is read");
    assert!(d.draft().is_none());

    // Begun in the channel, then a thread opened: the post is the channel's, and the foot says so.
    let (mut d, at) = channel(&held);
    d.edit(shown("ROW1").as_ref(), Edit::Text("hi")).unwrap();
    open(&mut d, &mut fonts, &[true, true]);
    d.edit(shown("ROW1").as_ref(), Edit::Text("!")).unwrap();
    assert_eq!(foot(&d), ["> hi!", "to CH1"], "the title says `thread`, so the foot says where the draft goes: the channel");
    let req = send(&mut d, at);
    assert_eq!((req.thread.as_ref(), req.target.channel.as_str(), req.text.as_str()), (None, "CH1", "hi!"));
    assert_eq!(reads(&made(&mut d, &req, at)), 0, "a post to the channel reads no thread");

    // The thread is read again after the person's own post in it, whole, from its first page.
    let (mut d, at) = channel(&held);
    let outs = open(&mut d, &mut fonts, &[true, true]);
    on(&mut d, &history(&outs), page(&[reply_to(&parent, 1)], None), at);
    d.layout(&mut fonts, body(), 1.0, body());
    assert_eq!(d.thread_len(), 2);
    let req = type_and_send(&mut d, "a reply", at);
    let again = history(&made(&mut d, &req, at));
    assert_eq!((again.thread.as_ref(), again.cursor.as_ref()), (Some(&parent), None));
    assert_eq!(d.thread_len(), 1, "the walk starts over from the parent");

    // Not after a post into another thread: the draft begun in the first, then the second opened.
    let (mut d, at) = channel(&held);
    open(&mut d, &mut fonts, &[true, true]);
    d.edit(shown("ROW1").as_ref(), Edit::Text("to the first")).unwrap();
    d.close_thread();
    let outs = open(&mut d, &mut fonts, &[false]);
    on(&mut d, &history(&outs), page(&[], None), at);
    let req = send(&mut d, at);
    assert_eq!(req.thread.as_ref(), Some(&parent));
    assert_eq!(reads(&made(&mut d, &req, at)), 0, "another thread is open: it is not read again");
}

/// A thread's posts come back in any order: the reread after `posted` is asked once the first page in flight has left.
#[test]
fn a_post_while_the_first_page_is_in_flight_is_read_again_once_it_is_answered() {
    let mut fonts = FontSystem::new();
    let held: Vec<ChatRecord> = (0..3).map(top).collect();
    let parent = held[1].ts.clone();
    let (mut d, at) = channel(&held);
    let first = history(&open(&mut d, &mut fonts, &[true, true]));
    let req = type_and_send(&mut d, "a reply", at);
    assert_eq!(reads(&made(&mut d, &req, at)), 0, "the equal read is still in flight: the reread waits behind it");
    let outs = on(&mut d, &first, page(&[], None), at);
    let again = history(&outs);
    assert_eq!(again, first, "the first page is asked for again");
    on(&mut d, &again, page(&[reply_to(&parent, 1)], None), at);
    assert_eq!(d.thread_len(), 2, "the thread holds the reply the post made");
}

/// Only a `posted` reply reads the thread again; the line under an empty draft names the thread.
#[test]
fn only_a_posted_reply_rereads_and_an_empty_draft_names_the_open_thread() {
    let mut fonts = FontSystem::new();
    let held: Vec<ChatRecord> = (0..3).map(top).collect();
    let parent = held[1].ts.clone();
    let (mut d, at) = channel(&held);
    assert!(foot(&d)[0].starts_with("> type a message"), "an empty draft for the title's channel has the prompt and its hint");
    let outs = open(&mut d, &mut fonts, &[true, true]);
    assert!(to_line(&d).starts_with(PARENT_STAMP), "{}", to_line(&d));
    on(&mut d, &history(&outs), page(&[reply_to(&parent, 1)], None), at);
    let req = type_and_send(&mut d, "a reply", at);
    let refused = ChatReply::Done(ChatPostRes::Refused { code: ChatCode::Busy });
    let outs = d.on_evt(&host(), ChatEvt::Posted { req, reply: refused }, at);
    assert_eq!((reads(&outs), d.thread_len()), (0, 2), "a refused post reads nothing");
}

/// The reread is by the Slack thread's identity: its host, workspace, channel and parent; the row that posted is not part of it.
#[test]
fn a_thread_is_the_one_posted_to_by_host_workspace_channel_and_parent() {
    let parent = top(1);
    let t = super::thread::Thread::new(host(), target(), parent.clone(), false, Default::default());
    let at = |v: serde_json::Value| -> ChatTarget { serde_json::from_value(v).unwrap() };
    let mk = |row: &str, ws: &str, ch: &str| at(serde_json::json!({ "row": row, "workspace": ws, "channel": ch }));
    assert!(t.is(&host(), &mk("ROW2", "WS1", "CH1"), &parent.ts), "another row's post to the same thread");
    assert!(!t.is(&"b".to_string(), &mk("ROW1", "WS1", "CH1"), &parent.ts));
    assert!(!t.is(&host(), &mk("ROW1", "WS2", "CH1"), &parent.ts));
    assert!(!t.is(&host(), &mk("ROW1", "WS1", "CH2"), &parent.ts));
    assert!(!t.is(&host(), &mk("ROW1", "WS1", "CH1"), &top(2).ts));
}

#[test]
fn a_selection_is_not_parked_with_the_channels_view_when_a_thread_opens() {
    let mut fonts = FontSystem::new();
    let held: Vec<ChatRecord> = (0..3).map(top).collect();
    let (mut d, _) = channel(&held);
    d.layout(&mut fonts, body(), 1.0, body());
    let hit = d.hit(d.point_at(&held[0].ts, 2, 0).unwrap());
    assert!(d.press(hit));
    let end = d.point_at(&held[0].ts, 2, 4).unwrap();
    d.extend_drag(end);
    let tinted = |d: &mut ChatDrawer, fonts: &mut FontSystem| d.layout(fonts, body(), 1.0, body()).iter().any(|q| q.color == (0x1B, 0x43, 0x56));
    assert!(tinted(&mut d, &mut fonts), "record 0 is selected");
    // Mark another message and open its thread: a press elsewhere (an agent click) while it is open, then close.
    open(&mut d, &mut fonts, &[true]);
    d.press(None);
    assert!(d.close_thread());
    assert!(!tinted(&mut d, &mut fonts), "the old selection does not return at close");
    assert_eq!(d.copy_text().as_deref(), Some("message 2"), "Primary+c copies the marked parent, not the old selection");
}

/// Nothing but the row's resolve for a name (`RowChannel` asks again after an answer that named nothing) went out.
fn unasked(outs: &[Out]) -> bool {
    outs.iter().all(|(_, r)| matches!(r, OutgoingReq::ChatResolve { .. }))
}

/// A thread's messages have the channel's rules: none above the parent or at a reply that starts a day, one above each other
/// reply.
#[test]
fn a_threads_messages_have_the_channels_rules() {
    const RULE: (u8, u8, u8) = (0x2A, 0x2D, 0x31);
    let mut fonts = FontSystem::new();
    let held: Vec<ChatRecord> = (0..3).map(top).collect();
    let (mut d, at) = channel(&held);
    let parent = held[2].ts.clone();
    let first = history(&open(&mut d, &mut fonts, &[true]));
    let rules = |d: &mut ChatDrawer, fonts: &mut FontSystem| d.layout(fonts, body(), 1.0, body()).iter().filter(|q| q.color == RULE).count();
    assert_eq!(rules(&mut d, &mut fonts), 0, "the parent alone has none");
    // The replies come a day after the parent: the first starts a day, and the other two are its group.
    let rs: Vec<ChatRecord> = (0..3).map(|n| reply_to(&parent, n)).collect();
    assert!(on(&mut d, &first, page(&rs, None), at + Duration::from_secs(1)).is_empty());
    assert_eq!(rules(&mut d, &mut fonts), 2, "{}", joined(&d));
    assert_eq!(d.placed_text().len(), 4, "the parent and three replies: {}", joined(&d));
}

/// A reply to a post of either kind, as the daemon would answer it.
#[derive(Clone, Copy, Debug)]
enum Answer {
    Posted,
    Refused(ChatCode),
    Limited,
    Unknown,
    Unsupported,
}

/// The one post `outs` holds, text or file, answered `answer`.
fn answer_post(d: &mut ChatDrawer, outs: &[Out], answer: Answer, at: Instant) -> Vec<Out> {
    let ts: Ts = serde_json::from_value(serde_json::json!("1900000000.000100")).unwrap();
    let evt = match (outs, answer) {
        ([(_, OutgoingReq::ChatPost { req })], a) => ChatEvt::Posted { req: req.clone(), reply: match a {
            Answer::Posted => ChatReply::Done(ChatPostRes::Posted { ts }),
            Answer::Refused(code) => ChatReply::Done(ChatPostRes::Refused { code }),
            Answer::Limited => ChatReply::Done(ChatPostRes::Limited { retry_after_secs: 30, assumed: false }),
            Answer::Unknown => ChatReply::Done(ChatPostRes::Unknown),
            Answer::Unsupported => ChatReply::Unsupported,
        } },
        ([(_, OutgoingReq::ChatPostFile { req })], a) => ChatEvt::FilePosted { req: req.clone(), reply: match a {
            Answer::Posted => ChatReply::Done(ChatPostFileRes::Posted { file: "F0FILE".into(), ts: None }),
            Answer::Refused(code) => ChatReply::Done(ChatPostFileRes::Refused { code }),
            Answer::Limited => ChatReply::Done(ChatPostFileRes::Limited { retry_after_secs: 30, assumed: false }),
            Answer::Unknown => ChatReply::Done(ChatPostFileRes::Unknown),
            Answer::Unsupported => ChatReply::Unsupported,
        } },
        (other, _) => panic!("one post expected, got {} requests", other.len()),
    };
    d.on_evt(&host(), evt, at)
}

/// One outcome table for both kinds of post, a thread open and the draft begun in it: only `posted` clears the draft and
/// reads the thread again; `no_user_token` opens the Connect step; every other outcome keeps the draft and says why; a file's
/// share says what went where.
#[test]
fn a_post_of_either_kind_settles_by_one_table() {
    let mut fonts = FontSystem::new();
    let held: Vec<ChatRecord> = (0..3).map(top).collect();
    let busy = format!("not posted: {}", ChatCode::Busy.text());
    let unavailable = format!("not posted: {}", ChatCode::Unavailable.text());
    for file in [false, true] {
        for answer in [Answer::Refused(ChatCode::Busy), Answer::Refused(ChatCode::NoUserToken), Answer::Limited, Answer::Unknown, Answer::Unsupported, Answer::Posted] {
            let (mut d, at) = channel_on(&held, None, true);
            let outs = open(&mut d, &mut fonts, &[true, true]);
            on(&mut d, &history(&outs), page(&[], None), at);
            d.edit(shown("ROW1").as_ref(), Edit::Text("hi")).unwrap();
            let sent = if file { d.send_file(shown("ROW1").as_ref(), Some("files:figs/fit.png"), at) } else { d.send(shown("ROW1").as_ref(), Some("tester"), at) };
            let outs = answer_post(&mut d, &sent.unwrap(), answer, at);
            let case = format!("file {file}, {answer:?}");
            let notice = d.take_notice();
            match answer {
                Answer::Posted => {
                    assert!(d.draft().is_none(), "{case}");
                    assert_eq!(history(&outs).thread.as_ref(), Some(&held[1].ts), "{case}: the thread is read again");
                    assert_eq!(notice.as_deref(), file.then_some("shared fit.png to CH1"), "{case}");
                }
                other => {
                    assert_eq!(d.draft(), Some(("hi", "CH1")), "{case}: kept");
                    assert_eq!(reads(&outs), 0, "{case}");
                    assert_eq!(d.in_setup(), matches!(other, Answer::Refused(ChatCode::NoUserToken)), "{case}");
                    let words = notice.unwrap_or_default();
                    match other {
                        Answer::Refused(ChatCode::Busy) => assert_eq!(words, busy, "{case}"),
                        Answer::Limited => assert!(words.contains("wait 30 s"), "{case}: {words}"),
                        Answer::Unknown => assert!(words.starts_with("may have been posted"), "{case}: {words}"),
                        Answer::Unsupported => assert_eq!(words, unavailable, "{case}"),
                        _ => {}
                    }
                }
            }
        }
    }
}

/// With no draft, the previewed file goes into the open thread; a blank draft is no comment.
#[test]
fn a_file_with_no_draft_goes_to_the_open_thread_and_a_blank_draft_is_no_comment() {
    let mut fonts = FontSystem::new();
    let held: Vec<ChatRecord> = (0..3).map(top).collect();
    let (mut d, at) = channel_on(&held, None, true);
    let outs = open(&mut d, &mut fonts, &[true, true]);
    on(&mut d, &history(&outs), page(&[], None), at);
    let req = match d.send_file(shown("ROW1").as_ref(), Some("files:a.png"), at).unwrap().as_slice() {
        [(_, OutgoingReq::ChatPostFile { req })] => req.clone(),
        other => panic!("one chat.post_file expected, got {} requests", other.len()),
    };
    assert_eq!((req.thread.as_ref(), req.comment.as_deref()), (Some(&held[1].ts), None));

    let (mut d, at) = channel_on(&held, None, true);
    d.edit(shown("ROW1").as_ref(), Edit::Text("  ")).unwrap();
    d.edit(shown("ROW1").as_ref(), Edit::Newline).unwrap();
    let req = match d.send_file(shown("ROW1").as_ref(), Some("files:a.png"), at).unwrap().as_slice() {
        [(_, OutgoingReq::ChatPostFile { req })] => req.clone(),
        other => panic!("one chat.post_file expected, got {} requests", other.len()),
    };
    assert_eq!((req.target.channel.as_str(), req.comment), ("CH1", None), "a blank draft is no comment");
}
