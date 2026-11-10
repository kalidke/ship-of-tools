use super::owner::{ChatOwner, Out};
use super::project::{project, Kind};
use super::view::*;
use crate::ui::render::text::EXTRA_TOP_PAD_PX;
use crate::ui::render::quad::ScreenRect;
use cosmic_text::FontSystem;
use crate::net::transport::{ChatEvt, ChatReply, OutgoingReq};
use sot_protocol::chat::ChatRecord;
use super::drawer::ChatDrawer;
use super::row_channel_tests::{answer, mapped};
use crate::ui::ResultRowIdentity;
use crate::ui::input::help::Pane;
use crate::ui::preview::markdown::BODY_LINE_H;
use sot_protocol::ops::{ChatCode, ChatWatchRes, ChatWatchState};
use std::time::{Duration, Instant};

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
    assert_eq!(tops(&place(4, 100.0, Some((1, 0.0)), |i| heights[i])), [(1, 0.0), (2, 40.0), (3, 90.0)]);
    // A top below the body's top moves every record down with it.
    assert_eq!(tops(&place(4, 100.0, Some((1, 20.0)), |i| heights[i])), [(1, 20.0), (2, 60.0)]);
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
        view.anchor = Some((records[from].ts.clone(), 0.0));
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

fn refused(row: &str, code: ChatCode) -> ChatEvt {
    ChatEvt::Resolved { row: row.to_string(), reply: ChatReply::Refused(code) }
}

fn row_target() -> sot_protocol::ops::ChatTarget {
    serde_json::from_value(serde_json::json!({ "row": "R", "workspace": "T1", "channel": "C1" })).unwrap()
}

fn shown(row: &str) -> Option<ResultRowIdentity> {
    Some(ResultRowIdentity { host: "a".to_string(), workspace_id: row.to_string() })
}

/// Resolves `R` (shown, connected) at `t0`, delivers `evt`, and returns the drawer with the instant of the answer.
fn answered_with(evt: impl FnOnce(&str) -> ChatEvt) -> (ChatDrawer, Instant) {
    let t0 = Instant::now();
    let mut d = ChatDrawer::default();
    d.on_connection(&"a".to_string(), true, t0);
    d.tick(shown("R"), t0);
    assert_eq!(d.title(), " chat · reading… ");
    d.tick(shown("R"), t0 + Duration::from_millis(200));
    d.on_evt(&"a".to_string(), evt("R"), t0 + Duration::from_millis(210));
    (d, t0 + Duration::from_millis(210))
}

/// Whether a frame at `at` asks again, for the row `R` of host `a`.
fn asks_at(d: &mut ChatDrawer, at: Instant) -> bool {
    d.tick(shown("R"), at).0.iter().any(|(_, r)| matches!(r, OutgoingReq::ChatResolve { .. }))
}

/// A mapped row whose watch is refused with `code` after the daemon's window, and the instant of the refusal.
fn gapped(code: ChatCode) -> (ChatDrawer, Instant) {
    let (mut d, at) = answered_with(mapped);
    let reply = ChatReply::Refused(code);
    let at = at + Duration::from_secs(25);
    d.on_evt(&"a".to_string(), ChatEvt::Watched { target: row_target(), reply }, at);
    (d, at)
}

fn gap_title(code: ChatCode) -> String {
    gapped(code).0.title()
}

