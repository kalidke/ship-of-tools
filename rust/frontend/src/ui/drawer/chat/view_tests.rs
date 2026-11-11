use super::owner::{ChatOwner, Out};
use super::project::{project, Kind};
use super::view::*;
use crate::ui::render::text::EXTRA_TOP_PAD_PX;
use crate::ui::render::cells::CellQuad;
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

/// A record by its own author (the id follows the stamp), so records seconds apart each start a group.
fn rec_at(ts: &str, text: &str) -> ChatRecord {
    serde_json::from_value(serde_json::json!({ "workspace": "T1", "channel": "C1", "ts": ts, "thread_ts": ts,
        "author": { "id": format!("U{ts}"), "name": "Ada" }, "origin": "member", "text": text, "reply_count": 0, "files": [] })).unwrap()
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

/// Records as the view takes them: none deleted.
fn plain<'a>(r: &[&'a ChatRecord]) -> Vec<(&'a ChatRecord, bool)> {
    r.iter().map(|r| (*r, false)).collect()
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
    view.layout(&mut fonts, &plain(&records), body, 1.0, 1_700_000_000, offset);
    let shown = view.areas().len();
    assert!((1..=8).contains(&shown), "{shown}");
    assert_eq!(view.cached(), shown);
    // A crossing record is clipped at the body's top and bottom, and its glyphs sit at its place.
    let areas = view.areas();
    assert!(areas.iter().all(|a| a.clip_top == Some(body.y) && a.clip_left == Some(body.x) && a.bottom == body.y + body.h && a.right == body.x + body.w));
    assert!(areas.iter().any(|a| a.y + EXTRA_TOP_PAD_PX < body.y), "one record crosses the top");
    // Scrolling through the history keeps up to 64 others, the nearest to the position: the last positions' records stay and
    // the first positions' are gone.
    let spans = |i: usize| project(records[i], i.checked_sub(1).map(|j| records[j]), 1_700_000_000, offset, false);
    for from in (0..=240).step_by(8) {
        view.anchor = Some((records[from].ts.clone(), 0.0));
        view.layout(&mut fonts, &plain(&records), body, 1.0, 1_700_000_000, offset);
        assert!(view.cached() <= view.areas().len() + 64, "{from}: {}", view.cached());
    }
    assert!(view.cached() >= 60, "the spare buffers are kept: {}", view.cached());
    assert!(view.holds(&spans(240)) && view.holds(&spans(232)), "the nearest are kept");
    assert!(!view.holds(&spans(0)) && !view.holds(&spans(8)) && !view.holds(&spans(16)), "the farthest are gone");
    view.layout(&mut fonts, &plain(&records), ScreenRect { w: 200.0, ..body }, 1.0, 1_700_000_000, offset);
    assert!(view.cached() <= view.areas().len() + 64);
    view.hide();
    assert!(view.areas().is_empty());

    // Each kind has its own style; a body line that imitates a header keeps the body's.
    let kinds = [Kind::Day, Kind::Name, Kind::Time, Kind::Badge, Kind::Body, Kind::Markup, Kind::Meta, Kind::Replies, Kind::Last];
    for (i, a) in kinds.iter().enumerate() {
        for b in &kinds[i + 1..] {
            assert_ne!(attrs(*a, 1.0), attrs(*b, 1.0), "{a:?} {b:?}");
        }
    }
    let spans = project(&rec(1), None, 1_700_000_000, offset, false);
    assert_eq!((spans[0].kind, spans[1].kind), (Kind::Day, Kind::Name));
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
    view.layout(fonts, &plain(&records), body, 1.0, 1_700_000_100, chrono::FixedOffset::east_opt(0).unwrap());
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
    // One record's height, from two records of a following view that are not the oldest (which carries a divider).
    let probe = lay(&mut view, &mut fonts, &held, 1000.0);
    let h = probe[2].1 - probe[1].1;
    let at = |view: &mut ChatView, fonts: &mut FontSystem, held: &[ChatRecord], n: &str, dy: f32| {
        let shown = lay(view, fonts, held, h);
        let (got, top) = top_of(&shown);
        assert!(got == n && near(top, dy), "wanted message {n} at {dy}, got message {got} at {top}");
    };
    // Following: the newest ends at the bottom, and a step down changes nothing.
    view.layout(&mut fonts, &plain(&held.iter().collect::<Vec<_>>()), ScreenRect { x: 0.0, y: 0.0, w: 400.0, h: 4.5 * h }, 1.0, 1_700_000_100, chrono::FixedOffset::east_opt(0).unwrap());
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
    big.layout(&mut fonts, &plain(&records), ScreenRect { x: 0.0, y: 0.0, w: 400.0, h: 1000.0 }, 2.0, 1_700_000_100, chrono::FixedOffset::east_opt(0).unwrap());
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
    use crate::ui::input::help::DrawerInput;
    use crate::ui::input::keybindings::Action as A;
    // The tenant decision: the Chat's keys, wheel, line scroll and draft are reached through `DrawerInput::Chat` alone, so
    // typing in the Monitor or Help drawer (`Inert`) reaches neither the draft nor the REPL.
    assert_eq!(DrawerInput::of(Chat), DrawerInput::Chat);
    for other in [Closed, Repl, Terminal, Monitor, Help] {
        assert_ne!(DrawerInput::of(other), DrawerInput::Chat, "{other:?}");
    }
    assert_eq!((DrawerInput::of(Monitor), DrawerInput::of(Help)), (DrawerInput::Inert, DrawerInput::Inert));
    for (action, want) in [
        (A::ScrollPageUp, Move::Scroll(true, Step::Page)), (A::ScrollPageDown, Move::Scroll(false, Step::Page)),
        (A::ChatLatest, Move::Latest), (A::ChatMarkOlder, Move::Mark(true)), (A::ChatMarkNewer, Move::Mark(false)),
        (A::ChatThreadOpen, Move::Thread),
    ] {
        assert_eq!(chat_move(Some(action)), Some(want), "{action:?}");
    }
    assert_eq!(chat_move(Some(A::ReplHistoryPrev)), None);
    assert_eq!(chat_move(None), None);
    assert_eq!(chat_wheel(3), Some(Move::Scroll(true, Step::Line)));
    assert_eq!(chat_wheel(-3), Some(Move::Scroll(false, Step::Line)));
    assert_eq!(chat_wheel(0), None);
    // End is the chat's alone.
    let ctx = |pane| crate::ui::input::help::Context { pane, ..Default::default() };
    assert!(ctx(crate::ui::input::help::Pane::Chat).allows(A::ChatLatest));
    for pane in [Pane::Nav, Pane::Preview, Pane::Repl, Pane::Terminal, Pane::Monitor, Pane::Agent, Pane::Help] {
        assert!(!ctx(pane).allows(A::ChatLatest), "{pane:?}");
    }
    // K1: plain ArrowUp and ArrowDown mark in the Chat and keep every other pane's meaning; the Alt chords stay the scroll's.
    use crate::ui::input::keybindings::{KeyBindings, Modifiers};
    use winit::keyboard::{Key, NamedKey};
    let b = KeyBindings::defaults();
    let name = |pane, key: NamedKey, alt: bool| {
        let c = ctx(pane);
        b.resolve(&Key::Named(key), None, Modifiers { alt, ..Modifiers::default() }, c.consumes_text(), |a| c.allows(a)).map(|a| a.spec().name)
    };
    assert_eq!(name(Pane::Chat, NamedKey::ArrowUp, false), Some("chat.mark_older"));
    assert_eq!(name(Pane::Chat, NamedKey::ArrowDown, false), Some("chat.mark_newer"));
    assert_eq!(name(Pane::Chat, NamedKey::ArrowUp, true), Some("view.scroll_line_up"));
    assert_eq!(name(Pane::Repl, NamedKey::ArrowUp, false), Some("repl.history_prev"));
    for pane in [Pane::Nav, Pane::Preview, Pane::Repl, Pane::Terminal, Pane::Monitor, Pane::Agent, Pane::Help] {
        for key in [NamedKey::ArrowUp, NamedKey::ArrowDown] {
            assert!(!name(pane, key, false).is_some_and(|n| n.starts_with("chat.")), "{pane:?} {key:?}");
        }
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
    d.layout(&mut fonts, body, 1.0, body);
    d.scroll(true, Step::Page);
    d.layout(&mut fonts, body, 1.0, body);
    assert!(!d.following(), "scrolled up");
    d.tick(shown("S"), at + Duration::from_secs(1));
    assert!(d.following(), "another row is read from its newest");
}

/// B5i's rule for an anchor on a deleted message: it is a held record, so the anchor stays on it; a replacing snapshot drops
/// it, and the anchor passes to the nearest newer held record.
#[test]
fn an_anchor_on_a_deleted_message_passes_on_when_a_snapshot_drops_it() {
    let now = Instant::now();
    let h = "h".to_string();
    let held: Vec<ChatRecord> = (0..6).map(rec).collect();
    let mut o = complete_with(now, &held);
    let gone = serde_json::json!({ "workspace": "T1", "channel": "C1", "put": [], "deleted": [held[2].ts] });
    o.on_evt(&h, ChatEvt::Changed(serde_json::from_value(gone).unwrap()), now);
    let (mut fonts, mut view) = (FontSystem::new(), ChatView::default());
    let body = ScreenRect { x: 0.0, y: 0.0, w: 400.0, h: 100.0 };
    let offset = chrono::FixedOffset::east_opt(0).unwrap();
    view.anchor = Some((held[2].ts.clone(), 0.0));
    view.layout(&mut fonts, &o.records(), body, 1.0, 1_700_000_100, offset);
    assert_eq!(view.anchor.as_ref().map(|a| a.0.clone()), Some(held[2].ts.clone()), "the tombstone is held, so the anchor stays");
    assert!(view.placed_text().iter().any(|t| t.ends_with("(message deleted)")), "the view shows the owner's flag");
    o.on_bus_lost(&h, sot_protocol::op::CHAT_CHANGED, now);
    let reply = ChatReply::Done(ChatWatchRes { state: ChatWatchState::Complete });
    let req = history_req(&o.on_evt(&h, ChatEvt::Watched { target: target(), reply }, now));
    let page = serde_json::from_value(serde_json::json!({ "records": [held[0], held[1], held[3], held[4], held[5]], "next": null })).unwrap();
    o.on_evt(&h, ChatEvt::History { req, reply: ChatReply::Done(page) }, now);
    view.layout(&mut fonts, &o.records(), body, 1.0, 1_700_000_100, offset);
    assert_eq!(view.anchor.as_ref().map(|a| a.0.clone()), Some(held[3].ts.clone()), "the snapshot dropped the tombstone: the next held record");
}

/// Through the drawer: a record deleted while watched shows as deleted in what the drawer lays out.
#[test]
fn a_deleted_record_shows_as_deleted_through_the_drawer() {
    let (mut d, at) = answered_with(mapped);
    let reply = ChatReply::Done(ChatWatchRes { state: ChatWatchState::Complete });
    let outs = d.on_evt(&"a".to_string(), ChatEvt::Watched { target: row_target(), reply }, at);
    let held: Vec<ChatRecord> = (0..3).map(rec).collect();
    let page = serde_json::from_value(serde_json::json!({ "records": held, "next": null })).unwrap();
    d.on_evt(&"a".to_string(), ChatEvt::History { req: history_req(&outs), reply: ChatReply::Done(page) }, at);
    let gone = serde_json::json!({ "workspace": "T1", "channel": "C1", "put": [], "deleted": [held[1].ts] });
    d.on_evt(&"a".to_string(), ChatEvt::Changed(serde_json::from_value(gone).unwrap()), at);
    let mut fonts = FontSystem::new();
    d.layout(&mut fonts, ScreenRect { x: 0.0, y: 0.0, w: 400.0, h: 400.0 }, 1.0, ScreenRect { x: 0.0, y: 0.0, w: 400.0, h: 400.0 });
    let text = d.placed_text();
    assert_eq!(text.iter().filter(|t| t.ends_with("(message deleted)")).count(), 1, "{text:?}");
}

/// The reader's position reaches the owner each frame, so its trim spares the record the reader is on.
#[test]
fn the_readers_anchor_reaches_the_owner_each_frame() {
    let (mut d, at) = answered_with(mapped);
    let reply = ChatReply::Done(ChatWatchRes { state: ChatWatchState::Complete });
    let outs = d.on_evt(&"a".to_string(), ChatEvt::Watched { target: row_target(), reply }, at);
    let held: Vec<ChatRecord> = (0..20).map(rec).collect();
    let page = serde_json::from_value(serde_json::json!({ "records": held, "next": null })).unwrap();
    d.on_evt(&"a".to_string(), ChatEvt::History { req: history_req(&outs), reply: ChatReply::Done(page) }, at);
    let mut fonts = FontSystem::new();
    let body = ScreenRect { x: 0.0, y: 0.0, w: 400.0, h: 100.0 };
    d.layout(&mut fonts, body, 1.0, body);
    assert_eq!(d.owner_anchor(), None, "following");
    d.scroll(true, Step::Page);
    d.layout(&mut fonts, body, 1.0, body);
    let first = d.owner_anchor().expect("the reader left the newest");
    for _ in 0..6 {
        d.scroll(true, Step::Page);
    }
    d.layout(&mut fonts, body, 1.0, body);
    let second = d.owner_anchor().expect("still anchored");
    assert!(super::owner::ts_order(&second) < super::owner::ts_order(&first), "the owner follows the reader up: {first:?} then {second:?}");
}

/// The message mark: a held record, and in view (its first line inside the body).
#[test]
fn the_mark_is_a_held_record_and_in_view() {
    let mut fonts = FontSystem::new();
    let mut view = ChatView::default();
    let held: Vec<ChatRecord> = (0..20).map(rec).collect();
    let probe = lay(&mut view, &mut fonts, &held, 1000.0);
    let h = probe[2].1 - probe[1].1;
    let number = |view: &ChatView| view.mark.as_ref().map(|t| t.as_str()[7..10].parse::<usize>().unwrap());
    let top = |view: &ChatView, shown: &[(String, f32)]| (view.anchor.is_some(), top_of(shown));
    view.reset();
    // No mark: either key marks the newest, and a mark on the newest wholly in view leaves the view following.
    view.mark(false);
    let shown = lay(&mut view, &mut fonts, &held, h);
    assert_eq!((number(&view), view.anchor.is_none()), (Some(19), true));
    assert_eq!(view.placed_marks().iter().filter(|m| **m).count(), 1);
    assert_eq!(*view.placed_marks().last().unwrap(), true, "the newest is placed last: {shown:?}");
    // Moving on, with the cache warm: the mark is the view's own and only one record carries it.
    view.mark(true);
    let shown = lay(&mut view, &mut fonts, &held, h);
    assert_eq!(number(&view), Some(18));
    assert_eq!(view.placed_marks().iter().position(|m| *m), Some(shown.len() - 2));
    assert_eq!(view.placed_marks().iter().filter(|m| **m).count(), 1);
    // The newest is the end: ArrowDown on it stays there.
    view.mark(false);
    view.mark(false);
    lay(&mut view, &mut fonts, &held, h);
    assert_eq!(number(&view), Some(19));
    // A mark moved above the body puts that record's top at the body's top.
    for _ in 0..4 {
        view.mark(true);
    }
    let shown = lay(&mut view, &mut fonts, &held, h);
    assert_eq!(number(&view), Some(15));
    assert_eq!(top(&view, &shown).0, true);
    assert!(top(&view, &shown).1 .0 == "15" && near(top(&view, &shown).1 .1, 0.0), "{shown:?}");
    // A mark moved below the body puts its bottom at the body's foot: the newest, wholly in view, follows again.
    for _ in 0..4 {
        view.mark(false);
    }
    let shown = lay(&mut view, &mut fonts, &held, h);
    assert_eq!((number(&view), view.anchor.is_none()), (Some(19), true), "{shown:?}");
    assert!(near(shown.last().unwrap().1, 3.5 * h));
    // A page up that leaves the mark below the body moves the mark to the nearest record in view.
    view.scroll(true, Step::Page);
    let shown = lay(&mut view, &mut fonts, &held, h);
    let last = shown.last().unwrap().0.rsplit("message ").next().unwrap().parse::<usize>().unwrap();
    assert!(last < 19 && number(&view) == Some(last), "{last} {:?}", number(&view));
    // A page down that leaves it above the body does the same at the other edge.
    for _ in 0..4 {
        view.mark(true);
    }
    lay(&mut view, &mut fonts, &held, h);
    for _ in 0..3 {
        view.scroll(false, Step::Page);
    }
    let shown = lay(&mut view, &mut fonts, &held, h);
    // The record crossing the top has lost its header, so the mark takes the first one whose header is in.
    let first = top_of(&shown).0.parse::<usize>().unwrap() + usize::from(top_of(&shown).1 < 0.0);
    assert_eq!(number(&view), Some(first), "{shown:?}");
    // A resync that drops the marked record passes the mark to the nearest newer held one.
    view.reset();
    view.mark(true);
    view.mark(true);
    view.mark(true);
    lay(&mut view, &mut fonts, &held, h);
    assert_eq!(number(&view), Some(17));
    let fewer: Vec<ChatRecord> = held.iter().filter(|r| r.ts != held[17].ts).cloned().collect();
    lay(&mut view, &mut fonts, &fewer, h);
    assert_eq!(number(&view), Some(18));
    // With none newer the mark is gone, and the next key marks the newest again.
    let older: Vec<ChatRecord> = held[..17].to_vec();
    lay(&mut view, &mut fonts, &older, h);
    assert_eq!(number(&view), None);
    view.mark(true);
    lay(&mut view, &mut fonts, &older, h);
    assert_eq!(number(&view), Some(16));
    // A record taller than the body shows its top when marked.
    let mut tall = held.clone();
    tall[10] = rec_at(&tall[10].ts.as_str().to_string(), &format!("{}message 10", "word ".repeat(300)));
    view.reset();
    for _ in 0..16 {
        view.mark(true);
    }
    let shown = lay(&mut view, &mut fonts, &tall, h);
    assert!(top_of(&shown).0 == "4" && near(top_of(&shown).1, 0.0), "an unplaced record above shows its top: {shown:?}");
    for _ in 0..6 {
        view.mark(false);
    }
    let shown = lay(&mut view, &mut fonts, &tall, h);
    assert_eq!(number(&view), Some(10));
    assert!(top_of(&shown).0 == "10" && near(top_of(&shown).1, 0.0), "{shown:?}");
    // A change of the watched target clears the mark and the position.
    view.mark(true);
    view.scroll(true, Step::Page);
    lay(&mut view, &mut fonts, &older, h);
    view.reset();
    assert!(view.mark.is_none() && view.anchor.is_none());
    lay(&mut view, &mut fonts, &older, h);
    assert!(number(&view).is_none() && view.placed_marks().iter().all(|m| !m));
}

/// Through the drawer: a row change clears the mark.
#[test]
fn a_row_change_clears_the_mark() {
    let (mut d, at) = answered_with(mapped);
    let reply = ChatReply::Done(ChatWatchRes { state: ChatWatchState::Complete });
    let outs = d.on_evt(&"a".to_string(), ChatEvt::Watched { target: row_target(), reply }, at);
    let held: Vec<ChatRecord> = (0..20).map(rec).collect();
    let page = serde_json::from_value(serde_json::json!({ "records": held, "next": null })).unwrap();
    d.on_evt(&"a".to_string(), ChatEvt::History { req: history_req(&outs), reply: ChatReply::Done(page) }, at);
    let mut fonts = FontSystem::new();
    d.mark(true);
    d.layout(&mut fonts, ScreenRect { x: 0.0, y: 0.0, w: 400.0, h: 100.0 }, 1.0, ScreenRect { x: 0.0, y: 0.0, w: 400.0, h: 100.0 });
    assert_eq!(d.marked(), Some(held[19].ts.clone()));
    d.tick(shown("S"), at + Duration::from_secs(1));
    assert_eq!(d.marked(), None);
}

/// The drawer's layout returns the rectangles of the frame it laid out, so a mark moved or a record scrolled in this frame is
/// in them: the frame takes no rectangle from before its layout.
#[test]
fn the_layout_returns_this_frames_rectangles() {
    let (mut d, at) = answered_with(mapped);
    let reply = ChatReply::Done(ChatWatchRes { state: ChatWatchState::Complete });
    let outs = d.on_evt(&"a".to_string(), ChatEvt::Watched { target: row_target(), reply }, at);
    let held: Vec<ChatRecord> = (0..20).map(rec).collect();
    let page = serde_json::from_value(serde_json::json!({ "records": held, "next": null })).unwrap();
    d.on_evt(&"a".to_string(), ChatEvt::History { req: history_req(&outs), reply: ChatReply::Done(page) }, at);
    let mut fonts = FontSystem::new();
    let body = ScreenRect { x: 0.0, y: 0.0, w: 400.0, h: 100.0 };
    let inside = ScreenRect { h: 140.0, ..body };
    let first = d.layout(&mut fonts, body, 1.0, inside);
    assert_eq!(first[0].h, 140.0, "the first frame's background is the drawer's inside");
    assert!(of_color(&first, BAND).is_empty());
    d.mark(true);
    let quads = d.layout(&mut fonts, body, 1.0, inside);
    assert_eq!(of_color(&quads, BAND).len(), 1, "the mark moved in this frame is drawn in it");
}

/// Lays `held` out in a body of `h` px; the marked record's header top, when a record is marked and placed.
fn lay_px(view: &mut ChatView, fonts: &mut FontSystem, held: &[ChatRecord], h: f32) -> Option<f32> {
    let records: Vec<&ChatRecord> = held.iter().collect();
    let body = ScreenRect { x: 0.0, y: 0.0, w: 400.0, h };
    view.layout(fonts, &plain(&records), body, 1.0, 1_700_000_100, chrono::FixedOffset::east_opt(0).unwrap());
    view.placed_marks().iter().position(|m| *m).map(|i| view.placed_tops()[i])
}

/// "In view" is the first line inside the body: records of 44 px in a body of 100 px leave slivers at the top.
#[test]
fn the_marks_first_line_is_inside_the_body() {
    let mut fonts = FontSystem::new();
    let mut held: Vec<ChatRecord> = (0..20).map(rec).collect();
    let mut view = ChatView::default();
    let header_in = |top: Option<f32>| top.is_some_and(|t| t >= 0.0 && t + BODY_LINE_H <= 100.0);
    for _ in 0..3 {
        view.mark(true);
    }
    assert!(header_in(lay_px(&mut view, &mut fonts, &held, 100.0)));
    assert_eq!(view.mark.as_ref().map(|t| t.as_str()[7..10].to_string()), Some("017".into()));
    // End follows the newest: record 17's header would be above the body, so the mark moves to one whose header is not.
    view.latest();
    let top = lay_px(&mut view, &mut fonts, &held, 100.0);
    assert!(header_in(top), "{top:?}");
    // A page down that leaves the mark above the body.
    view.reset();
    for _ in 0..8 {
        view.mark(true);
    }
    lay_px(&mut view, &mut fonts, &held, 100.0);
    view.scroll(false, Step::Page);
    let top = lay_px(&mut view, &mut fonts, &held, 100.0);
    assert!(header_in(top), "{top:?}");
    // No header inside the body (a tall record crossing the top, the next one's header below the foot): the nearest placed.
    let mut three: Vec<ChatRecord> = (0..3).map(rec).collect();
    three[1] = rec_at(three[1].ts.as_str(), &format!("{}message 1", "word ".repeat(300)));
    let mut view = ChatView::default();
    lay_px(&mut view, &mut fonts, &three, 5000.0);
    let tall_h = view.placed_tops()[2] - view.placed_tops()[1];
    (view.anchor, view.mark) = (Some((three[1].ts.clone(), 80.0 - tall_h)), Some(three[0].ts.clone()));
    lay_px(&mut view, &mut fonts, &three, 100.0);
    assert_eq!(view.mark, Some(three[1].ts.clone()));
    // The long record's bottom 1 px above the foot, the next one's header below it: the mark goes to the long record that
    // fills the body, not to the sliver of the one at the foot.
    (view.anchor, view.mark) = (Some((three[1].ts.clone(), 99.0 - tall_h)), Some(three[2].ts.clone()));
    lay_px(&mut view, &mut fonts, &three, 100.0);
    assert_eq!(view.mark, Some(three[1].ts.clone()));
    // An arrival while following lifts the marked record out of the top.
    view.reset();
    for _ in 0..2 {
        view.mark(true);
    }
    assert!(header_in(lay_px(&mut view, &mut fonts, &held, 100.0)));
    for n in 20..24 {
        held.push(rec(n));
        let top = lay_px(&mut view, &mut fonts, &held, 100.0);
        assert!(header_in(top), "after arrival {n}: {top:?}");
    }
}

/// Keys that arrive before one frame fold one at a time, each with its clamp.
#[test]
fn mark_keys_fold_one_at_a_time() {
    let mut fonts = FontSystem::new();
    let held: Vec<ChatRecord> = (0..20).map(rec).collect();
    let mut marked = |keys: &[bool], first: &[bool]| {
        let mut view = ChatView::default();
        for k in first {
            view.mark(*k);
        }
        lay_px(&mut view, &mut fonts, &held, 400.0);
        for k in keys {
            view.mark(*k);
        }
        lay_px(&mut view, &mut fonts, &held, 400.0);
        view.mark.as_ref().map(|t| t.as_str()[7..10].parse::<usize>().unwrap())
    };
    // No mark: Up then Down marks the newest and stays; Down then Up marks the newest and steps back.
    assert_eq!(marked(&[true, false], &[]), Some(19));
    assert_eq!(marked(&[false, true], &[]), Some(18));
    // At the newest, Down on it stays, so Down, Down, Up gives the one before.
    assert_eq!(marked(&[false, false, true], &[false]), Some(18));
    // Up on the oldest stays there.
    assert_eq!(marked(&[true; 21], &[]), Some(0));
}

/// The text is 145 px wide: the width these tests were written for, right of the gutter.
const BODY: ScreenRect = ScreenRect { x: 37.0, y: 61.0, w: 145.0 + GUTTER_PX, h: 600.0 };
const NOW: i64 = 1_700_000_100;

fn lay_in(view: &mut ChatView, fonts: &mut FontSystem, held: &[ChatRecord], body: ScreenRect) {
    let records: Vec<&ChatRecord> = held.iter().collect();
    view.layout(fonts, &plain(&records), body, 1.0, NOW, chrono::FixedOffset::east_opt(0).unwrap());
}

/// Presses at the record's buffer `(line, byte)` and drags to `to`, as the pointer does; the copied bytes.
fn drag(view: &mut ChatView, ts: &sot_protocol::chat::Ts, from: (usize, usize), to: (usize, usize)) -> Option<String> {
    let hit = view.hit(view.point_at(ts, from.0, from.1).unwrap());
    view.press(hit);
    view.extend_drag(view.point_at(ts, to.0, to.1).unwrap());
    view.release();
    view.selected_text()
}

#[test]
fn chat_and_preview_copy_same_utf8_ranges() {
    use crate::ui::preview::{markdown::{highlight::HighlightService, MarkdownPreview}, shaped::ShapedPreview};
    const TEXT: &str = "alpha beta gamma delta epsilon zeta eta theta\nbravé ñandú charlie\nomega";
    let mut fonts = FontSystem::new();
    let held = [rec_at("1700000001.000100", TEXT), rec_at("1700000002.000100", "a &lt; b"), rec_at("1700000003.000100", "<https://x?a=1&amp;b=2>")];
    let mut view = ChatView::default();
    lay_in(&mut view, &mut fonts, &held, BODY);

    // The same drags on the preview of the same text (its line 0 is the record's line 2, after the divider and the header): the same bytes.
    let rect = ScreenRect { x: 37.0, y: 61.0, w: 145.0, h: 400.0 };
    let mut preview = ShapedPreview::new(MarkdownPreview::new_plain(&mut fonts, TEXT, rect.w, 1.0), HighlightService::new().unwrap(), rect);
    let at = |preview: &ShapedPreview, line: usize, byte: usize| {
        let run = preview.md().buffer.layout_runs().find(|r| r.line_i == line && r.glyphs.iter().any(|g| g.start == byte)).unwrap();
        let g = run.glyphs.iter().find(|g| g.start == byte).unwrap();
        (rect.x + g.x + g.w * 0.15, rect.y + EXTRA_TOP_PAD_PX + run.line_top + run.line_height * 0.5)
    };
    for (from, to) in [((0, 6), (2, 3)), ((0, 6), (1, 7)), ((1, 0), (1, 9)), ((2, 3), (0, 6))] {
        let start = at(&preview, from.0, from.1);
        preview.press(preview.hit(start, true));
        preview.extend_drag(at(&preview, to.0, to.1));
        preview.release();
        let chat = drag(&mut view, &held[0].ts, (from.0 + 2, from.1), (to.0 + 2, to.1));
        assert!(chat.is_some());
        assert_eq!(chat, preview.selected_text(), "{from:?} to {to:?}");
    }
    assert_eq!(drag(&mut view, &held[0].ts, (2, 6), (4, 3)).as_deref(), Some("beta gamma delta epsilon zeta eta theta\nbravé ñandú charlie\nome"));
    assert_eq!(drag(&mut view, &held[0].ts, (2, 6), (3, 7)).as_deref(), Some("beta gamma delta epsilon zeta eta theta\nbravé "));
    // Markup shown as typed is copied as shown.
    assert_eq!(drag(&mut view, &held[1].ts, (1, 0), (1, 4)).as_deref(), Some("a < "));
    let end = view.point_at(&held[1].ts, 1, 4).unwrap();
    view.extend_drag((end.0 + 200.0, end.1));
    assert_eq!(view.selected_text().as_deref(), Some("a < "), "a release ends the drag");
    assert_eq!(drag(&mut view, &held[2].ts, (1, 0), (1, 18)).as_deref(), Some("<https://x?a=1&b=2"));

    // The copy is the selection's bytes; with none it is the marked message's shown text, the header left out.
    view.clear_selection();
    assert_eq!(view.copy_text(), None, "no selection and no mark");
    view.mark(true);
    lay_in(&mut view, &mut fonts, &held, BODY);
    assert_eq!(view.mark.as_ref(), Some(&held[2].ts));
    assert_eq!(view.copy_text().as_deref(), Some("<https://x?a=1&b=2>"));
    drag(&mut view, &held[1].ts, (1, 0), (1, 4));
    assert_eq!(view.copy_text().as_deref(), Some("a < "), "the selection comes first");
    view.mark(true);
    view.mark(true);
    lay_in(&mut view, &mut fonts, &held, BODY);
    view.clear_selection();
    assert_eq!(view.copy_text().as_deref(), Some(TEXT), "a marked message's lines, as the record holds them");
    view.reset();
    lay_in(&mut view, &mut fonts, &held, BODY);

    // The tint: rectangles inside the body, below the divider and the header, inside the selected record.
    drag(&mut view, &held[0].ts, (2, 6), (4, 3));
    let top = BODY.y + view.placed_tops()[0];
    let rects = view.selection_rects();
    assert!(rects.len() >= 3);
    let height = view.placed_tops()[1] - view.placed_tops()[0];
    for r in &rects {
        assert!(r.x >= BODY.x && r.x + r.w <= BODY.x + BODY.w + 0.01 && r.y >= top + DAY_H + BODY_LINE_H - 0.01 && r.y + r.h <= top + height + 0.01 && r.y + r.h <= BODY.y + BODY.h + 0.01, "{r:?}");
    }
}

#[test]
fn a_press_selects_in_the_record_under_it_and_a_drag_stays_in_it() {
    let mut fonts = FontSystem::new();
    let held = [rec_at("1700000001.000100", "first one"), rec_at("1700000002.000100", "second one")];
    let mut view = ChatView::default();
    lay_in(&mut view, &mut fonts, &held, BODY);
    // A divider and a header start none, but the press is still in the body; outside the body there is no press at all.
    let (divider, header) = (view.point_at(&held[0].ts, 0, 0).unwrap(), view.point_at(&held[0].ts, 1, 0).unwrap());
    assert!(view.hit(divider).is_some_and(|h| !view.press(Some(h))));
    assert!(view.hit(header).is_some_and(|h| !view.press(Some(h))));
    assert!(view.selected_text().is_none());
    assert!(view.hit((BODY.x - 1.0, header.1)).is_none() && view.hit((BODY.x + 1.0, BODY.y + BODY.h + 1.0)).is_none());
    // The body's blank above the oldest record starts none.
    assert!(view.hit((BODY.x + 1.0, BODY.y + 1.0)).is_some_and(|h| !view.press(Some(h))));
    // A drag into the next record, or above into the header, stays in the pressed one.
    let into_second = (BODY.x + BODY.w - 1.0, view.point_at(&held[1].ts, 1, 3).unwrap().1);
    let hit = view.hit(view.point_at(&held[0].ts, 2, 2).unwrap());
    assert!(view.press(hit));
    view.extend_drag(into_second);
    assert_eq!(view.selected_text().as_deref(), Some("rst one"));
    view.extend_drag(divider);
    assert_eq!(view.selected_text().as_deref(), Some("fi"));
    view.extend_drag(header);
    assert_eq!(view.selected_text().as_deref(), Some("fi"));
}

#[test]
fn a_change_to_the_selected_buffer_clears_the_selection() {
    let mut fonts = FontSystem::new();
    let held: Vec<ChatRecord> = (0..3).map(rec).collect();
    let ts = held[1].ts.clone();
    let selected = |held: &[ChatRecord], body: ScreenRect, view: &mut ChatView, fonts: &mut FontSystem| {
        lay_in(view, fonts, held, body);
        drag(view, &ts, (1, 0), (1, 4))
    };
    let mut view = ChatView::default();
    assert_eq!(selected(&held, BODY, &mut view, &mut fonts).as_deref(), Some("mess"));
    // Unchanged frames keep it, and so does a scroll that leaves the record held.
    lay_in(&mut view, &mut fonts, &held, BODY);
    assert_eq!(view.selected_text().as_deref(), Some("mess"));
    assert!(!view.selection_rects().is_empty());

    let mut edited = held.clone();
    edited[1] = rec_at(ts.as_str(), "message 1, edited");
    lay_in(&mut view, &mut fonts, &edited, BODY);
    assert!(view.selected_text().is_none() && view.selection_rects().is_empty(), "an edit");

    selected(&held, BODY, &mut view, &mut fonts);
    let records: Vec<&ChatRecord> = held.iter().collect();
    let gone: Vec<(&ChatRecord, bool)> = records.iter().map(|r| (*r, r.ts == ts)).collect();
    view.layout(&mut fonts, &gone, BODY, 1.0, NOW, chrono::FixedOffset::east_opt(0).unwrap());
    assert!(view.selected_text().is_none(), "a delete");

    selected(&held, BODY, &mut view, &mut fonts);
    lay_in(&mut view, &mut fonts, &[held[0].clone(), held[2].clone()], BODY);
    assert!(view.selected_text().is_none(), "a resync that drops the record");
    // A record of the same words at the same minute does not take its place.
    let twins: Vec<ChatRecord> = ["1700000001.000100", "1700000002.000100", "1700000003.000100"].map(|ts| rec_at(ts, "same words")).to_vec();
    lay_in(&mut view, &mut fonts, &twins, BODY);
    drag(&mut view, &twins[1].ts, (1, 0), (1, 4));
    assert_eq!(view.selected_text().as_deref(), Some("same"));
    lay_in(&mut view, &mut fonts, &[twins[0].clone(), twins[2].clone()], BODY);
    assert!(view.selected_text().is_none(), "a twin of the dropped record");

    selected(&held, BODY, &mut view, &mut fonts);
    lay_in(&mut view, &mut fonts, &held, ScreenRect { w: 100.0, ..BODY });
    assert!(view.selected_text().is_none(), "a reshape at a new width");

    selected(&held, BODY, &mut view, &mut fonts);
    let records: Vec<&ChatRecord> = held.iter().collect();
    view.layout(&mut fonts, &plain(&records), BODY, 1.25, NOW, chrono::FixedOffset::east_opt(0).unwrap());
    assert!(view.selected_text().is_none(), "a new text scale at the same width");

    selected(&held, BODY, &mut view, &mut fonts);
    view.reset();
    assert!(view.selected_text().is_none(), "a row change");

    // A record the trim let go: scrolled far from it, its buffer is evicted.
    let many: Vec<ChatRecord> = (0..256).map(rec).collect();
    let first = many[0].ts.clone();
    let small = ScreenRect { h: 120.0, ..BODY };
    view = ChatView::default();
    view.anchor = Some((first.clone(), 0.0));
    lay_in(&mut view, &mut fonts, &many, small);
    drag(&mut view, &first, (2, 0), (2, 4));
    assert!(view.selected_text().is_some());
    // Every position is visited, so the buffers shaped (a few a layout, as many as the host's fonts leave in view) far exceed
    // the 64 kept besides those in view whatever the fonts: stepping by 8 left the host's wrap to decide whether any went.
    for from in 1..=248 {
        view.anchor = Some((many[from].ts.clone(), 0.0));
        lay_in(&mut view, &mut fonts, &many, small);
    }
    assert!(view.selected_text().is_none() && view.selection_rects().is_empty(), "an evicted buffer");
    // And it stays cleared when the record is shaped again.
    view.anchor = Some((first, 0.0));
    lay_in(&mut view, &mut fonts, &many, small);
    assert!(view.selected_text().is_none(), "an evicted buffer shaped again");
}

#[test]
fn the_copy_goes_to_the_clipboard_or_says_why_not() {
    use super::copy_status;
    let mut sent = None;
    assert_eq!(copy_status(Some("a < b".into()), |t| { sent = Some(t); Ok(()) }), "copied chat text");
    assert_eq!(sent.as_deref(), Some("a < b"));
    assert_eq!(copy_status(Some("x".into()), |_| Err("no clipboard".into())), "copy failed · no clipboard");
    assert_eq!(copy_status(None, |_| panic!("nothing is written")), "nothing to copy: select text or mark a message");
}

fn rec_by(ts: &str, name: &str, origin: &str, text: &str) -> ChatRecord {
    serde_json::from_value(serde_json::json!({ "workspace": "T1", "channel": "C1", "ts": ts, "thread_ts": ts,
        "author": { "id": "U1", "name": name }, "origin": origin, "text": text, "reply_count": 0, "files": [] })).unwrap()
}

/// The rectangles of `color` among `quads`, in order.
fn of_color(quads: &[CellQuad], color: (u8, u8, u8)) -> Vec<&CellQuad> {
    quads.iter().filter(|q| q.color == color).collect()
}

const LOW: (u8, u8, u8) = (0x32, 0x35, 0x38);
const BAND: (u8, u8, u8) = (0x21, 0x24, 0x28);
const SKY: (u8, u8, u8) = (0x1D, 0x9B, 0xD1);
const TINT: (u8, u8, u8) = (0x1B, 0x43, 0x56);
const DRAWER: ScreenRect = ScreenRect { x: 30.0, y: 50.0, w: 420.0, h: 400.0 };

/// The drawer's body: the drawer less a two-row foot.
fn body_of_drawer() -> ScreenRect {
    ScreenRect { h: DRAWER.h - 44.0, ..DRAWER }
}

fn lay_drawer(view: &mut ChatView, fonts: &mut FontSystem, held: &[ChatRecord]) {
    lay_in(view, fonts, held, body_of_drawer());
}

/// A divider's line, in pixels at text scale 1.
const DAY_H: f32 = 30.0;

/// The badges' rectangles of the placed records: those in `LOW` below the divider's lines (the divider's pill and rules are in
/// it too, and lie in the oldest record's first lines, which a narrow drawer wraps into more than one).
fn badges_in<'a>(view: &ChatView, quads: &'a [CellQuad]) -> Vec<&'a CellQuad> {
    let divider: f32 = view.placed_buffers()[0].layout_runs().filter(|r| r.line_i == 0).map(|r| r.line_height).sum();
    let below = DRAWER.y + view.placed_tops()[0] + divider;
    of_color(quads, LOW).into_iter().filter(|q| q.y >= below - 0.01).collect()
}

