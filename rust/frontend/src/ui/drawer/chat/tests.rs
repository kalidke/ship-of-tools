use super::*;

fn host() -> HostKey {
    "h".to_string()
}

fn target(channel: &str) -> ChatTarget {
    serde_json::from_value(serde_json::json!({ "row": "repo__box", "workspace": "T1", "channel": channel })).unwrap()
}

fn post_req(text: &str) -> ChatPostReq {
    serde_json::from_value(serde_json::json!({ "target": target("C1"), "kind": "person", "name": "Ada", "text": text })).unwrap()
}

fn status(gap: bool, resync: &[&str]) -> ChatEvt {
    ChatEvt::Status(serde_json::from_value(serde_json::json!({ "service": "running", "gap": gap, "resync": resync })).unwrap())
}

fn record(ts: &str, channel: &str) -> serde_json::Value {
    serde_json::json!({ "workspace": "T1", "channel": channel, "ts": ts, "thread_ts": ts, "author": { "id": "U1", "name": "Ada" },
        "origin": "member", "text": "hi", "reply_count": 0, "files": [] })
}

fn ts(s: &str) -> Ts {
    Ts::parse(s).unwrap()
}

fn page(records: &[&str], next: Option<&str>) -> ChatReply<ChatHistoryRes> {
    let records: Vec<_> = records.iter().map(|t| record(t, "C1")).collect();
    ChatReply::Done(serde_json::from_value(serde_json::json!({ "records": records, "next": next })).unwrap())
}

fn watched(state: ChatWatchState) -> ChatEvt {
    ChatEvt::Watched { target: target("C1"), reply: ChatReply::Done(ChatWatchRes { state }) }
}

fn history_req(out: &[Out]) -> ChatHistoryReq {
    match out {
        [(_, OutgoingReq::ChatHistory { req })] => req.clone(),
        other => panic!("one history read expected, got {} requests", other.len()),
    }
}

fn is_watch(out: &[Out]) -> bool {
    matches!(out, [(_, OutgoingReq::ChatWatch { .. })])
}

fn fill(o: &ChatOwner) -> Fill {
    o.watch.as_ref().unwrap().fill
}

/// A connected, watched owner whose snapshot holds `held`, read whole: `Complete`, with no attempt counted.
fn complete_with(now: Instant, held: &[&str]) -> ChatOwner {
    let mut o = ChatOwner::default();
    o.on_connection(&host(), true, now);
    assert!(is_watch(&o.watch(host(), target("C1"), now)));
    let req = history_req(&o.on_evt(&host(), watched(ChatWatchState::Complete), now));
    assert!(o.on_evt(&host(), ChatEvt::History { req, reply: page(held, None) }, now).is_empty());
    assert_eq!(fill(&o), Fill::Complete);
    o
}

fn complete(now: Instant) -> ChatOwner {
    complete_with(now, &[])
}

/// The loss rule: a lost chat bus marks the watched state incomplete and watches again; the read follows the watch's
/// `complete` reply; a lost bus of another kind says nothing about the chat.
#[test]
fn a_lost_chat_bus_marks_the_state_incomplete_and_watches_again() {
    let t0 = Instant::now();
    let now = t0 + PACE;
    let mut o = complete(t0);
    assert!(o.on_bus_lost(&host(), "file.changed", now).is_empty());
    assert_eq!(fill(&o), Fill::Complete);
    assert!(is_watch(&o.on_bus_lost(&host(), "chat.changed", now)));
    assert_eq!(fill(&o), Fill::Needed);
    history_req(&o.on_evt(&host(), watched(ChatWatchState::Complete), now));
    assert_eq!(fill(&o), Fill::Reading);
}

/// Rule 1: a post goes out only after the last one's reply.
#[test]
fn a_post_is_sent_only_after_the_last_ones_reply() {
    let mut o = complete(Instant::now());
    assert!(o.post(host(), post_req("one")).is_some());
    assert!(o.post(host(), post_req("two")).is_none(), "the first has no reply yet");
    let reply = ChatReply::Done(ChatPostRes::Posted { ts: ts("1700000000.000100") });
    o.on_evt(&host(), ChatEvt::Posted { req: post_req("one"), reply }, Instant::now());
    assert!(matches!(o.shown, Some(PostShown::Posted(_))));
    assert!(o.post(host(), post_req("two")).is_some());
}

