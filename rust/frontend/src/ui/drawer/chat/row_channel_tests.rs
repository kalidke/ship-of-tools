use std::time::{Duration, Instant};

use sot_protocol::ops::{ChatCode, ChatHistoryReq, ChatResolveRes, ChatService, ChatStatusEvt, ChatWatchRes, ChatWatchState};

use super::drawer::ChatDrawer;
use super::owner::{Fill, Out, PARKED};
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
    d.on_connection(&host(), true, false, t0);

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
    assert!(d.on_connection(&host(), false, false, at(1500)).is_empty());
    assert!(d.tick(wanted("R"), at(1510)).0.is_empty(), "no resolve on a host that is down");
    d.on_connection(&host(), true, false, at(1520));
    assert_eq!(resolves(&d.tick(wanted("R"), at(1530)).0), 1);
    assert!(d.tick(wanted("R"), at(1540)).0.is_empty());
    assert!(d.on_evt(&host(), mapped("R"), at(1550)).is_empty());
    assert!(d.on_evt(&host(), mapped("R"), at(1560)).is_empty());

    // The shown row's answer decides its watch: after a link returns, `unmapped` ends the watch the owner took up again.
    d.on_connection(&host(), false, false, at(1600));
    assert_eq!(watches(&d.on_connection(&host(), true, false, at(1610))), 1);
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
    d.on_connection(&host(), true, false, t0);
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
    d.on_connection(&host(), true, false, t0);
    reading(&mut d, t0, 0, "C1");
    d.on_connection(&host(), false, false, at(300));
    d.on_connection(&host(), true, false, at(310));
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
    d.on_connection(&host(), true, false, t0);
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
        d.on_connection(&host(), true, false, t0);
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
    d.on_connection(&host(), true, false, t0);
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
    d.on_connection(&host(), true, false, t0);
    d.on_connection(&host_b(), true, false, t0);
    let old = reading(&mut d, t0, 0, "C1");
    d.tick(None, at(200));
    d.tick(wanted("R"), at(300));
    d.tick(wanted("R"), at(450));
    d.on_evt(&host(), mapped("R"), at(460));
    assert!(d.on_evt(&host(), complete("R"), at(470)).is_empty(), "held behind the orphan");
    d.on_connection(&host_b(), false, false, at(500));
    d.on_connection(&host_b(), true, false, at(510));
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
    d.on_connection(&host(), true, false, t0);
    d.on_connection(&host_b(), true, false, t0);
    let old = reading(&mut d, t0, 0, "C1");
    d.tick(None, at(200));
    d.tick(wanted("R"), at(300));
    d.tick(wanted("R"), at(450));
    d.on_evt(&host(), mapped("R"), at(460));
    assert!(d.on_evt(&host(), complete("R"), at(470)).is_empty(), "held behind the orphan");
    d.on_connection(&host(), false, false, at(600));
    d.on_connection(&host(), true, false, at(610));
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
/// while its snapshot is read again, which replaces them; another channel never shows them, and the owner keeps `PARKED` channels.
#[test]
fn a_switch_back_draws_the_channels_records_while_it_is_read_again() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, false, t0);
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

    // The owner keeps `PARKED` channels: that many more, each read with a record, push the first (C1) out.
    for k in 0..=PARKED {
        let (row, channel) = (format!("X{k}"), format!("D{k}"));
        let from = 6000 + 1000 * k as u64;
        shown(&mut d, from, &row, Some(&channel));
        let read = history(&d.on_evt(&host(), complete_to(&row, &channel), at(from + 170)));
        d.on_evt(&host(), ChatEvt::History { req: read, reply: page_of(&channel, 1) }, at(from + 180));
        assert_eq!(d.held(), 1);
    }
    shown(&mut d, 20000, "R", Some("C1"));
    assert_eq!(d.held(), 0, "the oldest parked channel is gone");
    shown(&mut d, 21000, "X7", Some("D7"));
    assert_eq!(d.held(), 1, "the newer ones stay");
}

