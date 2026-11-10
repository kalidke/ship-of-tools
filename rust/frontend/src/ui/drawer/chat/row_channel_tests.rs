use std::time::{Duration, Instant};

use sot_protocol::ops::{ChatHistoryReq, ChatResolveRes, ChatWatchRes, ChatWatchState};

use super::drawer::ChatDrawer;
use super::owner::{Fill, Out};
use crate::net::dial::HostKey;
use crate::net::transport::{ChatEvt, ChatReply, OutgoingReq};
use crate::ui::ResultRowIdentity;

fn host() -> HostKey {
    "a".to_string()
}

fn wanted(row: &str) -> Option<ResultRowIdentity> {
    Some(ResultRowIdentity { host: host(), workspace_id: row.to_string() })
}

pub(super) fn answer(row: &str, json: serde_json::Value) -> ChatEvt {
    let res: ChatResolveRes = serde_json::from_value(json).unwrap();
    ChatEvt::Resolved { row: row.to_string(), reply: ChatReply::Done(res) }
}

fn mapped_to(row: &str, channel: &str) -> ChatEvt {
    answer(row, serde_json::json!({ "state": "mapped", "workspace": "T1", "channel": channel, "delivers": false }))
}

pub(super) fn mapped(row: &str) -> ChatEvt {
    mapped_to(row, "C1")
}

fn complete_to(row: &str, channel: &str) -> ChatEvt {
    let target = serde_json::from_value(serde_json::json!({ "row": row, "workspace": "T1", "channel": channel })).unwrap();
    ChatEvt::Watched { target, reply: ChatReply::Done(ChatWatchRes { state: ChatWatchState::Complete }) }
}

fn complete(row: &str) -> ChatEvt {
    complete_to(row, "C1")
}

fn count(outs: &[Out], f: fn(&OutgoingReq) -> bool) -> usize {
    outs.iter().filter(|(_, r)| f(r)).count()
}

fn resolves(outs: &[Out]) -> usize {
    count(outs, |r| matches!(r, OutgoingReq::ChatResolve { .. }))
}

fn watches(outs: &[Out]) -> usize {
    count(outs, |r| matches!(r, OutgoingReq::ChatWatch { .. }))
}

fn unwatches(outs: &[Out]) -> usize {
    count(outs, |r| matches!(r, OutgoingReq::ChatUnwatch { .. }))
}

fn history(outs: &[Out]) -> ChatHistoryReq {
    match outs {
        [(_, OutgoingReq::ChatHistory { req })] => req.clone(),
        other => panic!("one history read expected, got {} requests", other.len()),
    }
}

fn page() -> ChatReply<sot_protocol::ops::ChatHistoryRes> {
    ChatReply::Done(serde_json::from_value(serde_json::json!({ "records": [], "next": null })).unwrap())
}