/// Rule 2: a status that lists the channel is answered by a watch, never by a read of its own; losses within the pace send
/// nothing, a watch that stays pending is sent again on the pace and counts, and attempts that never complete end in a
/// shown gap instead of a loop.
#[test]
fn a_status_is_answered_by_a_paced_and_capped_watch() {
    let t0 = Instant::now();
    let mut o = complete(t0);
    let mut now = t0 + PACE;
    assert!(is_watch(&o.on_evt(&host(), status(true, &["C1"]), now)));
    assert!(o.on_evt(&host(), status(true, &["C1"]), now).is_empty(), "inside the pace");
    for _ in 1..MAX_TRIES {
        assert!(o.on_evt(&host(), watched(ChatWatchState::Pending), now).is_empty(), "a pending watch starts no read");
        now += PACE;
        assert!(is_watch(&o.tick(now)));
    }
    assert!(o.on_evt(&host(), watched(ChatWatchState::Pending), now).is_empty());
    assert!(o.tick(now + PACE).is_empty());
    assert_eq!(fill(&o), Fill::Gap, "recovery cannot complete: the gap is shown");
}

/// Rule 3: a status entry for another channel, a bare gap flag, or any status once nothing is watched starts nothing.
#[test]
fn a_status_entry_for_an_unwatched_channel_is_ignored() {
    let t0 = Instant::now();
    let now = t0 + PACE;
    let mut o = complete(t0);
    assert!(o.on_evt(&host(), status(false, &["C2"]), now).is_empty());
    assert!(o.on_evt(&host(), status(true, &["C2"]), now).is_empty(), "the gap flag alone says nothing about this channel");
    assert_eq!(fill(&o), Fill::Complete);
    assert!(is_watch(&o.on_evt(&host(), status(false, &["C1"]), now)));
    o.watch = None;
    assert!(o.on_evt(&host(), status(false, &["C1"]), now).is_empty());
}

/// Rule 4: the unsupported state is per reply, and the connection coming back clears it.
#[test]
fn the_unsupported_state_clears_when_the_connection_comes_back() {
    let now = Instant::now();
    let mut o = complete(now);
    o.on_evt(&host(), ChatEvt::Watched { target: target("C1"), reply: ChatReply::Unsupported }, now);
    assert!(o.unsupported.contains(&host()));
    o.on_connection(&host(), false, now);
    assert!(o.unsupported.contains(&host()), "a lost link does not clear it");
    o.on_connection(&host(), true, now);
    assert!(o.unsupported.is_empty());
}

/// Rule 5: two replies to one queued read apply once.
#[test]
fn a_second_reply_to_one_read_is_not_applied() {
    let t0 = Instant::now();
    let mut o = complete(t0);
    o.on_evt(&host(), status(false, &["C1"]), t0 + PACE);
    let req = history_req(&o.on_evt(&host(), watched(ChatWatchState::Complete), t0 + PACE));
    o.on_evt(&host(), ChatEvt::History { req: req.clone(), reply: page(&[], None) }, t0 + PACE);
    assert_eq!(fill(&o), Fill::Complete);
    o.watch.as_mut().unwrap().fill = Fill::Needed;
    o.on_evt(&host(), ChatEvt::History { req, reply: page(&[], None) }, t0 + PACE);
    assert_eq!(fill(&o), Fill::Needed, "the second reply found no read in flight");
}

/// Rule 6: a chat event that did not parse is a gap to watch again from, not silence.
#[test]
fn an_unreadable_chat_event_watches_again() {
    let t0 = Instant::now();
    let mut o = complete(t0);
    assert!(o.on_evt(&"other".to_string(), ChatEvt::Unreadable, t0 + PACE).is_empty(), "another link's loss is not this watch's");
    assert!(is_watch(&o.on_evt(&host(), ChatEvt::Unreadable, t0 + PACE)));
}

/// Rule 7: a post waiting when its link ends is unknown (it may exist); one never written is refused, not unknown.
#[test]
fn a_post_is_unknown_only_when_it_may_exist() {
    let now = Instant::now();
    let mut o = complete(now);
    assert!(o.post(host(), post_req("one")).is_some());
    o.on_connection(&host(), false, now);
    assert_eq!(o.shown, Some(PostShown::Unknown), "written, and the link ended before its reply");
    assert!(o.post(host(), post_req("two")).is_none());
    assert_eq!(o.shown, Some(PostShown::Refused(ChatCode::Unavailable)), "never written");
}