/// Row R shown, resolved, mapped to `C1` (named `name` when given) and read with `n` records, then another row shown: the state
/// of a session the person has left. The instant its time is counted from.
fn away_from_r(n: u64, name: Option<&str>) -> (ChatDrawer, Instant) {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, false, t0);
    d.tick(wanted("R"), at(0));
    d.tick(wanted("R"), at(150));
    d.on_evt(&host(), name.map_or_else(|| mapped("R"), |name| named_to("R", "C1", name)), at(160));
    let read = history(&d.on_evt(&host(), complete("R"), at(170)));
    d.on_evt(&host(), ChatEvt::History { req: read, reply: page_of("C1", n) }, at(180));
    assert_eq!(d.held(), n as usize);
    d.tick(wanted("S"), at(1000));
    assert_eq!(d.held(), 0, "another row shows nothing of R's");
    (d, t0)
}

/// A row switched back to draws its channel's last records at the first frame, before its resolve is answered, under the title
/// that says they are read again; the answer keeps them, and the snapshot replaces them.
#[test]
fn a_row_switched_back_to_draws_its_last_records_on_the_first_frame() {
    let (mut d, t0) = away_from_r(3, Some("general"));
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let (outs, settling) = d.tick(wanted("R"), at(2000));
    assert!(outs.is_empty() && settling, "the first frame sends nothing");
    assert_eq!(d.held(), 3, "its records are drawn at the first frame");
    assert_eq!(d.title(), " chat · #general reading… ");
    assert_eq!(d.fill(), None, "nothing is watched yet");
    // The debounce passes, the resolve alone goes, and the records stand until it is answered.
    let (outs, _) = d.tick(wanted("R"), at(2150));
    assert_eq!((resolves(&outs), outs.len()), (1, 1));
    assert_eq!(d.held(), 3);
    assert_eq!(watches(&d.on_evt(&host(), named_to("R", "C1", "general"), at(2160))), 1, "the answer starts the watch");
    assert_eq!((d.held(), d.title()), (3, " chat · #general reading… ".to_string()), "the watch takes the records up");
    let read = history(&d.on_evt(&host(), complete("R"), at(2170)));
    d.on_evt(&host(), ChatEvt::History { req: read, reply: page_of("C1", 1) }, at(2180));
    assert_eq!((d.held(), d.fill()), (1, Some(Fill::Complete)), "the snapshot replaces them");
    assert_eq!(d.title(), " chat · #general ");
    // A row never shown has nothing to draw at its first frame.
    d.tick(wanted("T"), at(3000));
    assert_eq!(d.held(), 0);
}

/// A resolve that names another channel, none, or refuses removes the drawn records at once; they stay parked under their own
/// channel; a link that goes takes them as well.
#[test]
fn an_answer_that_is_not_the_remembered_channel_removes_the_drawn_records() {
    let at_first = |d: &mut ChatDrawer, t0: Instant| {
        assert!(d.tick(wanted("R"), t0 + Duration::from_millis(2000)).0.is_empty());
        assert_eq!(d.held(), 3);
        assert_eq!(resolves(&d.tick(wanted("R"), t0 + Duration::from_millis(2150)).0), 1);
        t0 + Duration::from_millis(2160)
    };
    // Another channel: paired again while away.
    let (mut d, t0) = away_from_r(3, None);
    let now = at_first(&mut d, t0);
    assert_eq!(watches(&d.on_evt(&host(), mapped_to("R", "C9"), now)), 1);
    assert_eq!(d.held(), 0, "the old channel's records are gone at the answer");
    assert_eq!(d.title(), " chat · C9 reading… ");
    // They are parked under C1 still: another row mapped to C1 takes them up.
    d.tick(wanted("Q"), now + Duration::from_millis(100));
    d.tick(wanted("Q"), now + Duration::from_millis(300));
    d.on_evt(&host(), mapped_to("Q", "C1"), now + Duration::from_millis(310));
    assert_eq!(d.held(), 3, "parked again under their own channel");

    // Unpaired while away; and the row is no longer remembered.
    let (mut d, t0) = away_from_r(3, None);
    let now = at_first(&mut d, t0);
    d.on_evt(&host(), answer("R", serde_json::json!({ "state": "unmapped" })), now);
    assert_eq!((d.held(), d.title()), (0, " chat · no channel ".to_string()));
    d.tick(wanted("S"), now + Duration::from_millis(100));
    d.tick(wanted("R"), now + Duration::from_millis(200));
    assert_eq!(d.held(), 0, "an unmapped row's channel is forgotten");

    // Refused: what it shows today, no old records under it.
    let (mut d, t0) = away_from_r(3, None);
    let now = at_first(&mut d, t0);
    d.on_evt(&host(), ChatEvt::Resolved { row: "R".into(), reply: ChatReply::Refused(ChatCode::StaleRow) }, now);
    assert_eq!((d.held(), d.title()), (0, format!(" chat · {} ", ChatCode::StaleRow.text())));

    // Lost: the link went before the answer.
    let (mut d, t0) = away_from_r(3, None);
    let now = at_first(&mut d, t0);
    d.on_connection(&host(), false, false, now);
    assert_eq!((d.held(), d.title()), (0, " chat · no connection to a ".to_string()));
    // The link's return does not bring them back before the answer: they are what the last link's hub said.
    d.on_connection(&host(), true, false, now + Duration::from_millis(100));
    assert_eq!(d.held(), 0);
}