/// The header line is the name, then the time at 12/15 of its size and muted, and is as tall as a body line: a smaller
/// span keeps the line's height only because its metrics carry it (cosmic-text takes a line's height from the glyphs that
/// set one).
#[test]
fn the_header_is_the_bold_name_and_a_smaller_muted_time() {
    let mut fonts = FontSystem::new();
    assert_eq!(attrs(Kind::Name, 1.0).weight, cosmic_text::Weight::BOLD);
    for scale in [1.0_f32, 1.5] {
        let held = [rec(1)];
        let records: Vec<&ChatRecord> = held.iter().collect();
        let mut view = ChatView::default();
        view.layout(&mut fonts, &plain(&records), body_of_drawer(), scale, NOW, chrono::FixedOffset::east_opt(0).unwrap());
        let buffers = view.placed_buffers();
        let run = buffers[0].layout_runs().find(|r| r.line_i == 1).expect("the header line, after the divider's");
        let (name, time) = (run.glyphs.first().unwrap(), run.glyphs.last().unwrap());
        assert!(time.font_size < name.font_size, "the time is smaller: {} against {}", time.font_size, name.font_size);
        assert!(near(time.font_size, name.font_size * 12.0 / 15.0), "12/15 of the name: {} against {}", time.font_size, name.font_size);
        assert!(near(run.line_height, BODY_LINE_H * scale), "the header line keeps the body's height: {}", run.line_height);
        assert_eq!(time.color_opt, Some(cosmic_text::Color::rgb(0x9A, 0x9C, 0x9E)));
        assert_eq!(name.color_opt, Some(cosmic_text::Color::rgb(0xD1, 0xD2, 0xD3)));
    }
    // Every visual line of a header, wrapped or not, is a body line tall: one that holds only the badge (the time wrapped off
    // it) or only the time takes its height from those glyphs alone.
    let drawer = [rec_by("1700000001.000100", "Ada Lovelace", "drawer", "x")];
    let records: Vec<&ChatRecord> = drawer.iter().collect();
    for scale in [1.0_f32, 1.5] {
        for w in (90..=320).step_by(3) {
            let mut view = ChatView::default();
            // `w` is the text's width, right of the gutter.
            view.layout(&mut fonts, &plain(&records), ScreenRect { w: w as f32 + GUTTER_PX * scale, ..body_of_drawer() }, scale, NOW, chrono::FixedOffset::east_opt(0).unwrap());
            for run in view.placed_buffers()[0].layout_runs().filter(|r| r.line_i == 1) {
                assert!(near(run.line_height, BODY_LINE_H * scale), "width {w}, scale {scale}: a header line is {} tall", run.line_height);
            }
        }
    }
}

