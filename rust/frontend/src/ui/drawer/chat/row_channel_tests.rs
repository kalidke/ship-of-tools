use std::time::{Duration, Instant};

use sot_protocol::ops::{ChatHistoryReq, ChatResolveRes, ChatWatchRes, ChatWatchState};

use super::drawer::ChatDrawer;
use super::owner::{Fill, Out};
use crate::net::dial::HostKey;
use crate::net::transport::{ChatEvt, ChatReply, OutgoingReq};

fn host() -> HostKey {
    "a".to_string()
}

fn wanted(row: &str) -> Option<(HostKey, String)> {
    Some((host(), row.to_string()))
}

fn mapped(row: &str) -> ChatEvt {
    let res: ChatResolveRes = serde_json::from_value(serde_json::json!({ "state": "mapped", "workspace": "T1", "channel": "C1", "delivers": false })).unwrap();
    ChatEvt::Resolved { row: row.to_string(), reply: ChatReply::Done(res) }
}

fn complete(row: &str) -> ChatEvt {
    let target = serde_json::from_value(serde_json::json!({ "row": row, "workspace": "T1", "channel": "C1" })).unwrap();
    ChatEvt::Watched { target, reply: ChatReply::Done(ChatWatchRes { state: ChatWatchState::Complete }) }
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
    assert_eq!(unwatches(&d.tick(None, at(300)).0), 1);
    assert!(d.tick(None, at(350)).0.is_empty());

    // Chat again: one resolve, one watch. Hidden again: one unwatch.
    assert!(d.tick(wanted("R"), at(400)).0.is_empty());
    assert_eq!(resolves(&d.tick(wanted("R"), at(550)).0), 1);
    assert_eq!(watches(&d.on_evt(&host(), mapped("R"), at(560))), 1);
    assert_eq!(unwatches(&d.tick(None, at(600)).0), 1);

    // Rows passed within the debounce are never resolved.
    assert!(d.tick(wanted("S"), at(700)).0.is_empty());
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