/// A row left before its first read finished parks nothing, but its answer is remembered: shown again, the drawer draws no
/// records, takes typing into a draft in the remembered channel, and the title names that channel, so a person never types
/// where nothing on screen names it.
#[test]
fn the_title_names_the_channel_a_draft_is_typed_into_before_the_answer() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, false, t0);
    d.tick(wanted("R"), at(0));
    d.tick(wanted("R"), at(150));
    d.on_evt(&host(), answer("R", serde_json::json!({ "state": "mapped", "workspace": "T1", "channel": "C2", "delivers": false, "name": "general", "workspace_name": "Lab" })), at(160));
    d.tick(wanted("S"), at(1000));
    d.tick(wanted("R"), at(2000));
    assert_eq!(d.held(), 0, "the channel parked nothing");
    assert_eq!(d.edit(wanted("R").as_ref(), super::compose::Edit::Text("typed")), Ok(()));
    assert_eq!(d.draft(), Some(("typed", "C2")));
    assert_eq!(d.title(), " chat · Lab · #general reading… ", "the title names the channel the draft is in, and its workspace");
}

/// The remembered answers are as many as the parked channels, keyed by host and row, and dropped by a row that closes and by a
/// link change; none is drawn from another host's row of the same id.
#[test]
fn the_remembered_answers_are_bounded_and_keyed_by_host_and_row() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, false, t0);
    // PARKED + 1 rows, each read with a record: X0 is pushed out of the memory, though its channel is still parked.
    for k in 0..=PARKED {
        let (row, channel) = (format!("X{k}"), format!("D{k}"));
        let from = 1000 * k as u64;
        d.tick(wanted(&row), at(from));
        d.tick(wanted(&row), at(from + 150));
        d.on_evt(&host(), mapped_to(&row, &channel), at(from + 160));
        let read = history(&d.on_evt(&host(), complete_to(&row, &channel), at(from + 170)));
        d.on_evt(&host(), ChatEvt::History { req: read, reply: page_of(&channel, 1) }, at(from + 180));
    }
    let first_frame = |d: &mut ChatDrawer, row: Option<ResultRowIdentity>, ms: u64| {
        d.tick(row, at(100_000 + ms));
        d.held()
    };
    assert_eq!(first_frame(&mut d, wanted("X0"), 0), 0, "the oldest row is no longer remembered");
    assert_eq!(first_frame(&mut d, wanted("X1"), 1000), 1);
    // The same row id on another host is another row, though that host's own channel of the same id is parked.
    d.on_connection(&host_b(), true, false, at(100_000));
    let read = reading_on(&mut d, &host_b(), t0, 100_500, "Y", "D2");
    d.on_evt(&host_b(), ChatEvt::History { req: read, reply: page_of("D2", 1) }, at(100_700));
    assert_eq!(d.held(), 1);
    assert_eq!(first_frame(&mut d, wanted_on(host_b(), "X2"), 2000), 0);
    // A row that closes is forgotten.
    d.retire(&[ResultRowIdentity { host: host(), workspace_id: "X3".into() }]);
    assert_eq!(first_frame(&mut d, wanted("X3"), 3000), 0);
    assert_eq!(first_frame(&mut d, wanted("X4"), 4000), 1);
    // A link change forgets them all; the channel's records are still parked, and the hub's answer takes them up.
    d.on_connection(&host(), false, false, at(105_000));
    d.on_connection(&host(), true, false, at(105_100));
    assert_eq!(first_frame(&mut d, wanted("X5"), 5200), 0);
    d.tick(wanted("X5"), at(105_400));
    d.on_evt(&host(), mapped_to("X5", "D5"), at(105_410));
    assert_eq!(d.held(), 1);
}

