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
    let page = serde_json::from_value(serde_json::json!({ "records": held, "next": null })).unwrap();
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
    d.on_connection(&host(), true, at);
    assert_eq!(d.title(), " chat · CH1 · thread ");

    // Escape closes it: the channel's view is back, with its mark on the parent.
    assert!(d.close_thread());
    assert!(!d.close_thread(), "nothing to close");
    d.layout(&mut fonts, body(), 1.0);
    assert_eq!((d.marked(), d.placed_text().len()), (Some(parent), 3));
    assert!(!d.title().contains("thread"));

    // A row switch closes it, and so does hiding the drawer.
    d.open_thread();
    assert!(d.title().ends_with("thread "));
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
}