#[test]
fn every_outcome_has_a_state_a_next_action_and_words() {
    let pace = Duration::from_secs(60);
    let link = |d: &mut ChatDrawer, at: Instant| {
        d.on_connection(&"a".to_string(), false, at);
        d.on_connection(&"a".to_string(), true, at + Duration::from_millis(1));
        let asked = asks_at(d, at + Duration::from_millis(2));
        assert_eq!(d.title(), " chat · reading… ", "the old answer is forgotten");
        asked
    };

    // No listing for the row: it is read again every frame.
    let mut d = ChatDrawer::default();
    d.tick(None, Instant::now());
    assert_eq!(d.title(), " chat · waiting for this row's listing ");

    // The host is not connected: asked at `Connected`.
    let t0 = Instant::now();
    d.tick(shown("R"), t0);
    assert_eq!(d.title(), " chat · not connected ");
    d.on_connection(&"a".to_string(), true, t0);
    assert!(asks_at(&mut d, t0 + Duration::from_millis(200)));

    // Unmapped, no usable configuration, an old daemon and not the hub: asked again at a link change, never on a frame.
    for (evt, words) in [
        (answer("R", serde_json::json!({ "state": "unmapped" })), "the row's folder maps to no channel"),
        (answer("R", serde_json::json!({ "state": "something_new" })), "the hub has no usable chat configuration: its daemon log says why"),
        (ChatEvt::Resolved { row: "R".to_string(), reply: ChatReply::Unsupported }, "this daemon has no chat: update it"),
        (refused("R", ChatCode::Unavailable), "the chat runs on the hub, not on a"),
    ] {
        let (mut d, at) = answered_with(|_| evt);
        assert_eq!(d.title(), format!(" chat · {words} "));
        assert!(!asks_at(&mut d, at + pace * 5), "{words}: not on a frame");
        assert!(link(&mut d, at + pace * 6), "{words}: at a link change");
    }

    // A new row does not inherit the old row's answer, and a late answer for the old row changes nothing.
    let (mut d, at) = answered_with(|r| answer(r, serde_json::json!({ "state": "unmapped" })));
    d.tick(shown("S"), at + Duration::from_secs(1));
    assert_eq!(d.title(), " chat · reading… ");
    d.on_evt(&"a".to_string(), answer("R", serde_json::json!({ "state": "unmapped" })), at + Duration::from_secs(2));
    assert_eq!(d.title(), " chat · reading… ", "an answer for a row no longer shown is nothing");

    // A replaced row: the listing's new id is another row, and is asked.
    let (mut d, at) = answered_with(|r| refused(r, ChatCode::StaleRow));
    assert_eq!(d.title(), " chat · the row is gone or was replaced ");
    assert!(!asks_at(&mut d, at + pace * 5));
    d.tick(shown("R2"), at + pace * 6);
    assert!(d.tick(shown("R2"), at + pace * 6 + Duration::from_millis(200)).0.iter().any(|(_, r)| matches!(r, OutgoingReq::ChatResolve { .. })));

    // Any other refusal: asked after the pace, not before; once, with its answer cleared meanwhile.
    let (mut d, at) = answered_with(|r| refused(r, ChatCode::TooManyWatches));
    assert_eq!(d.title(), " chat · the hub could not answer: too many channels are watched ");
    assert!(!asks_at(&mut d, at + pace - Duration::from_secs(1)), "too soon");
    assert!(asks_at(&mut d, at + pace), "after the pace");
    assert!(!asks_at(&mut d, at + pace + Duration::from_millis(16)), "one resolve, not one a frame");
    assert_eq!(d.title(), " chat · reading… ");

    // Mapped: reading, then nothing after the channel once the snapshot is whole.
    let (mut d, at) = answered_with(mapped);
    assert_eq!(d.title(), " chat · C1 reading… ");
    let reply = ChatReply::Done(ChatWatchRes { state: ChatWatchState::Complete });
    let outs = d.on_evt(&"a".to_string(), ChatEvt::Watched { target: row_target(), reply }, at);
    let page = serde_json::from_value(serde_json::json!({ "records": [], "next": null })).unwrap();
    d.on_evt(&"a".to_string(), ChatEvt::History { req: history_req(&outs), reply: ChatReply::Done(page) }, at);
    assert_eq!(d.title(), " chat · C1 ");

    // A gap names its reason; a target refused for good shows its code's words, and is not asked again.
    assert_eq!(gap_title(ChatCode::Unavailable), " chat · C1 the hub's chat service is not running: its daemon log says why ");
    assert_eq!(gap_title(ChatCode::Busy), " chat · C1 not up to date: the chat service is busy ");
    for code in [ChatCode::StaleRow, ChatCode::StaleMapping, ChatCode::Unmapped] {
        let (mut d, at) = gapped(code);
        assert_eq!(d.title(), format!(" chat · C1 {} ", code.text()));
        assert!(d.tick(shown("R"), at + pace * 5).0.is_empty(), "{code:?}: not asked again");
        d.on_connection(&"a".to_string(), false, at + pace * 6);
        let outs = d.on_connection(&"a".to_string(), true, at + pace * 6 + Duration::from_millis(1));
        assert_eq!(outs.iter().filter(|(_, r)| matches!(r, OutgoingReq::ChatWatch { target } if target.channel.as_str() == "C1")).count(), 1, "{code:?}: asked again at a link change");
    }
    let (mut d, at) = answered_with(mapped);
    (0..40).for_each(|s| drop(d.tick(shown("R"), at + Duration::from_secs(s))));
    assert_eq!(d.title(), " chat · C1 not up to date: no snapshot ");
}

fn history_req(outs: &[Out]) -> sot_protocol::ops::ChatHistoryReq {
    match outs {
        [(_, OutgoingReq::ChatHistory { req })] => req.clone(),
        _ => panic!("one read expected"),
    }
}