/// While a first show's watch waits on its short pace the drawer names the instant of the next ask, which the window
/// sleeps to, drawing no frame before it; none while an ask's reply is awaited, and none once the last short ask has gone.
/// The answer names the channel, so the row's own retry for a name does not stand in for the owner's.
#[test]
fn the_drawer_names_the_instant_of_a_first_watchs_next_ask() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let pending = || {
        let target = serde_json::from_value(serde_json::json!({ "row": "R", "workspace": "T1", "channel": "C1" })).unwrap();
        ChatEvt::Watched { target, reply: ChatReply::Done(ChatWatchRes { state: ChatWatchState::Pending }) }
    };
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, false, t0);
    d.tick(wanted("R"), t0);
    assert_eq!(resolves(&d.tick(wanted("R"), at(150)).0), 1);
    assert_eq!(d.next_ask(), None, "nothing watched before the answer");
    assert_eq!(watches(&d.on_evt(&host(), named_to("R", "C1", "general"), at(160))), 1);
    assert_eq!(d.next_ask(), None, "the first watch's reply is awaited");
    d.on_evt(&host(), pending(), at(170));
    assert_eq!(d.next_ask(), Some(at(260)), "100 ms after the first watch");
    assert!(d.tick(wanted("R"), at(200)).0.is_empty(), "a frame before it asks nothing");
    assert_eq!(watches(&d.tick(wanted("R"), at(260)).0), 1);
    d.on_evt(&host(), pending(), at(270));
    assert_eq!(d.next_ask(), Some(at(360)), "and each ask after the pace of the last");
    // Each ask after that is named by the drawer, whatever its gap: 21 more to the last, 22 s after the first, where the
    // recovery is 20 s old and the pace is `PACE`.
    let mut asks = 2;
    while let Some(next) = d.next_ask() {
        assert_eq!(watches(&d.tick(wanted("R"), next).0), 1);
        d.on_evt(&host(), pending(), next);
        asks += 1;
    }
    assert_eq!(asks, 23, "the owner's schedule: 12 asks in the first 2 s, 23 to the window's end");
    assert_eq!(d.next_ask(), None, "from the last ask on the pace is PACE and nothing wakes the window");
}

/// A page read for the channel a row left is an orphan: asked again after a hide and show, the same page waits for the first
/// one's reply, and the first reply fills nothing.
#[test]
fn a_page_read_again_after_a_hide_and_show_waits_for_the_first_reply() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, false, t0);
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

fn named_to(row: &str, channel: &str, name: &str) -> ChatEvt {
    answer(row, serde_json::json!({ "state": "mapped", "workspace": "T1", "channel": channel, "delivers": false, "name": name }))
}

/// The title says `#name` when an answer for the wanted row named the watched channel, and the id when none did: an older
/// daemon's answer, or a name for another channel. A later answer without a name for the same channel keeps it (a new Slack
/// app outside a private channel cannot read its name).
#[test]
fn the_title_names_the_channel_when_the_answer_did_and_says_the_id_otherwise() {
    let t0 = Instant::now();
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, false, t0);
    d.tick(wanted("R"), t0);
    d.tick(wanted("R"), t0 + Duration::from_millis(150));
    d.on_evt(&host(), mapped("R"), t0 + Duration::from_millis(160));
    assert_eq!(d.title(), " chat · C1 reading… ", "an answer with no name: the id");

    d.on_evt(&host(), named_to("R", "C1", "general"), t0 + Duration::from_millis(200));
    assert_eq!(d.title(), " chat · #general reading… ");
    d.on_evt(&host(), named_to("R", "C1", "ge\u{7}ne\nral\u{2028}x"), t0 + Duration::from_millis(210));
    assert_eq!(d.title(), " chat · #ge ne ⏎ ral ⏎ x reading… ", "the daemon's text reaches the cells through `cell_row`");

    // A later answer with no name for the same channel keeps the name; one for another channel says its id.
    d.on_evt(&host(), mapped("R"), t0 + Duration::from_millis(220));
    assert_eq!(d.title(), " chat · #ge ne ⏎ ral ⏎ x reading… ");
    d.on_evt(&host(), mapped_to("R", "C2"), t0 + Duration::from_millis(230));
    assert_eq!(d.title(), " chat · C2 reading… ");
}