/// The window watches the channel of the row the drawer shows, and only while it shows.
#[test]
fn the_watch_follows_the_shown_rows_channel_and_ends_with_it() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, t0);

    // Row R shown: nothing inside the debounce (the pacer is asked back), one resolve after it, `mapped` gives one watch.
    let (outs, settling) = d.tick(wanted("R"), t0);
    assert!(outs.is_empty() && settling);
    let (outs, settling) = d.tick(wanted("R"), at(150));
    assert!(!settling);
    assert_eq!((resolves(&outs), outs.len()), (1, 1), "one resolve after the debounce");
    assert_eq!(watches(&d.on_evt(&host(), mapped("R"), at(160))), 1);

    // Help opened from Chat keeps the wanted row: nothing. The REPL opening over that Help ends the watch.
    assert!(d.tick(wanted("R"), at(200)).0.is_empty());
    assert_eq!(d.fill(), Some(Fill::Needed));
    assert_eq!(unwatches(&d.tick(None, at(300)).0), 1);
    assert_eq!(d.fill(), None, "nothing is watched once the chat is not shown");
    assert!(d.tick(None, at(350)).0.is_empty());

    // Chat again: one resolve, one watch. Hidden again: one unwatch.
    assert!(d.tick(wanted("R"), at(400)).0.is_empty());
    assert_eq!(resolves(&d.tick(wanted("R"), at(550)).0), 1);
    assert_eq!(watches(&d.on_evt(&host(), mapped("R"), at(560))), 1);
    assert_eq!(unwatches(&d.tick(None, at(600)).0), 1);
    assert_eq!(d.fill(), None);

    // Rows passed within the debounce are never resolved.
    assert!(d.tick(wanted("S"), at(700)).0.is_empty());
    assert!(d.on_evt(&host(), mapped("R"), at(710)).is_empty(), "a late answer for a row no longer wanted starts nothing");
    assert_eq!(d.fill(), None);
    assert!(d.tick(wanted("T"), at(800)).0.is_empty());
    assert!(d.tick(wanted("T"), at(900)).0.is_empty());
    assert!(d.tick(None, at(950)).0.is_empty());

    // R again.
    d.tick(wanted("R"), at(1000));
    assert_eq!(resolves(&d.tick(wanted("R"), at(1150)).0), 1);
    assert_eq!(watches(&d.on_evt(&host(), mapped("R"), at(1160))), 1);

    // The link goes down and up: one resolve, and a second `mapped` for it changes nothing.
    assert!(d.on_connection(&host(), false, at(1500)).is_empty());
    assert!(d.tick(wanted("R"), at(1510)).0.is_empty(), "no resolve on a host that is down");
    d.on_connection(&host(), true, at(1520));
    assert_eq!(resolves(&d.tick(wanted("R"), at(1530)).0), 1);
    assert!(d.tick(wanted("R"), at(1540)).0.is_empty());
    assert!(d.on_evt(&host(), mapped("R"), at(1550)).is_empty());
    assert!(d.on_evt(&host(), mapped("R"), at(1560)).is_empty());

    // The shown row's answer decides its watch: after a link returns, `unmapped` ends the watch the owner took up again.
    d.on_connection(&host(), false, at(1600));
    assert_eq!(watches(&d.on_connection(&host(), true, at(1610))), 1);
    assert_eq!(resolves(&d.tick(wanted("R"), at(1620)).0), 1);
    let unmapped = answer("R", serde_json::json!({ "state": "unmapped" }));
    assert_eq!(unwatches(&d.on_evt(&host(), unmapped, at(1630))), 1);
    assert_eq!(d.fill(), None, "nothing stays watched");
}

/// A read from before a re-watch installs nothing in the new watch: the new watch's equal read waits for the old reply.
#[test]
fn a_read_from_before_a_rewatch_never_fills_the_new_watch() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, t0);
    d.tick(wanted("R"), t0);
    assert_eq!(resolves(&d.tick(wanted("R"), at(150)).0), 1);
    d.on_evt(&host(), mapped("R"), at(160));
    let old = history(&d.on_evt(&host(), complete("R"), at(170)));
    assert_eq!(unwatches(&d.tick(None, at(200)).0), 1);
    d.tick(wanted("R"), at(300));
    assert_eq!(resolves(&d.tick(wanted("R"), at(450)).0), 1);
    assert_eq!(watches(&d.on_evt(&host(), mapped("R"), at(460))), 1);
    assert!(d.on_evt(&host(), complete("R"), at(470)).is_empty(), "an equal read waits for the old reply");
    let sent = d.on_evt(&host(), ChatEvt::History { req: old.clone(), reply: page() }, at(480));
    assert_eq!(history(&sent), old, "then one read goes out");
    assert_eq!(d.fill(), Some(Fill::Reading), "the old reply installed nothing");
    assert!(d.on_evt(&host(), ChatEvt::History { req: old, reply: page() }, at(490)).is_empty());
    assert_eq!(d.fill(), Some(Fill::Complete));
}

fn answered(req: &ChatHistoryReq) -> ChatEvt {
    ChatEvt::History { req: req.clone(), reply: page() }
}

fn host_b() -> HostKey {
    "b".to_string()
}

fn wanted_on(host: HostKey, row: &str) -> Option<ResultRowIdentity> {
    Some(ResultRowIdentity { host, workspace_id: row.to_string() })
}

/// Row `row` shown on `host` from `from` ms, resolved, mapped to `channel` and complete: the snapshot read in flight.
fn reading_on(d: &mut ChatDrawer, host: &HostKey, t0: Instant, from: u64, row: &str, channel: &str) -> ChatHistoryReq {
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    d.tick(wanted_on(host.clone(), row), at(from));
    d.tick(wanted_on(host.clone(), row), at(from + 150));
    d.on_evt(host, mapped_to(row, channel), at(from + 160));
    history(&d.on_evt(host, complete_to(row, channel), at(from + 170)))
}