/// Every post reply reads as what it proves, and a reply for another request changes nothing.
#[test]
fn a_posts_outcome_reads_as_the_reply_proves() {
    let cases: Vec<(ChatReply<ChatPostRes>, PostShown)> = vec![
        (ChatReply::Done(ChatPostRes::Posted { ts: ts("1700000000.000100") }), PostShown::Posted(ts("1700000000.000100"))),
        (ChatReply::Done(ChatPostRes::Refused { code: ChatCode::Busy }), PostShown::Refused(ChatCode::Busy)),
        (ChatReply::Done(ChatPostRes::Limited { retry_after_secs: 30, assumed: true }), PostShown::Limited(30)),
        (ChatReply::Done(ChatPostRes::Unknown), PostShown::Unknown),
        (ChatReply::Unsupported, PostShown::Refused(ChatCode::Unavailable)),
    ];
    for (reply, want) in cases {
        let mut o = complete(Instant::now());
        o.post(host(), post_req("one"));
        o.on_evt(&host(), ChatEvt::Posted { req: post_req("other"), reply: ChatReply::Done(ChatPostRes::Posted { ts: ts("1.000001") }) }, Instant::now());
        assert!(o.posting.is_some() && o.shown.is_none(), "a reply for another post clears nothing");
        o.on_evt(&host(), ChatEvt::Posted { req: post_req("one"), reply }, Instant::now());
        assert_eq!(o.shown, Some(want));
        assert!(o.posting.is_none());
    }
}

/// A snapshot replaces what was held: a record deleted during the loss is gone, and a first page with an older-history
/// cursor is a whole snapshot.
#[test]
fn a_snapshot_replaces_the_held_records_and_a_next_cursor_is_not_a_gap() {
    let t0 = Instant::now();
    let mut o = complete_with(t0, &["1.000001", "2.000001"]);
    assert_eq!(o.watch.as_ref().unwrap().records.len(), 2);
    o.on_evt(&host(), status(false, &["C1"]), t0 + PACE);
    let req = history_req(&o.on_evt(&host(), watched(ChatWatchState::Complete), t0 + PACE));
    o.on_evt(&host(), ChatEvt::History { req, reply: page(&["2.000001"], Some("older")) }, t0 + PACE);
    assert_eq!(fill(&o), Fill::Complete);
    assert_eq!(o.watch.as_ref().unwrap().records.keys().collect::<Vec<_>>(), [&ts("2.000001")]);
}

/// The reply of a read a loss outdated merges nothing and does not make the state whole.
#[test]
fn the_late_reply_of_a_stale_read_leaves_the_state_needed() {
    let t0 = Instant::now();
    let mut o = complete(t0);
    o.on_evt(&host(), status(false, &["C1"]), t0 + PACE);
    let req = history_req(&o.on_evt(&host(), watched(ChatWatchState::Complete), t0 + PACE));
    assert!(o.on_evt(&host(), status(false, &["C1"]), t0 + PACE * 2).is_empty(), "the read in flight is the only one");
    let late = o.on_evt(&host(), ChatEvt::History { req, reply: page(&["3.000001"], None) }, t0 + PACE * 2);
    assert_eq!(fill(&o), Fill::Needed);
    assert!(o.watch.as_ref().unwrap().records.is_empty());
    assert!(is_watch(&late), "the next step is a watch, never a second read beside it");
}

/// A refused watch is a shown gap, and a duplicate `complete` reply starts no second read.
#[test]
fn a_refused_watch_is_a_gap_and_a_duplicate_reply_starts_no_read() {
    let t0 = Instant::now();
    let mut o = complete(t0);
    assert!(o.on_evt(&host(), watched(ChatWatchState::Complete), t0).is_empty(), "the state needs no read");
    o.on_evt(&host(), ChatEvt::Watched { target: target("C1"), reply: ChatReply::Refused(ChatCode::Busy) }, t0);
    assert_eq!(fill(&o), Fill::Complete, "a late reply to a watch the state no longer waits on changes nothing");
    o.on_evt(&host(), status(false, &["C1"]), t0 + PACE);
    o.on_evt(&host(), ChatEvt::Watched { target: target("C1"), reply: ChatReply::Refused(ChatCode::Busy) }, t0 + PACE);
    assert_eq!(fill(&o), Fill::Gap);
    assert!(o.tick(t0 + PACE * 3).is_empty());
}

/// `chat.changed` for the watched channel puts and deletes records; another channel's is ignored.
#[test]
fn changed_events_put_and_delete_the_watched_channels_records() {
    let now = Instant::now();
    let mut o = complete_with(now, &["1.000001"]);
    let changed = |channel: &str, put: &[&str], deleted: &[&str]| {
        let put: Vec<_> = put.iter().map(|t| record(t, channel)).collect();
        ChatEvt::Changed(serde_json::from_value(serde_json::json!({ "workspace": "T1", "channel": channel, "put": put, "deleted": deleted })).unwrap())
    };
    o.on_evt(&host(), changed("C1", &["2.000001"], &[]), now);
    o.on_evt(&host(), changed("C2", &["3.000001"], &["1.000001"]), now);
    assert_eq!(o.watch.as_ref().unwrap().records.len(), 2, "C2's event changed nothing");
    o.on_evt(&host(), changed("C1", &[], &["1.000001"]), now);
    assert_eq!(o.watch.as_ref().unwrap().records.keys().collect::<Vec<_>>(), [&ts("2.000001")]);
}