/// A hub serving several workspaces names the row's in the title, ahead of the channel; an answer with no workspace name
/// (one workspace, or an older daemon) leaves the title as it was, and a later answer without it takes the name away.
#[test]
fn the_title_names_the_workspace_when_the_hub_does() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, false, t0);
    d.tick(wanted("R"), t0);
    d.tick(wanted("R"), at(150));
    let json = |extra: serde_json::Value| {
        let mut j = serde_json::json!({ "state": "mapped", "workspace": "T1", "channel": "C1", "delivers": false, "name": "general" });
        j.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        answer("R", j)
    };
    d.on_evt(&host(), json(serde_json::json!({})), at(160));
    assert_eq!(d.title(), " chat · #general reading… ", "no workspace name: today's title");
    d.on_evt(&host(), json(serde_json::json!({ "workspace_name": "Lab" })), at(200));
    assert_eq!(d.title(), " chat · Lab · #general reading… ");
    d.on_evt(&host(), json(serde_json::json!({ "workspace_name": "La\nb" })), at(210));
    assert_eq!(d.title(), " chat · La ⏎ b · #general reading… ", "the daemon's text reaches the cells through `cell_row`");
    d.on_evt(&host(), json(serde_json::json!({})), at(220));
    assert_eq!(d.title(), " chat · #general reading… ");
}

/// A mapped answer with a `workspace_tag` makes the title `tag:channel`, with no `#`; the id stands for an unnamed channel;
/// an answer with the name alone (an older daemon) keeps the older form; the tag is cut like the name.
#[test]
fn the_title_joins_the_workspace_tag_and_the_channel_with_a_colon() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, false, t0);
    d.tick(wanted("R"), t0);
    d.tick(wanted("R"), at(150));
    let mut say = |d: &mut ChatDrawer, extra: serde_json::Value, ms: u64| {
        let mut j = serde_json::json!({ "state": "mapped", "workspace": "T1", "channel": "C1", "delivers": false });
        j.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        d.on_evt(&host(), answer("R", j), at(ms));
    };
    say(&mut d, serde_json::json!({ "name": "general", "workspace_tag": "acme" }), 160);
    assert_eq!(d.title(), " chat · acme:general reading… ");
    say(&mut d, serde_json::json!({ "channel": "C2", "workspace_tag": "acme" }), 170);
    assert_eq!(d.title(), " chat · acme:C2 reading… ", "a channel with no name shows its id");
    say(&mut d, serde_json::json!({ "name": "general", "workspace_tag": "acme", "workspace_name": "Acme Lab" }), 180);
    assert_eq!(d.title(), " chat · acme:general reading… ", "the tag wins over the name");
    say(&mut d, serde_json::json!({ "workspace": "T2", "name": "general", "workspace_name": "Acme Lab" }), 190);
    assert_eq!(d.title(), " chat · Acme Lab · #general reading… ", "an older daemon's answer");
    say(&mut d, serde_json::json!({ "name": "general", "workspace_tag": "a".repeat(100) }), 200);
    assert_eq!(d.title(), format!(" chat · {}…:general reading… ", "a".repeat(23)), "the tag is cut to 24 cells");
}

/// An answer for the same workspace with no short name leaves the one held; another workspace's answer clears it.
#[test]
fn an_answer_with_no_tag_keeps_the_same_workspaces_tag_and_another_workspace_drops_it() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, false, t0);
    d.tick(wanted("R"), t0);
    d.tick(wanted("R"), at(150));
    let say = |d: &mut ChatDrawer, extra: serde_json::Value, ms: u64| {
        let mut j = serde_json::json!({ "state": "mapped", "workspace": "T1", "channel": "C1", "delivers": false, "name": "general" });
        j.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        d.on_evt(&host(), answer("R", j), at(ms));
    };
    say(&mut d, serde_json::json!({ "workspace_tag": "acme" }), 160);
    say(&mut d, serde_json::json!({}), 170);
    assert_eq!(d.title(), " chat · acme:general reading… ", "the same workspace's untagged answer");
    say(&mut d, serde_json::json!({ "workspace": "T2" }), 180);
    assert_eq!(d.title(), " chat · #general reading… ", "no stale tag for another workspace");
}

