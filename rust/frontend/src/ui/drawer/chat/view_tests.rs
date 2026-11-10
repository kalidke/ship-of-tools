use super::owner::{ChatOwner, Out};
use super::project::{project, Kind};
use super::view::*;
use crate::ui::render::text::EXTRA_TOP_PAD_PX;
use crate::ui::render::quad::ScreenRect;
use cosmic_text::FontSystem;
use crate::net::transport::{ChatEvt, ChatReply, OutgoingReq};
use sot_protocol::chat::ChatRecord;
use sot_protocol::ops::{ChatWatchRes, ChatWatchState};
use std::time::Instant;

fn rec_at(ts: &str, text: &str) -> ChatRecord {
    serde_json::from_value(serde_json::json!({ "workspace": "T1", "channel": "C1", "ts": ts, "thread_ts": ts,
        "author": { "id": "U1", "name": "Ada" }, "origin": "member", "text": text, "reply_count": 0, "files": [] })).unwrap()
}

fn rec(n: usize) -> ChatRecord {
    rec_at(&format!("1700000{n:03}.000100"), &format!("message {n}"))
}

fn target() -> sot_protocol::ops::ChatTarget {
    serde_json::from_value(serde_json::json!({ "row": "repo__box", "workspace": "T1", "channel": "C1" })).unwrap()
}

fn watching(now: Instant) -> ChatOwner {
    let mut o = ChatOwner::default();
    o.on_connection(&"h".to_string(), true, now);
    o.watch("h".to_string(), target(), now);
    o
}

/// An owner whose watch answered `complete` and whose snapshot read holds `held`.
fn complete_with(now: Instant, held: &[ChatRecord]) -> ChatOwner {
    let mut o = watching(now);
    let reply = ChatReply::Done(ChatWatchRes { state: ChatWatchState::Complete });
    let outs: Vec<Out> = o.on_evt(&"h".to_string(), ChatEvt::Watched { target: target(), reply }, now);
    let [(_, OutgoingReq::ChatHistory { req })] = &outs[..] else { panic!("one read expected") };
    let page = serde_json::from_value(serde_json::json!({ "records": held, "next": null })).unwrap();
    o.on_evt(&"h".to_string(), ChatEvt::History { req: req.clone(), reply: ChatReply::Done(page) }, now);
    o
}

fn tops(p: &[Placed]) -> Vec<(usize, f32)> {
    p.iter().map(|p| (p.index, p.top)).collect()
}