/// Lays `held` out in a body `lines` records tall and a half (so a record crosses the top); the placed records' texts
/// (`message N` ends the text) and tops.
fn lay(view: &mut ChatView, fonts: &mut FontSystem, held: &[ChatRecord], h: f32) -> Vec<(String, f32)> {
    let records: Vec<&ChatRecord> = held.iter().collect();
    let body = ScreenRect { x: 0.0, y: 0.0, w: 400.0, h: 4.5 * h };
    view.layout(fonts, &records, body, 1.0, 1_700_000_100, chrono::FixedOffset::east_opt(0).unwrap());
    view.placed_text().into_iter().zip(view.placed_tops()).collect()
}

/// The first placed record's number and top.
fn top_of(shown: &[(String, f32)]) -> (String, f32) {
    let (text, top) = &shown[0];
    (text.rsplit("message ").next().unwrap().to_string(), *top)
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}

#[test]
fn an_anchored_reader_stays_put_through_arrivals_edits_and_deletes() {
    let mut fonts = FontSystem::new();
    let mut view = ChatView::default();
    let mut held: Vec<ChatRecord> = (0..20).map(rec).collect();
    // One record's height, from two records of a following view.
    let probe = lay(&mut view, &mut fonts, &held, 1000.0);
    let h = probe[1].1 - probe[0].1;
    let at = |view: &mut ChatView, fonts: &mut FontSystem, held: &[ChatRecord], n: &str, dy: f32| {
        let shown = lay(view, fonts, held, h);
        let (got, top) = top_of(&shown);
        assert!(got == n && near(top, dy), "wanted message {n} at {dy}, got message {got} at {top}");
    };
    // Following: the newest ends at the bottom, and a step down changes nothing.
    view.layout(&mut fonts, &held.iter().collect::<Vec<_>>(), ScreenRect { x: 0.0, y: 0.0, w: 400.0, h: 4.5 * h }, 1.0, 1_700_000_100, chrono::FixedOffset::east_opt(0).unwrap());
    view.scroll(false, Step::Page);
    let shown = lay(&mut view, &mut fonts, &held, h);
    assert!(view.anchor.is_none() && near(shown.last().unwrap().1, 3.5 * h), "{shown:?}");
    // A page up moves a third of the body: the reader is now at message 14, 5h above the follow position.
    view.scroll(true, Step::Page);
    at(&mut view, &mut fonts, &held, "14", 0.0);
    assert!(view.anchor.is_some());
    // An arrival moves nothing.
    held.extend((20..22).map(rec));
    at(&mut view, &mut fonts, &held, "14", 0.0);
    // A line down moves the record up a line; a line up moves it back (the wheel and Alt+Down use the same step).
    view.scroll(false, Step::Line);
    let line = BODY_LINE_H;
    at(&mut view, &mut fonts, &held, "14", -line);
    // A page down passes the record: none is left wholly above the top, and a page up returns to it.
    view.scroll(false, Step::Page);
    let (got, top) = top_of(&lay(&mut view, &mut fonts, &held, h));
    assert!(got != "14" && top + h > 0.0, "message {got} at {top}");
    view.scroll(true, Step::Page);
    at(&mut view, &mut fonts, &held, "14", -line);
    // Edits that change height keep the anchor's top: the anchor taller, a record below it taller, one above it taller.
    let long = "word ".repeat(200);
    for n in [14, 16, 10] {
        held[n] = rec_at(&format!("1700000{n:03}.000100"), &format!("{long}message {n}"));
        at(&mut view, &mut fonts, &held, "14", -line);
    }
    // A delete above the anchor moves nothing.
    held.remove(12);
    at(&mut view, &mut fonts, &held, "14", -line);
    // The anchor deleted: the next record takes its top.
    held.remove(13);
    at(&mut view, &mut fonts, &held, "15", -line);
    // A snapshot that replaces every record and drops the anchor: the first record newer than it, at the same top.
    let snapshot: Vec<ChatRecord> = (17..30).map(rec).collect();
    at(&mut view, &mut fonts, &snapshot, "17", -line);
    // A snapshot wholly older than the anchor leaves nothing newer: the view follows the newest.
    let older: Vec<ChatRecord> = (0..10).map(rec).collect();
    let shown = lay(&mut view, &mut fonts, &older, h);
    assert!(view.anchor.is_none() && near(shown.last().unwrap().1, 3.5 * h), "{shown:?}");
    // Page down from an anchor brings the newest wholly into view, and the position becomes None.
    view.scroll(true, Step::Page);
    lay(&mut view, &mut fonts, &held, h);
    assert!(view.anchor.is_some());
    for _ in 0..8 {
        view.scroll(false, Step::Page);
        let shown = lay(&mut view, &mut fonts, &held, h);
        let (text, top) = shown.last().unwrap();
        assert!(view.anchor.is_none() || !text.ends_with("message 21") || top + h > 4.5 * h, "the newest is in view but the reader is not following");
    }
    assert!(view.anchor.is_none(), "the newest is in view");
    // Paging up past the oldest stops with it at the top, never an empty body.
    for _ in 0..60 {
        view.scroll(true, Step::Page);
        lay(&mut view, &mut fonts, &held, h);
    }
    at(&mut view, &mut fonts, &held, "0", 0.0);
    // A line is the text scale's: at scale 2 a line down moves the anchor twice as far, and a key before the first frame
    // is kept.
    let mut big = ChatView::default();
    big.anchor = Some((held[10].ts.clone(), 0.0));
    big.scroll(false, Step::Line);
    let records: Vec<&ChatRecord> = held.iter().collect();
    big.layout(&mut fonts, &records, ScreenRect { x: 0.0, y: 0.0, w: 400.0, h: 1000.0 }, 2.0, 1_700_000_100, chrono::FixedOffset::east_opt(0).unwrap());
    assert!(near(big.placed_tops()[0], -2.0 * BODY_LINE_H), "{:?}", big.placed_tops());
    // End: back to the newest from anywhere, and a pending move is dropped.
    view.scroll(true, Step::Page);
    lay(&mut view, &mut fonts, &held, h);
    view.scroll(true, Step::Page);
    view.latest();
    let shown = lay(&mut view, &mut fonts, &held, h);
    assert!(view.anchor.is_none() && near(shown.last().unwrap().1, 3.5 * h));
}