/// A row left and come back to shows its short name before its resolve is answered.
#[test]
fn a_remembered_row_shows_its_tag_before_its_resolve_answers() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, false, t0);
    d.tick(wanted("R"), t0);
    d.tick(wanted("R"), at(150));
    d.on_evt(&host(), answer("R", serde_json::json!({ "state": "mapped", "workspace": "T1", "channel": "C2", "delivers": false, "name": "general", "workspace_tag": "acme" })), at(160));
    d.tick(wanted("S"), at(1000));
    d.tick(wanted("R"), at(2000));
    assert_eq!(d.title(), " chat · acme:general reading… ");
}

/// A workspace's name is free text up to the wire's 160 bytes and stands ahead of the channel: a long one is cut to a
/// cell budget with a final `…`, and the title drawn at a narrow interior keeps the channel and writes nothing past the
/// interior, whether the name is long, wide or made of characters that take no cell.
#[test]
fn a_long_workspace_name_never_costs_the_channel_or_overruns_the_interior() {
    use ratatui::{buffer::Buffer, layout::Rect, style::Style};
    use unicode_width::UnicodeWidthStr;
    const INTERIOR: u16 = 60;
    let t0 = Instant::now();
    let long_ascii = "W".repeat(160);
    let wide = "\u{4e16}".repeat(50);
    let invisible = "\u{200b}".repeat(53);
    for (name, cut) in [(long_ascii.as_str(), true), (wide.as_str(), true), (invisible.as_str(), false), ("Lab", false)] {
        let mut d = ChatDrawer::default();
        d.on_connection(&host(), true, false, t0);
        d.tick(wanted("R"), t0);
        d.tick(wanted("R"), t0 + Duration::from_millis(150));
        let json = serde_json::json!({ "state": "mapped", "workspace": "T1", "channel": "C1", "delivers": false, "name": "general", "workspace_name": name });
        d.on_evt(&host(), answer("R", json), t0 + Duration::from_millis(160));
        let title = d.title();
        let label = title.split(" · ").nth(1).unwrap();
        assert!(label.width() <= 24, "{name:?}: the workspace takes {} cells: {title:?}", label.width());
        assert_eq!(label.ends_with('…'), cut, "{name:?}: {title:?}");

        // A buffer wider than the interior, a marker in the cell past it: nothing may be written there.
        let mut buf = Buffer::filled(Rect::new(0, 0, 100, 1), ratatui::buffer::Cell::new("░"));
        crate::ui::chrome::write_title(&mut buf, 1, 0, &title, INTERIOR, Style::default());
        let row: String = (1..100).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(row.contains("#general"), "{name:?}: the channel is gone: {row:?}");
        let past: String = (1 + INTERIOR..100).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert_eq!(past, "░".repeat(100 - 1 - INTERIOR as usize), "{name:?}: the title wrote past the interior: {row:?}");

        // Whatever the title's text, the clamp is by cells: an uncut name overruns nothing either.
        let mut buf = Buffer::filled(Rect::new(0, 0, 100, 1), ratatui::buffer::Cell::new("░"));
        crate::ui::chrome::write_title(&mut buf, 1, 0, &format!(" chat · {name} · #general "), INTERIOR, Style::default());
        let past: String = (1 + INTERIOR..100).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert_eq!(past, "░".repeat(100 - 1 - INTERIOR as usize), "{name:?}: an uncut title wrote past the interior");
    }
}