#[test]
fn the_view_shows_the_held_records_newest_at_the_bottom_and_shapes_only_what_it_shows() {
    // Three records of known heights end at the body's bottom, in order.
    let heights = [30.0, 40.0, 50.0];
    let three = place(3, 200.0, None, |i| heights[i]);
    assert_eq!(tops(&three), [(0, 80.0), (1, 110.0), (2, 150.0)]);
    // An arrival takes the bottom and lifts the rest.
    let heights = [30.0, 40.0, 50.0, 20.0];
    let four = place(4, 200.0, None, |i| heights[i]);
    assert_eq!(tops(&four), [(0, 60.0), (1, 90.0), (2, 130.0), (3, 180.0)]);
    // From a position, records run down from the top and stop at the body's bottom.
    assert_eq!(tops(&place(4, 100.0, Some(1), |i| heights[i])), [(1, 0.0), (2, 40.0), (3, 90.0)]);
    // With 256 records, only those reaching the body are measured.
    let mut asked = Vec::new();
    let many = place(256, 100.0, None, |i| {
        asked.push(i);
        30.0
    });
    assert_eq!(asked, [255, 254, 253, 252]);
    assert_eq!(tops(&many), [(252, -20.0), (253, 10.0), (254, 40.0), (255, 70.0)]);

    // Through real buffers: only the records in view are shaped, and a scroll keeps up to 64 others.
    let mut fonts = FontSystem::new();
    let held: Vec<ChatRecord> = (0..256).map(rec).collect();
    let records: Vec<&ChatRecord> = held.iter().collect();
    let body = ScreenRect { x: 10.0, y: 20.0, w: 400.0, h: 120.0 };
    let offset = chrono::FixedOffset::east_opt(0).unwrap();
    let mut view = ChatView::default();
    view.layout(&mut fonts, &records, body, 1.0, 1_700_000_000, offset);
    let shown = view.areas().len();
    assert!((1..=8).contains(&shown), "{shown}");
    assert_eq!(view.cached(), shown);
    // A crossing record is clipped at the body's top and bottom, and its glyphs sit at its place.
    let areas = view.areas();
    assert!(areas.iter().all(|a| a.clip_top == Some(body.y) && a.clip_left == Some(body.x) && a.bottom == body.y + body.h && a.right == body.x + body.w));
    assert!(areas.iter().any(|a| a.y + EXTRA_TOP_PAD_PX < body.y), "one record crosses the top");
    // Scrolling through the history keeps up to 64 others, the nearest to the position: the last positions' records stay and
    // the first positions' are gone.
    let spans = |i: usize| project(records[i], 1_700_000_000, offset, false);
    for from in (0..=240).step_by(8) {
        view.from = Some(from);
        view.layout(&mut fonts, &records, body, 1.0, 1_700_000_000, offset);
        assert!(view.cached() <= view.areas().len() + 64, "{from}: {}", view.cached());
    }
    assert!(view.cached() >= 60, "the spare buffers are kept: {}", view.cached());
    assert!(view.holds(&spans(240)) && view.holds(&spans(232)), "the nearest are kept");
    assert!(!view.holds(&spans(0)) && !view.holds(&spans(8)) && !view.holds(&spans(16)), "the farthest are gone");
    view.layout(&mut fonts, &records, ScreenRect { w: 200.0, ..body }, 1.0, 1_700_000_000, offset);
    assert!(view.cached() <= view.areas().len() + 64);
    view.hide();
    assert!(view.areas().is_empty());

    // Each kind has its own style; a body line that imitates a header keeps the body's.
    let kinds = [Kind::Header(false), Kind::Header(true), Kind::Origin, Kind::Body, Kind::Markup, Kind::Meta];
    for (i, a) in kinds.iter().enumerate() {
        for b in &kinds[i + 1..] {
            assert_ne!(attrs(*a), attrs(*b), "{a:?} {b:?}");
        }
    }
    let spans = project(&rec(1), 1_700_000_000, offset, false);
    assert_eq!(spans[0].kind, Kind::Header(false));
}

#[test]
fn the_view_shows_records_oldest_to_newest_from_an_owner_whose_records_arrive_out_of_order() {
    let now = Instant::now();
    let mut o = watching(now);
    // Different second-lengths, so a string order would put "10" before "9".
    let stamps = ["100.000002", "9.000001", "1700000000.000001", "100.000001", "10.000001"];
    let put: Vec<_> = stamps.iter().map(|t| rec_at(t, &format!("at {t}"))).collect();
    let changed = serde_json::json!({ "workspace": "T1", "channel": "C1", "put": put, "deleted": [] });
    o.on_evt(&"h".to_string(), ChatEvt::Changed(serde_json::from_value(changed).unwrap()), now);
    let mut fonts = FontSystem::new();
    let mut view = ChatView::default();
    let body = ScreenRect { x: 0.0, y: 0.0, w: 600.0, h: 2000.0 };
    view.layout(&mut fonts, &o.records(), body, 1.0, 1_700_000_100, chrono::FixedOffset::east_opt(0).unwrap());
    let shown = view.placed_text();
    let order = ["9.000001", "10.000001", "100.000001", "100.000002", "1700000000.000001"];
    assert_eq!(shown.len(), 5);
    for (text, ts) in shown.iter().zip(order) {
        assert!(text.ends_with(&format!("at {ts}")), "{text:?} should be {ts}");
    }
}

#[test]
fn no_messages_shows_only_for_a_channel_read_whole_and_empty() {
    let now = Instant::now();
    assert!(!ChatOwner::default().no_messages(), "nothing watched");
    assert!(!watching(now).no_messages(), "the snapshot is not read");
    assert!(complete_with(now, &[]).no_messages());
    assert!(!complete_with(now, &[rec(1)]).no_messages(), "a record is held");
    let mut offline = complete_with(now, &[]);
    offline.on_connection(&"h".to_string(), false, now);
    assert!(!offline.no_messages(), "after the link went, the snapshot is owed");
}