/// A badge is drawn from the record's origin alone: a name or a body that reads as one draws none.
#[test]
fn the_badge_comes_from_the_origin_and_no_text_draws_one() {
    let mut fonts = FontSystem::new();
    let mut view = ChatView::default();
    for (origin, name, text, badges) in [
        ("member", "Eve [drawer]", "[drawer] [row] APP DRAWER ROW", 0),
        ("member", "ROW", "APP", 0),
        ("member", "APP", "x", 0),
        ("other", "Ada", "x", 0),
        ("drawer", "Ada", "x", 1),
        ("row", "Ada", "x", 1),
        ("bot", "Ada", "x", 1),
    ] {
        let held = [rec_by("1700000001.000100", name, origin, text)];
        lay_drawer(&mut view, &mut fonts, &held);
        let quads = view.backs(DRAWER);
        let drawn = badges_in(&view, &quads);
        assert_eq!(drawn.len(), badges, "{origin} {name:?} {text:?}");
        for q in drawn {
            // As wide as its text and the padding of 4 px each side, and inside the header line.
            let buffers = view.placed_buffers();
            let run = buffers[0].layout_runs().find(|r| r.line_i == 1).unwrap();
            let top = body_of_drawer().y + view.placed_tops()[0] + run.line_top;
            assert!(q.w > 8.0 && q.x >= body_of_drawer().x && q.y >= top && q.y + q.h <= top + run.line_height, "{q:?}");
        }
    }
}