/// A name belongs to the id its answer named: no other id is drawn with it.
#[test]
fn a_name_is_drawn_only_for_the_id_its_answer_named() {
    use super::row_channel::RowChannel;
    let t0 = Instant::now();
    let mut rows = RowChannel::default();
    rows.on_connection(&host(), true);
    rows.tick(wanted("R"), t0);
    let reply = ChatReply::Done(serde_json::from_value(serde_json::json!({ "state": "mapped", "workspace": "T1", "channel": "C2", "delivers": false, "name": "general" })).unwrap());
    rows.answered(&reply, t0);
    assert_eq!((rows.label("C2"), rows.label("C1")), ("#general".to_string(), "C1".to_string()));
    // An answer that names nothing, or no mapping, takes the name away.
    rows.answered(&ChatReply::Done(ChatResolveRes::Unmapped), t0);
    assert_eq!(rows.label("C2"), "C2");
}

fn reply(channel: &str, name: Option<&str>) -> ChatReply<ChatResolveRes> {
    let mut json = serde_json::json!({ "state": "mapped", "workspace": "T1", "channel": channel, "delivers": false });
    if let Some(name) = name {
        json["name"] = name.into();
    }
    ChatReply::Done(serde_json::from_value(json).unwrap())
}

/// A `mapped` answer with no name is resolved again at 1, 3, 10 and 30 s after the answer before it (the daemon looks the
/// names up after its start, so the first answer may come first), the answer standing meanwhile; a row or link change
/// starts the asking afresh; the answer that names the channel ends it; and nothing is asked after the fourth try.
#[test]
fn a_mapped_answer_with_no_name_is_asked_again_until_a_name_comes_or_the_tries_end() {
    use super::row_channel::{RowChannel, Stand};
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let asked = |rows: &mut RowChannel, row: &str, ms: u64| rows.tick(wanted(row), at(ms)).resolve.is_some();
    let mut rows = RowChannel::default();
    rows.on_connection(&host(), true);
    rows.tick(wanted("R"), t0);
    assert!(asked(&mut rows, "R", 150));
    assert_eq!(rows.next_ask(), None, "nothing is owed before the answer");
    rows.answered(&reply("C1", None), at(160));
    assert_eq!((rows.stand(), rows.label("C1")), (Stand::Mapped, "C1".to_string()), "an answer with no name stands");
    assert_eq!(rows.next_ask(), Some(at(1160)), "the first try is 1 s after the answer");
    assert!(!asked(&mut rows, "R", 1000), "nothing before it");

    // Each try is asked once, and the answer that carries no name schedules the next.
    for (ask, answer, next) in [(1200, 1210, 4210), (4300, 4310, 14310), (14400, 14410, 44410)] {
        assert!(asked(&mut rows, "R", ask), "try at {ask}");
        assert_eq!(rows.stand(), Stand::Mapped, "the answer stands while the try is out");
        assert!(!asked(&mut rows, "R", ask + 1), "asked once");
        rows.answered(&reply("C1", None), at(answer));
        assert_eq!(rows.next_ask(), Some(at(next)));
    }
    assert!(asked(&mut rows, "R", 44_500), "the fourth try");
    rows.answered(&reply("C1", None), at(44_510));
    assert_eq!(rows.next_ask(), None, "the asking ended");
    assert!(!asked(&mut rows, "R", 200_000), "nothing is asked after the last try");

    // A row change starts it afresh; so does a link change, which clears the answer and asks the row again at once.
    rows.tick(wanted("S"), at(201_000));
    assert!(asked(&mut rows, "S", 201_150));
    rows.answered(&reply("C1", None), at(201_160));
    assert_eq!(rows.next_ask(), Some(at(202_160)), "a new row has its four tries");
    rows.on_connection(&host(), false);
    assert_eq!(rows.next_ask(), None, "a link that is down asks nothing");
    rows.on_connection(&host(), true);
    assert!(asked(&mut rows, "S", 201_610));
    rows.answered(&reply("C1", None), at(201_620));
    assert_eq!(rows.next_ask(), Some(at(202_620)), "a link change has them afresh too");

    // The answer that carries the name shows it and asks nothing more.
    assert!(asked(&mut rows, "S", 202_700));
    rows.answered(&reply("C1", Some("general")), at(202_710));
    assert_eq!((rows.label("C1"), rows.next_ask()), ("#general".to_string(), None));
    assert!(!asked(&mut rows, "S", 300_000));
}