/// R shown on the first host from `from` ms: the snapshot read in flight.
fn reading(d: &mut ChatDrawer, t0: Instant, from: u64, channel: &str) -> ChatHistoryReq {
    reading_on(d, &host(), t0, from, "R", channel)
}

/// A link change clears the host's reads in flight: fe-net ends a read with the connection and sends no reply, so an entry
/// left behind would hold the reconnected watch's equal read for good.
#[test]
fn a_link_change_clears_the_reads_in_flight() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, t0);
    reading(&mut d, t0, 0, "C1");
    d.on_connection(&host(), false, at(300));
    d.on_connection(&host(), true, at(310));
    let outs = d.on_evt(&host(), complete("R"), at(320));
    assert_eq!(count(&outs, |r| matches!(r, OutgoingReq::ChatHistory { .. })), 1, "not held behind the dropped read");
}

/// A held read whose reader gave it up is cancelled, never sent: when the first read is answered, the second watch's
/// held read stays unsent, and the third watch's own read fills the third watch alone.
#[test]
fn a_held_read_given_up_is_cancelled_and_never_sent() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, t0);
    let first = reading(&mut d, t0, 0, "C1");
    d.tick(None, at(200));
    d.tick(wanted("R"), at(300));
    d.tick(wanted("R"), at(450));
    d.on_evt(&host(), mapped("R"), at(460));
    assert!(d.on_evt(&host(), complete("R"), at(470)).is_empty(), "the second watch's read is held");
    d.tick(None, at(500));
    d.tick(wanted("R"), at(600));
    d.tick(wanted("R"), at(750));
    d.on_evt(&host(), mapped("R"), at(760));
    assert!(d.on_evt(&host(), answered(&first), at(770)).is_empty(), "the cancelled read is not sent");
    let third = history(&d.on_evt(&host(), complete("R"), at(780)));
    assert!(d.on_evt(&host(), answered(&third), at(790)).is_empty());
    assert_eq!(d.fill(), Some(Fill::Complete));
}

/// Whatever ends or replaces the watch gives its read up, in the drawer's one place: an answer that is not `mapped`, and a
/// `mapped` answer for another channel. The old read's reply installs nothing in the watch that follows.
#[test]
fn an_answer_that_ends_or_replaces_the_watch_orphans_its_read() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    for ended in [true, false] {
        let mut d = ChatDrawer::default();
        d.on_connection(&host(), true, t0);
        let old = reading(&mut d, t0, 0, "C1");
        if ended {
            d.on_evt(&host(), answer("R", serde_json::json!({ "state": "unmapped" })), at(200));
            assert_eq!(d.fill(), None);
            d.on_evt(&host(), mapped("R"), at(210));
        } else {
            d.on_evt(&host(), mapped_to("R", "C2"), at(200));
            d.on_evt(&host(), mapped("R"), at(210));
        }
        assert!(d.on_evt(&host(), complete("R"), at(220)).is_empty(), "held behind the orphan (ended: {ended})");
        assert_eq!(history(&d.on_evt(&host(), answered(&old), at(230))), old);
        assert_eq!(d.fill(), Some(Fill::Reading), "the old reply installed nothing (ended: {ended})");
    }
}

/// A frame between the snapshot read and its reply keeps the read: the owner still holds it, so it is not given up, and its
/// reply fills the watch.
#[test]
fn a_frame_during_the_read_keeps_it() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, t0);
    let read = reading(&mut d, t0, 0, "C1");
    d.tick(wanted("R"), at(186));
    assert!(d.on_evt(&host(), answered(&read), at(200)).is_empty());
    assert_eq!(d.fill(), Some(Fill::Complete), "the read's reply fills after a frame");
}