/// The mark is a band across the record's width with a 2 px bar at its left, and it does not restyle the record: moving it
/// onto or off the selected record keeps the selection.
#[test]
fn the_mark_is_a_band_and_moving_it_keeps_the_selection() {
    let mut fonts = FontSystem::new();
    let held: Vec<ChatRecord> = (0..3).map(rec).collect();
    let body = body_of_drawer();
    let mut view = ChatView::default();
    lay_drawer(&mut view, &mut fonts, &held);
    assert!(of_color(&view.backs(DRAWER), BAND).is_empty(), "no mark, no band");
    drag(&mut view, &held[1].ts, (1, 0), (1, 4));
    assert_eq!(view.selected_text().as_deref(), Some("mess"));
    for marked in [2, 1, 0] {
        // At most one key a record away: a mark that does not arrive fails the test, it does not hang it.
        for _ in 0..held.len() {
            if view.mark.as_ref() == Some(&held[marked].ts) {
                break;
            }
            view.mark(true);
            lay_drawer(&mut view, &mut fonts, &held);
        }
        assert_eq!(view.mark.as_ref(), Some(&held[marked].ts), "the mark reaches record {marked}");
        assert_eq!(view.selected_text().as_deref(), Some("mess"), "the mark on record {marked} leaves the selection");
        let quads = view.backs(DRAWER);
        let (bands, bars) = (of_color(&quads, BAND), of_color(&quads, SKY));
        assert_eq!((bands.len(), bars.len()), (1, 1), "record {marked}");
        let (top, height) = (view.placed_tops()[marked], view.placed_buffers()[marked].layout_runs().map(|r| r.line_height).sum::<f32>());
        let (band, bar) = (bands[0], bars[0]);
        // The oldest record's divider is above its band.
        let day = if marked == 0 { DAY_H } else { 0.0 };
        assert!(near(band.x, body.x) && near(band.w, body.w) && near(band.y, body.y + top + day) && near(band.h, height - day), "{band:?} {height}");
        assert!(near(bar.x, body.x) && near(bar.w, 2.0) && near(bar.y, band.y) && near(bar.h, band.h), "{bar:?}");
    }
}