#[test]
fn the_keys_and_the_wheel_reach_the_view_only_in_the_chat_tenant() {
    use crate::ui::DrawerContent::*;
    use crate::ui::drawer::chat::{chat_move, chat_wheel, Move};
    use crate::ui::input::keybindings::Action as A;
    for (action, want) in [
        (A::ScrollPageUp, Move::Scroll(true, Step::Page)), (A::ScrollPageDown, Move::Scroll(false, Step::Page)),
        (A::ChatLatest, Move::Latest),
    ] {
        assert_eq!(chat_move(Chat, Some(action)), Some(want), "{action:?}");
        for other in [Closed, Repl, Terminal, Monitor, Help] {
            assert_eq!(chat_move(other, Some(action)), None, "{action:?} in {other:?}");
        }
    }
    assert_eq!(chat_move(Chat, Some(A::ReplHistoryPrev)), None);
    assert_eq!(chat_move(Chat, None), None);
    assert_eq!(chat_wheel(Chat, 3), Some(Move::Scroll(true, Step::Line)));
    assert_eq!(chat_wheel(Chat, -3), Some(Move::Scroll(false, Step::Line)));
    for other in [Closed, Repl, Terminal, Monitor, Help] {
        assert_eq!(chat_wheel(other, 3), None, "{other:?}");
    }
    // End is the chat's alone.
    let ctx = |pane| crate::ui::input::help::Context { pane, ..Default::default() };
    assert!(ctx(crate::ui::input::help::Pane::Chat).allows(A::ChatLatest));
    for pane in [Pane::Nav, Pane::Preview, Pane::Repl, Pane::Terminal, Pane::Monitor, Pane::Agent, Pane::Help] {
        assert!(!ctx(pane).allows(A::ChatLatest), "{pane:?}");
    }
}

#[test]
fn a_row_change_returns_the_reader_to_the_newest() {
    let (mut d, at) = answered_with(mapped);
    let reply = ChatReply::Done(ChatWatchRes { state: ChatWatchState::Complete });
    let outs = d.on_evt(&"a".to_string(), ChatEvt::Watched { target: row_target(), reply }, at);
    let held: Vec<ChatRecord> = (0..20).map(rec).collect();
    let page = serde_json::from_value(serde_json::json!({ "records": held, "next": null })).unwrap();
    d.on_evt(&"a".to_string(), ChatEvt::History { req: history_req(&outs), reply: ChatReply::Done(page) }, at);
    let mut fonts = FontSystem::new();
    let body = ScreenRect { x: 0.0, y: 0.0, w: 400.0, h: 100.0 };
    d.layout(&mut fonts, body, 1.0);
    d.scroll(true, Step::Page);
    d.layout(&mut fonts, body, 1.0);
    assert!(!d.following(), "scrolled up");
    d.tick(shown("S"), at + Duration::from_secs(1));
    assert!(d.following(), "another row is read from its newest");
}