/// Another host's link change leaves this host's reads in flight: the orphan's reply is still dropped, and the held read
/// goes after it.
#[test]
fn another_hosts_link_change_keeps_this_hosts_reads_in_flight() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, t0);
    d.on_connection(&host_b(), true, t0);
    let old = reading(&mut d, t0, 0, "C1");
    d.tick(None, at(200));
    d.tick(wanted("R"), at(300));
    d.tick(wanted("R"), at(450));
    d.on_evt(&host(), mapped("R"), at(460));
    assert!(d.on_evt(&host(), complete("R"), at(470)).is_empty(), "held behind the orphan");
    d.on_connection(&host_b(), false, at(500));
    d.on_connection(&host_b(), true, at(510));
    let sent = d.on_evt(&host(), answered(&old), at(520));
    assert_eq!(d.fill(), Some(Fill::Reading), "the old reply installed nothing");
    assert_eq!(history(&sent), old, "then the held read goes");
}

/// A link change cancels that host's held read: it never goes out later, when another host's reply releases the held ones,
/// and a return to the row reads at once.
#[test]
fn a_link_change_cancels_that_hosts_held_read() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, t0);
    d.on_connection(&host_b(), true, t0);
    let old = reading(&mut d, t0, 0, "C1");
    d.tick(None, at(200));
    d.tick(wanted("R"), at(300));
    d.tick(wanted("R"), at(450));
    d.on_evt(&host(), mapped("R"), at(460));
    assert!(d.on_evt(&host(), complete("R"), at(470)).is_empty(), "held behind the orphan");
    d.on_connection(&host(), false, at(600));
    d.on_connection(&host(), true, at(610));
    let b_read = reading_on(&mut d, &host_b(), t0, 700, "S", "C1");
    assert!(d.on_evt(&host_b(), answered(&b_read), at(880)).is_empty(), "no stray read of the old watch goes out");
    d.tick(wanted("R"), at(900));
    d.tick(wanted("R"), at(1050));
    d.on_evt(&host(), mapped("R"), at(1060));
    assert_eq!(history(&d.on_evt(&host(), complete("R"), at(1070))), old, "the re-watch's read goes at once");
}

/// A page of `n` top-level records of channel `channel` (times 100.000001 and up), the whole channel.
fn page_of(channel: &str, n: u64) -> ChatReply<sot_protocol::ops::ChatHistoryRes> {
    let records: Vec<serde_json::Value> = (0..n).map(|i| serde_json::json!({ "workspace": "T1", "channel": channel, "ts": format!("{}.000001", 100 + i),
        "thread_ts": format!("{}.000001", 100 + i), "author": { "id": "U1", "name": "A" }, "origin": "member", "text": "x", "reply_count": 0, "files": [] })).collect();
    ChatReply::Done(serde_json::from_value(serde_json::json!({ "records": records })).unwrap())
}

/// A channel shown a moment ago draws what the window held for it as soon as the hub answers the row's resolve with it,
/// while its snapshot is read again, which replaces them; another channel never shows them, and the owner keeps two.
#[test]
fn a_switch_back_draws_the_channels_records_while_it_is_read_again() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, t0);
    let shown = |d: &mut ChatDrawer, from: u64, row: &str, channel: Option<&str>| {
        d.tick(wanted(row), at(from));
        assert_eq!(resolves(&d.tick(wanted(row), at(from + 150)).0), 1);
        let evt = channel.map_or_else(|| answer(row, serde_json::json!({ "state": "unmapped" })), |c| mapped_to(row, c));
        d.on_evt(&host(), evt, at(from + 160))
    };
    shown(&mut d, 0, "R", Some("C1"));
    let read = history(&d.on_evt(&host(), complete_to("R", "C1"), at(170)));
    d.on_evt(&host(), ChatEvt::History { req: read, reply: page_of("C1", 3) }, at(180));
    assert_eq!(d.held(), 3);

    // Another row: nothing of R's channel shows, mapped or not.
    assert_eq!(unwatches(&d.tick(wanted("S"), at(1000)).0), 1);
    assert_eq!(d.held(), 0);
    shown(&mut d, 1000, "S", None);
    assert_eq!(d.held(), 0, "an unmapped row shows nothing");
    shown(&mut d, 2000, "T", Some("C2"));
    assert_eq!(d.held(), 0, "another channel never shows them");

    // Back to R: its records at the hub's answer, before any read, under its channel and the recovery's words.
    assert_eq!(watches(&shown(&mut d, 3000, "R", Some("C1"))), 1);
    assert_eq!(d.held(), 3, "a switch back draws what was held at once");
    assert_eq!(d.fill(), Some(Fill::Needed));
    assert_eq!(d.title(), " chat · C1 reading… ");
    let read = history(&d.on_evt(&host(), complete_to("R", "C1"), at(3170)));
    d.on_evt(&host(), ChatEvt::History { req: read, reply: page_of("C1", 1) }, at(3180));
    assert_eq!((d.held(), d.fill()), (1, Some(Fill::Complete)), "the snapshot replaces them");

    // The same channel through another row of the same folder takes them up too.
    shown(&mut d, 4000, "S", None);
    shown(&mut d, 5000, "R2", Some("C1"));
    assert_eq!(d.held(), 1, "the records are the channel's");

    // Two are kept: passing through C3 and C4 pushes C2 out.
    shown(&mut d, 6000, "U", Some("C3"));
    shown(&mut d, 7000, "V", Some("C4"));
    shown(&mut d, 8000, "T", Some("C2"));
    assert_eq!(d.held(), 0, "the oldest parked channel is gone");
}