/// The bar at the mark's left edge is 2 px at text scale 1 and follows the scale.
#[test]
fn the_marks_bar_follows_the_text_scale() {
    let mut fonts = FontSystem::new();
    let held: Vec<ChatRecord> = (0..2).map(rec).collect();
    let records: Vec<&ChatRecord> = held.iter().collect();
    for scale in [1.0_f32, 2.0] {
        let mut view = ChatView::default();
        view.mark = Some(held[1].ts.clone());
        view.layout(&mut fonts, &plain(&records), body_of_drawer(), scale, NOW, chrono::FixedOffset::east_opt(0).unwrap());
        let quads = view.backs(DRAWER);
        let bars = of_color(&quads, SKY);
        assert!(bars.len() == 1 && near(bars[0].w, 2.0 * scale), "{scale}: {bars:?}");
    }
}

/// A badge's rectangle is its run of glyphs, padding included, on every visual line a narrow drawer wraps the header into: its
/// left padding is whole wherever the line starts (at the body's edge too), and where the badge ends a line (the time wraps
/// after it) the rectangle still covers the run, though the wrap drops the padding after the word that does not fit.
#[test]
fn a_badge_keeps_its_rectangle_and_its_padding_where_a_wrap_cuts_the_header() {
    let mut fonts = FontSystem::new();
    let flag = attrs(Kind::Badge, 1.0).metadata;
    let held = [rec_by("1700000001.000100", "Ada Lovelace", "drawer", "x")];
    let records: Vec<&ChatRecord> = held.iter().collect();
    let (mut ends_a_line, mut starts_a_line) = (false, false);
    for scale in [1.0_f32, 1.5] {
        for w in (90..=320).step_by(3) {
            let body = ScreenRect { w: w as f32 + GUTTER_PX * scale, ..body_of_drawer() };
            let mut view = ChatView::default();
            view.layout(&mut fonts, &plain(&records), body, scale, NOW, chrono::FixedOffset::east_opt(0).unwrap());
            let quads = view.backs(DRAWER);
            let drawn = badges_in(&view, &quads);
            let buffer = view.placed_buffers()[0];
            let mut badges = 0;
            for run in buffer.layout_runs() {
                let g: Vec<_> = run.glyphs.iter().filter(|g| g.metadata & flag != 0).collect();
                if g.is_empty() {
                    continue;
                }
                badges += 1;
                // Two no-break spaces, the word, two more: the padding is glyphs, so it scales and no clip takes it.
                assert!((8..=10).contains(&g.len()), "width {w}: {} glyphs", g.len());
                assert!(g[0].w + g[1].w >= 4.0 * scale, "width {w}: the left padding is {:?}", [g[0].w, g[1].w]);
                let last = g[g.len() - 1];
                assert!(drawn.iter().any(|q| near(q.x, body.x + GUTTER_PX * scale + g[0].x) && near(q.w, last.x + last.w - g[0].x)), "width {w}: no rectangle for the run at {}", g[0].x);
                ends_a_line |= run.glyphs.last().is_some_and(|l| l.metadata & flag != 0);
                starts_a_line |= run.glyphs.first().is_some_and(|f| f.metadata & flag != 0);
            }
            assert_eq!(drawn.len(), badges, "width {w}");
        }
    }
    assert!(ends_a_line && starts_a_line, "the widths tried wrap the header both ways: {ends_a_line} {starts_a_line}");
}