/// Through the drawer: the title and the watch hold while the name is awaited, the second resolve goes out when the first
/// answer named nothing, and the later answer that names the channel puts `#name` in the title.
#[test]
fn the_title_shows_the_name_once_a_later_answer_carries_it() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, false, t0);
    d.tick(wanted("R"), t0);
    assert_eq!(resolves(&d.tick(wanted("R"), at(150)).0), 1);
    assert_eq!(watches(&d.on_evt(&host(), mapped("R"), at(160))), 1);
    // The watch is answered and its snapshot read is out, so the owner asks nothing and only the name is awaited.
    history(&d.on_evt(&host(), complete("R"), at(165)));
    assert_eq!(d.title(), " chat · C1 reading… ");
    assert!(d.tick(wanted("R"), at(900)).0.is_empty());
    assert!(d.next_ask().is_some_and(|t| t <= at(1160)), "the window is woken for the second resolve");
    let outs = d.tick(wanted("R"), at(1160)).0;
    assert_eq!((resolves(&outs), outs.len()), (1, 1), "the second resolve, and no unwatch");
    assert_eq!(d.title(), " chat · C1 reading… ", "the answer stands while it is out");
    assert!(d.on_evt(&host(), named_to("R", "C1", "general"), at(1170)).is_empty(), "the same target is nothing to the owner");
    assert_eq!(d.title(), " chat · #general reading… ");
    assert!(d.tick(wanted("R"), at(60_000)).0.is_empty(), "the name ended the asking");
}

fn status(service: ChatService) -> ChatEvt {
    ChatEvt::Status(ChatStatusEvt { service, gap: false, resync: Vec::new() })
}

fn unavailable(row: &str) -> ChatEvt {
    let target = serde_json::from_value(serde_json::json!({ "row": row, "workspace": "T1", "channel": "C1" })).unwrap();
    ChatEvt::Watched { target, reply: ChatReply::Refused(ChatCode::Unavailable) }
}

/// A service that stops while the drawer watches may have ended for good (an app token of another app, a token Slack
/// refused): a status whose service is not running, and a watch refused `unavailable`, each resolve the shown row again,
/// once and not on every frame, so its `unconfigured` answer brings the Connect step. A running status, and another
/// host's, ask nothing.
#[test]
fn a_service_that_stops_while_watched_resolves_the_row_again() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, false, t0);
    d.on_connection(&host_b(), true, false, t0);
    d.tick(wanted("R"), t0);
    assert_eq!(resolves(&d.tick(wanted("R"), at(150)).0), 1);
    assert_eq!(watches(&d.on_evt(&host(), mapped("R"), at(160))), 1);
    history(&d.on_evt(&host(), complete("R"), at(165)));

    d.on_evt(&host(), status(ChatService::Running), at(170));
    d.on_evt(&host_b(), status(ChatService::Stopped), at(175));
    assert_eq!(resolves(&d.tick(wanted("R"), at(180)).0), 0, "a running status and another host's ask nothing");

    d.on_evt(&host(), status(ChatService::Stopped), at(200));
    assert_eq!(resolves(&d.tick(wanted("R"), at(210)).0), 1, "a stopped service: one resolve");
    assert_eq!(resolves(&d.tick(wanted("R"), at(220)).0), 0, "once per status, not each frame");
    assert!(d.on_evt(&host(), mapped("R"), at(230)).is_empty(), "the same target is nothing to the owner");

    for (n, ms) in [(1, 300), (2, 400)] {
        d.on_evt(&host(), unavailable("R"), at(ms));
        assert_eq!(resolves(&d.tick(wanted("R"), at(ms + 10)).0), 1, "refusal {n}: one resolve");
        assert_eq!(resolves(&d.tick(wanted("R"), at(ms + 20)).0), 0, "refusal {n}: not each frame");
    }

    d.on_evt(&host(), answer("R", serde_json::json!({ "state": "unconfigured", "reason": "wrong_app" })), at(500));
    d.tick(wanted("R"), at(510));
    assert!(d.in_setup(), "the ended service's answer brings the Connect step");
}

/// A sentence names the computer by what the window knows: this one, the host's name, or the words a person knows it by.
#[test]
fn a_sentence_names_the_local_a_named_and_an_unnamed_computer() {
    use super::row_channel::computer;
    assert_eq!(computer("local"), "this computer");
    assert_eq!(computer("box-a"), "box-a");
    assert_eq!(computer(""), "the computer that runs your sessions");
}