/// The frame asks for another one while a first show's watch waits on its short paces, which is what wakes an idle window
/// for the second ask, and stops asking once the watch is complete.
#[test]
fn a_frame_is_asked_for_while_a_first_watch_waits_its_short_paces() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, t0);
    d.tick(wanted("R"), t0);
    assert_eq!(resolves(&d.tick(wanted("R"), at(150)).0), 1);
    assert_eq!(watches(&d.on_evt(&host(), mapped_to("R", "C1"), at(160))), 1);
    let (outs, again) = d.tick(wanted("R"), at(170));
    assert!(outs.is_empty() && again, "the answer is awaited, and a frame is wanted for the second ask");
    let (outs, again) = d.tick(wanted("R"), at(410));
    assert_eq!((watches(&outs), again), (1, true), "the second ask goes at 250 ms and the third is still to come");
    let (outs, again) = d.tick(wanted("R"), at(1160));
    assert_eq!((watches(&outs), again), (1, false), "the third goes at one second, and no frame is wanted after it");
}

/// A page read for the channel a row left is an orphan: asked again after a hide and show, the same page waits for the first
/// one's reply, and the first reply fills nothing.
#[test]
fn a_page_read_again_after_a_hide_and_show_waits_for_the_first_reply() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, t0);
    let snapshot = reading(&mut d, t0, 0, "C1");
    let reply = serde_json::json!({ "records": [{ "workspace": "T1", "channel": "C1", "ts": "5.000001", "thread_ts": "5.000001",
        "author": { "id": "U1", "name": "A" }, "origin": "member", "text": "x", "reply_count": 0, "files": [] }], "next": "older" });
    let snap = ChatEvt::History { req: snapshot, reply: ChatReply::Done(serde_json::from_value(reply).unwrap()) };
    d.on_evt(&host(), snap, at(180));
    d.reach_oldest(true);
    let (outs, _) = d.tick(wanted("R"), at(190));
    let first = history(&outs);
    d.tick(None, at(200));
    d.tick(wanted("R"), at(300));
    d.tick(wanted("R"), at(450));
    d.on_evt(&host(), mapped("R"), at(460));
    let snapshot = history(&d.on_evt(&host(), complete("R"), at(470)));
    let reply = serde_json::json!({ "records": [{ "workspace": "T1", "channel": "C1", "ts": "5.000001", "thread_ts": "5.000001",
        "author": { "id": "U1", "name": "A" }, "origin": "member", "text": "x", "reply_count": 0, "files": [] }], "next": "older" });
    let snap = ChatEvt::History { req: snapshot, reply: ChatReply::Done(serde_json::from_value(reply).unwrap()) };
    assert!(d.on_evt(&host(), snap, at(480)).is_empty());
    d.reach_oldest(true);
    assert!(d.tick(wanted("R"), at(490)).0.is_empty(), "the same page waits for the first one's reply");
    let late = ChatEvt::History { req: first.clone(), reply: ChatReply::Done(serde_json::from_value(serde_json::json!({ "records": [{ "workspace": "T1", "channel": "C1",
        "ts": "4.000001", "thread_ts": "4.000001", "author": { "id": "U1", "name": "A" }, "origin": "member", "text": "x", "reply_count": 0, "files": [] }] })).unwrap()) };
    assert_eq!(history(&d.on_evt(&host(), late, at(500))), first, "then it goes");
    assert_eq!(d.held(), 1, "and the first reply fills nothing");
}