/// The selection's tint comes after the background, the band and every fill, so it lies over them and under the glyphs.
#[test]
fn the_selection_lies_over_every_fill_and_is_the_last_rect() {
    let mut fonts = FontSystem::new();
    let held = [rec_at("1700000001.000100", "first"), rec_by("1700000002.000100", "Ada", "drawer", "second record of words")];
    let mut view = ChatView::default();
    lay_drawer(&mut view, &mut fonts, &held);
    view.mark(true);
    lay_drawer(&mut view, &mut fonts, &held);
    assert_eq!(view.mark.as_ref(), Some(&held[1].ts));
    drag(&mut view, &held[1].ts, (1, 0), (1, 6));
    let quads = view.backs(DRAWER);
    let first = quads.iter().position(|q| q.color == TINT).expect("a tint");
    assert!(quads[first..].iter().all(|q| q.color == TINT), "the tint is the last rectangles: {quads:?}");
    assert_eq!(quads[first..].len(), view.selection_rects().len());
    let before: Vec<_> = quads[..first].iter().map(|q| q.color).collect();
    assert_eq!(before.first(), Some(&(0x1A, 0x1D, 0x21)), "the background is first");
    for fill in [BAND, SKY, LOW] {
        assert!(before.contains(&fill), "{fill:?} is drawn under the tint");
    }
}

/// One background covers the drawer, foot included; every other rectangle lies inside the body, a record crossing its edge
/// cut at it; and nothing is drawn while the chat is not shown.
#[test]
fn every_rect_is_inside_the_body_and_the_background_covers_the_drawer() {
    let mut fonts = FontSystem::new();
    let held: Vec<ChatRecord> = (0..8).map(|n| rec_by(&format!("1700000{n:03}.000100"), "Ada", if n % 2 == 1 { "drawer" } else { "member" }, &format!("message {n}\nmore of {n}"))).collect();
    let body = ScreenRect { h: 100.0, ..body_of_drawer() };
    let mut view = ChatView::default();
    view.anchor = Some((held[2].ts.clone(), -20.0));
    view.mark = Some(held[3].ts.clone());
    lay_in(&mut view, &mut fonts, &held, body);
    let last_h: f32 = view.placed_buffers().last().unwrap().layout_runs().map(|r| r.line_height).sum();
    assert!(view.placed_tops()[0] < 0.0 && view.placed_tops().last().unwrap() + last_h > body.h, "records cross the top and the foot");
    assert_eq!(view.mark.as_ref(), Some(&held[3].ts));
    drag(&mut view, &held[3].ts, (1, 0), (1, 4));
    let quads = view.backs(DRAWER);
    assert_eq!((quads[0].x, quads[0].y, quads[0].w, quads[0].h, quads[0].color), (DRAWER.x, DRAWER.y, DRAWER.w, DRAWER.h, (0x1A, 0x1D, 0x21)));
    assert_eq!(of_color(&quads, (0x1A, 0x1D, 0x21)).len(), 1);
    assert!(quads.len() > 3, "a band, badges and a tint are drawn: {quads:?}");
    for q in &quads[1..] {
        assert!(q.x >= body.x - 0.01 && q.x + q.w <= body.x + body.w + 0.01 && q.y >= body.y - 0.01 && q.y + q.h <= body.y + body.h + 0.01, "{q:?} is outside {body:?}");
    }
    for q in &quads {
        assert!(q.x >= DRAWER.x && q.x + q.w <= DRAWER.x + DRAWER.w && q.y >= DRAWER.y && q.y + q.h <= DRAWER.y + DRAWER.h, "{q:?}");
    }
    view.hide();
    assert!(view.backs(DRAWER).is_empty());
}

