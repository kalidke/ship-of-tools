use std::time::{Duration, Instant};

use cosmic_text::FontSystem;
use sot_protocol::chat::ChatRecord;
use sot_protocol::ops::{ChatHistoryReq, ChatTarget, ChatWatchRes, ChatWatchState};

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
    let at = format!("1700000{n:03}.000100");
    serde_json::from_value(serde_json::json!({ "workspace": "WS1", "channel": "CH1", "ts": at, "thread_ts": at,
        "author": { "id": "U1", "name": "Ada" }, "origin": "member", "text": format!("message {n}"), "reply_count": 0, "files": [] })).unwrap()
}

fn history(outs: &[Out]) -> ChatHistoryReq {
    match outs {
        [(_, OutgoingReq::ChatHistory { req })] => req.clone(),
        other => panic!("one history read expected, got {} requests", other.len()),
    }
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
    let page = serde_json::from_value(serde_json::json!({ "records": held, "next": next })).unwrap();
    d.on_evt(&host(), ChatEvt::History { req: history(&outs), reply: ChatReply::Done(page) }, at);
    (d, at)
}

fn body() -> ScreenRect {
    ScreenRect { x: 0.0, y: 0.0, w: 400.0, h: 400.0 }
}

/// Presses the mark keys (`true` toward older messages), as a frame spends them, and opens the marked message's thread.
fn open(d: &mut ChatDrawer, fonts: &mut FontSystem, keys: &[bool]) {
    d.layout(fonts, body(), 1.0);
    keys.iter().for_each(|older| d.mark(*older));
    d.layout(fonts, body(), 1.0);
    d.open_thread();
    d.layout(fonts, body(), 1.0);
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

    // The thread's view never moves the channel's reading place: the owner keeps the reader's anchor and asks for no page.
    let (mut d, at) = channel_with(&(0..20).map(top).collect::<Vec<_>>(), Some("older"));
    d.layout(&mut fonts, body(), 1.0);
    d.mark(true);
    d.layout(&mut fonts, body(), 1.0);
    d.scroll(true, Step::Page);
    d.layout(&mut fonts, body(), 1.0);
    let anchor = d.owner_anchor().expect("scrolled up");
    d.open_thread();
    d.layout(&mut fonts, body(), 1.0);
    assert_eq!(d.owner_anchor(), Some(anchor), "the channel's anchor is kept");
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