/// `rec_by`'s record: Ada, a member, `secs` seconds after 22:15:00 UTC on the day of `NOW` (1700000100).
fn by_ada(secs: i64, text: &str) -> ChatRecord {
    rec_by(&format!("{}.000100", 1_700_000_100 + secs), "Ada", "member", text)
}

/// The press and the drag at buffer `(line, byte)` of `ts`, and what they select.
fn selects(view: &mut ChatView, ts: &sot_protocol::chat::Ts, line: usize) -> Option<String> {
    let hit = view.hit(view.point_at(ts, line, 0).unwrap());
    view.press(hit);
    view.extend_drag(view.point_at(ts, line, 4).unwrap());
    view.release();
    view.selected_text()
}

/// A continuation's first line is body and selects; a header never does, and nor does the gutter.
#[test]
fn a_continuations_first_line_selects_and_a_header_never_does() {
    let mut fonts = FontSystem::new();
    let held = [by_ada(0, "alpha line"), by_ada(30, "gamma line")];
    let mut view = ChatView::default();
    lay_drawer(&mut view, &mut fonts, &held);
    let buffers = view.placed_buffers();
    assert_eq!((buffers[0].lines.len(), buffers[1].lines.len()), (3, 1), "a divider, a header and a body line, then a body line alone");
    assert_eq!(selects(&mut view, &held[1].ts, 0).as_deref(), Some("gamm"));
    // The divider's and the header's glyphs select nothing; the body line below them does.
    assert_eq!(selects(&mut view, &held[0].ts, 0), None);
    assert_eq!(selects(&mut view, &held[0].ts, 1), None);
    assert_eq!(selects(&mut view, &held[0].ts, 2).as_deref(), Some("alph"));
    // A press in the gutter beside a continuation selects nothing.
    let (x, y) = (view.point_at(&held[1].ts, 0, 0).unwrap().0 - GUTTER_PX / 2.0, view.point_at(&held[1].ts, 0, 0).unwrap().1);
    let hit = view.hit((x, y));
    assert!(view.press(hit), "the press ended the old selection");
    assert_eq!(view.selected_text(), None);
    // The text is placed right of the gutter.
    assert!(view.areas().iter().all(|a| a.x >= body_of_drawer().x + GUTTER_PX - 0.01));
}

/// A marked continuation shows its time, `h:mm`, small and muted, right-aligned in the gutter; a marked header and an
/// unmarked continuation show none, and the time follows the scale.
#[test]
fn the_marked_continuation_shows_its_time_in_the_gutter() {
    let mut fonts = FontSystem::new();
    let held = [by_ada(0, "alpha"), by_ada(30, "gamma")];
    let body = body_of_drawer();
    for scale in [1.0_f32, 2.0] {
        let records: Vec<&ChatRecord> = held.iter().collect();
        let mut view = ChatView::default();
        let mut lay = |view: &mut ChatView| view.layout(&mut fonts, &plain(&records), body, scale, NOW, chrono::FixedOffset::east_opt(0).unwrap());
        lay(&mut view);
        assert_eq!(view.areas().len(), 2, "no mark, no time");
        // With no mark a key marks the newest.
        view.mark(false);
        lay(&mut view);
        let areas = view.areas();
        assert_eq!((view.mark.as_ref(), areas.len()), (Some(&held[1].ts), 3));
        let stamp = &areas[2];
        let run = stamp.buffer.layout_runs().next().unwrap();
        let text: String = run.text.to_string();
        assert_eq!(text, "10:15", "22:15:30 under a zero offset");
        let (first, last) = (run.glyphs.first().unwrap(), run.glyphs.last().unwrap());
        assert!(near(first.font_size, 11.0 * scale) && first.color_opt == Some(cosmic_text::Color::rgb(0x9A, 0x9C, 0x9E)), "{} {:?}", first.font_size, first.color_opt);
        assert!(near(stamp.x, body.x) && near(stamp.y + EXTRA_TOP_PAD_PX, body.y + view.placed_tops()[1]), "in the gutter at the record's first line");
        let right = stamp.x + last.x + last.w;
        assert!(right <= body.x + (GUTTER_PX - 4.0) * scale && right >= body.x + (GUTTER_PX - 12.0) * scale, "right-aligned near the gutter's edge: {right}");
        // Moving the mark to the header takes the time away: a marked header shows none.
        view.mark(true);
        lay(&mut view);
        assert_eq!((view.mark.as_ref(), view.areas().len()), (Some(&held[0].ts), 2));
    }
}

/// The marked message's copy skips only the lead: a header's line, and nothing of a continuation.
#[test]
fn the_marked_text_skips_only_the_lead() {
    let mut fonts = FontSystem::new();
    let held = [by_ada(0, "alpha\nbeta"), by_ada(30, "gamma\ndelta")];
    let mut view = ChatView::default();
    lay_drawer(&mut view, &mut fonts, &held);
    view.mark(false);
    lay_drawer(&mut view, &mut fonts, &held);
    assert_eq!(view.marked_text().as_deref(), Some("gamma\ndelta"), "a continuation has no lead");
    view.mark(true);
    lay_drawer(&mut view, &mut fonts, &held);
    assert_eq!(view.marked_text().as_deref(), Some("alpha\nbeta"), "a header's line is skipped");
}

/// A tile is one square in the gutter for each group's first record, colored from the author alone, and no text draws one.
#[test]
fn a_tile_starts_each_group_and_no_text_draws_one() {
    let mut fonts = FontSystem::new();
    let mut eve = rec_by("1700000200.000100", "Eve", "member", "own group");
    eve.author.id = "U2".to_string();
    let held = [by_ada(0, "first"), by_ada(30, "Ada  10:15 PM\nlooks like a header"), eve];
    let records: Vec<&ChatRecord> = held.iter().collect();
    let body = body_of_drawer();
    // At each text scale: one tile a group, at its place and size times the scale, in the color of its author.
    for scale in [1.0_f32, 1.5] {
        let mut view = ChatView::default();
        view.layout(&mut fonts, &plain(&records), body, scale, NOW, chrono::FixedOffset::east_opt(0).unwrap());
        let quads = view.backs(DRAWER);
        let tiles: Vec<_> = quads.iter().filter(|q| TILES.contains(&q.color)).collect();
        assert_eq!(tiles.len(), 2, "one a group at scale {scale}: {tiles:?}");
        // The first group's record is the oldest, so its divider is above its tile.
        for ((t, top), author) in tiles.iter().zip([view.placed_tops()[0] + DAY_H * scale, view.placed_tops()[2]]).zip([&held[0].author, &held[2].author]) {
            let (left, side, down) = (8.0 * scale, 34.0 * scale, 2.0 * scale);
            assert!(near(t.x, body.x + left) && near(t.w, side) && near(t.h, side) && near(t.y, body.y + top + down), "scale {scale}: {t:?}");
            assert_eq!(t.color, tile_color(author), "the color is the author's");
        }
        // The same author keeps the color, and one that drew none of its own leaves none.
        let mut again = ChatView::default();
        again.layout(&mut fonts, &plain(&records[..1]), body, scale, NOW, chrono::FixedOffset::east_opt(0).unwrap());
        assert_eq!(again.backs(DRAWER).iter().find(|q| TILES.contains(&q.color)).map(|q| q.color), Some(tiles[0].color));
    }
    // The color follows the id and the name each: some other id under one name, and some other name under one id, differ (two
    // people's drawer posts share a bot's id), and the gutter is wider than the tile and its left gap.
    let author = |id: &str, name: &str| sot_protocol::chat::ChatAuthor { id: id.to_string(), name: name.to_string() };
    let colors = |of: &dyn Fn(usize) -> sot_protocol::chat::ChatAuthor| (0..32).map(|n| tile_color(&of(n))).collect::<std::collections::HashSet<_>>();
    assert!(colors(&|n| author(&format!("U{n}"), "Ada")).len() > 1, "the id is not part of the color");
    assert!(colors(&|n| author("U1", &format!("N{n}"))).len() > 1, "the name is not part of the color");
    assert!(TILE_LEFT_PX + TILE_PX < GUTTER_PX, "the tile lies inside the gutter");
}

/// A group's first record is at least as tall as its tile, so a header alone (an empty text) does not run its tile into the
/// next record's.
#[test]
fn a_header_alone_is_as_tall_as_its_tile() {
    let mut fonts = FontSystem::new();
    let held = [rec_by("1700000100.000100", "Ada", "member", ""), rec_by("1700000100.000200", "Ben", "member", "")];
    let mut view = ChatView::default();
    lay_drawer(&mut view, &mut fonts, &held);
    let (tops, quads) = (view.placed_tops(), view.backs(DRAWER));
    let tiles: Vec<_> = quads.iter().filter(|q| TILES.contains(&q.color)).collect();
    assert_eq!(tiles.len(), 2);
    assert!(tops[1] - tops[0] >= 36.0, "the first record is {} tall", tops[1] - tops[0]);
    assert!(tiles[0].y + tiles[0].h <= body_of_drawer().y + tops[1], "the first tile ends above the next record: {tiles:?}");
}

/// A page joining above takes the old oldest record's header off and moves every record below it up a line; the oldest held
/// record always starts a group, so the one that becomes oldest gains a header and the records below it move down a line.
#[test]
fn a_page_above_moves_every_record_below_it_by_one_line() {
    let mut fonts = FontSystem::new();
    let held: Vec<ChatRecord> = (0..40).map(|n| by_ada(n, "x")).collect();
    let line = BODY_LINE_H;
    // Anchored on the 11th record, with the ten before it held or not.
    let layout = |from: usize, view: &mut ChatView, fonts: &mut FontSystem| {
        view.anchor = Some((held[10].ts.clone(), 0.0));
        lay_drawer(view, fonts, &held[from..]);
        (view.placed_buffers()[0].lines.len(), view.placed_tops()[1] - view.placed_tops()[0])
    };
    assert_eq!(layout(10, &mut ChatView::default(), &mut fonts), (3, DAY_H + 2.0 * line), "the oldest held record has a divider, a header and its text");
    assert_eq!(layout(0, &mut ChatView::default(), &mut fonts), (1, line), "with a page above it, one line and the next record up by its divider and header");
    // The oldest leaving: the record after it becomes the oldest and gains a divider and a header.
    assert_eq!(layout(11, &mut ChatView::default(), &mut fonts).0, 3);
}

/// A divider is a lead line: no press, drag or copy reaches it, on the record that starts a day and on the oldest alike.
#[test]
fn a_divider_is_neither_selected_nor_copied() {
    let mut fonts = FontSystem::new();
    // The oldest is yesterday's and the other today's: each starts a day.
    let held = [rec_by("1699900000.000100", "Ada", "member", "alpha beta"), by_ada(0, "gamma delta")];
    let mut view = ChatView::default();
    lay_drawer(&mut view, &mut fonts, &held);
    let buffers = view.placed_buffers();
    assert_eq!(buffers.iter().map(|b| b.lines[0].text().trim_matches('\u{a0}').to_string()).collect::<Vec<_>>(), ["Yesterday", "Today"]);
    for (i, ts) in held.iter().map(|r| &r.ts).enumerate() {
        assert_eq!(selects(&mut view, ts, 0), None, "record {i}: a press and drag on the divider");
        assert_eq!(selects(&mut view, ts, 1), None, "record {i}: the header");
        assert!(selects(&mut view, ts, 2).is_some(), "record {i}: the body");
        // A drag that ends on the divider stays in the body.
        let hit = view.hit(view.point_at(ts, 2, 3).unwrap());
        view.press(hit);
        view.extend_drag(view.point_at(ts, 0, 0).unwrap());
        let text = view.selected_text().expect("a selection");
        assert!(!text.is_empty() && held[i].text.starts_with(&text), "record {i}: {text:?}");
        view.clear_selection();
    }
    // The marked message's copy is its body alone.
    view.mark(false);
    lay_drawer(&mut view, &mut fonts, &held);
    assert_eq!(view.marked_text().as_deref(), Some("gamma delta"));
    view.mark(true);
    lay_drawer(&mut view, &mut fonts, &held);
    assert_eq!(view.marked_text().as_deref(), Some("alpha beta"));
}

/// A divider's label is centered in the text's width, in a pill a body line tall with a border and the ground inside it, and a
/// rule runs from each side of the pill to the tile's distance from the body's edge; the band and the tile start below it.
#[test]
fn a_divider_is_a_pill_with_a_rule_to_each_side() {
    let mut fonts = FontSystem::new();
    let held = [by_ada(0, "alpha")];
    let body = body_of_drawer();
    for scale in [1.0_f32, 1.5] {
        let records: Vec<&ChatRecord> = held.iter().collect();
        let mut view = ChatView::default();
        view.mark = Some(held[0].ts.clone());
        view.layout(&mut fonts, &plain(&records), body, scale, NOW, chrono::FixedOffset::east_opt(0).unwrap());
        let quads = view.backs(DRAWER);
        let top = body.y + view.placed_tops()[0];
        let run = view.placed_buffers()[0].layout_runs().next().unwrap();
        let (first, last) = (run.glyphs.first().unwrap(), run.glyphs.last().unwrap());
        let text_x = body.x + GUTTER_PX * scale;
        let (left, right) = (text_x + first.x, text_x + last.x + last.w);
        assert!(near((left + right) / 2.0, text_x + (body.w - GUTTER_PX * scale) / 2.0), "centered in the text's width: {left} {right}");
        assert!(near(run.line_height, DAY_H * scale) && near(first.font_size, 13.0 * scale), "13/15 of the body on a line of its own height");
        // The pill: the border's rectangle, the ground inside it a pixel in, and the rules between the pill and the margins.
        let pill = quads.iter().find(|q| q.color == LOW && near(q.h, BODY_LINE_H * scale)).expect("the pill's border");
        assert!(near(pill.x, left) && near(pill.w, right - left) && near(pill.y + pill.h / 2.0, top + DAY_H * scale / 2.0), "{pill:?}");
        let inner = quads.iter().find(|q| q.color == (0x1A, 0x1D, 0x21) && q.w < DRAWER.w).expect("the ground inside");
        assert!(near(inner.x, pill.x + scale) && near(inner.y, pill.y + scale) && near(inner.w, pill.w - 2.0 * scale) && near(inner.h, pill.h - 2.0 * scale), "{inner:?}");
        let rules: Vec<_> = quads.iter().filter(|q| q.color == LOW && near(q.h, scale)).collect();
        assert_eq!(rules.len(), 2, "{rules:?}");
        assert!(near(rules[0].x, body.x + TILE_LEFT_PX * scale) && near(rules[0].x + rules[0].w, pill.x), "{:?}", rules[0]);
        assert!(near(rules[1].x, pill.x + pill.w) && near(rules[1].x + rules[1].w, body.x + body.w - TILE_LEFT_PX * scale), "{:?}", rules[1]);
        assert!(rules.iter().all(|r| near(r.y + r.h / 2.0, pill.y + pill.h / 2.0)));
        // The band, its bar and the tile start below the divider's line.
        let day = DAY_H * scale;
        let (band, bar) = (of_color(&quads, BAND)[0], of_color(&quads, SKY)[0]);
        assert!(near(band.y, top + day) && near(bar.y, top + day), "{band:?} {bar:?}");
        let tile = quads.iter().find(|q| TILES.contains(&q.color)).unwrap();
        assert!(near(tile.y, top + day + 2.0 * scale), "{tile:?}");
    }
}

/// A thread's summary is a line of its own: the count bold in the accent, then the last reply in the muted color, both 13/15.
#[test]
fn a_thread_summary_is_its_own_line_in_the_accent_and_the_muted_color() {
    let mut fonts = FontSystem::new();
    let mut r = by_ada(0, "alpha");
    r.reply_count = 2;
    r.latest_reply = Some(sot_protocol::chat::Ts::parse(&format!("{}.000100", 1_700_000_100 + 60)).unwrap());
    let mut view = ChatView::default();
    lay_drawer(&mut view, &mut fonts, &[r]);
    let buffers = view.placed_buffers();
    let run = buffers[0].layout_runs().find(|r| r.line_i == 3).expect("the summary's line after the divider, header and body");
    assert_eq!(run.text, "2 replies   Last reply today at 10:16 PM");
    let (count, rest) = (run.glyphs.first().unwrap(), run.glyphs.last().unwrap());
    assert_eq!(count.color_opt, Some(cosmic_text::Color::rgb(0x1D, 0x9B, 0xD1)));
    assert_eq!(rest.color_opt, Some(cosmic_text::Color::rgb(0x9A, 0x9C, 0x9E)));
    assert!(near(count.font_size, 13.0) && near(rest.font_size, 13.0) && near(run.line_height, BODY_LINE_H));
    assert_eq!(attrs(Kind::Replies, 1.0).weight, cosmic_text::Weight::BOLD);
    assert_ne!(attrs(Kind::Last, 1.0).weight, cosmic_text::Weight::BOLD);
}

/// A drawer too narrow for the label wraps it into more lines, and the tile and the band start below all of them.
#[test]
fn a_divider_wrapped_in_a_narrow_drawer_keeps_the_tile_below_it() {
    let mut fonts = FontSystem::new();
    let held = [by_ada(0, "alpha")];
    let records: Vec<&ChatRecord> = held.iter().collect();
    let mut view = ChatView::default();
    view.layout(&mut fonts, &plain(&records), ScreenRect { w: GUTTER_PX + 20.0, ..body_of_drawer() }, 1.0, NOW, chrono::FixedOffset::east_opt(0).unwrap());
    let lines: Vec<f32> = view.placed_buffers()[0].layout_runs().filter(|r| r.line_i == 0).map(|r| r.line_height).collect();
    assert!(lines.len() > 1, "the label wraps: {lines:?}");
    let quads = view.backs(DRAWER);
    let tile = quads.iter().find(|q| TILES.contains(&q.color)).unwrap();
    assert!(near(tile.y, body_of_drawer().y + view.placed_tops()[0] + lines.iter().sum::<f32>() + 2.0), "{tile:?} {lines:?}");
}
