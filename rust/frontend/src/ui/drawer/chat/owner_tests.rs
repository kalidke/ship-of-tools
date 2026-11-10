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
/// nothing, and a watch that stays pending is sent again on the pace (the gap is `a_gap_comes_only_after_...`).
#[test]
fn a_status_is_answered_by_a_paced_watch() {
    let t0 = Instant::now();
    let mut o = complete(t0);
    let mut now = t0 + PACE;
    assert!(is_watch(&o.on_evt(&host(), status(true, &["C1"]), now)));
    assert!(o.on_evt(&host(), status(true, &["C1"]), now).is_empty(), "inside the pace");
    assert!(o.on_evt(&host(), watched(ChatWatchState::Pending), now).is_empty(), "a pending watch starts no read");
    now += FIRST_PACE;
    assert!(is_watch(&o.tick(now)));
    assert!(o.on_evt(&host(), watched(ChatWatchState::Pending), now).is_empty());
    assert!(o.tick(now + PACE - Duration::from_millis(1)).is_empty());
    assert!(is_watch(&o.tick(now + PACE)));
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
        assert_eq!(o.unsupported.contains(&host()), matches!(o.shown, Some(PostShown::Refused(ChatCode::Unavailable))));
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
    let req = history_req(&o.on_evt(&host(), watched(ChatWatchState::Complete), t0 + PACE * 2));
    o.on_evt(&host(), ChatEvt::History { req, reply: page(&["4.000001"], None) }, t0 + PACE * 2);
    assert_eq!(fill(&o), Fill::Complete, "the next read is not outdated");
    assert_eq!(o.watch.as_ref().unwrap().records.len(), 1);
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
    assert_eq!(fill(&o), Fill::Gap(Some(ChatCode::Busy)));
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
    let w = o.watch.as_ref().unwrap();
    assert_eq!(w.records.len(), 2, "the delete leaves a tombstone");
    assert!(o.is_deleted(&ts("1.000001")) && !o.is_deleted(&ts("2.000001")));
    assert!(w.records[&ts("1.000001")].text.is_empty() && w.records[&ts("2.000001")].text == "hi");
}

fn stopped() -> ChatEvt {
    ChatEvt::Status(serde_json::from_value(serde_json::json!({ "service": "stopped", "gap": false, "resync": [] })).unwrap())
}

/// The last watch attempt gets its full pace to be answered: a `complete` reply that crosses the link after the window's
/// last watch still ends complete, and the gap comes one pace after that send.
#[test]
fn a_slow_last_watch_still_ends_complete() {
    let t0 = Instant::now();
    let mut o = complete(t0);
    let mut now = t0 + PACE;
    assert!(is_watch(&o.on_evt(&host(), status(false, &["C1"]), now)));
    o.on_evt(&host(), watched(ChatWatchState::Pending), now);
    for s in [1, 6, 11, 16, 21] {
        now = t0 + PACE + Duration::from_secs(s);
        assert!(is_watch(&o.tick(now)));
    }
    assert!(o.tick(now + Duration::from_millis(16)).is_empty());
    assert_eq!(fill(&o), Fill::Needed);
    let req = history_req(&o.on_evt(&host(), watched(ChatWatchState::Complete), now + Duration::from_millis(100)));
    o.on_evt(&host(), ChatEvt::History { req, reply: page(&[], None) }, now + Duration::from_millis(200));
    assert_eq!(fill(&o), Fill::Complete);
}

/// A `chat.changed` that came while the read was in flight is newer than its snapshot and wins over it.
#[test]
fn a_change_during_the_read_is_kept_over_the_older_snapshot() {
    let t0 = Instant::now();
    let mut o = complete_with(t0, &["1.000001"]);
    o.on_evt(&host(), status(false, &["C1"]), t0 + PACE);
    let req = history_req(&o.on_evt(&host(), watched(ChatWatchState::Complete), t0 + PACE));
    let changed = serde_json::json!({ "workspace": "T1", "channel": "C1", "put": [record("3.000001", "C1")], "deleted": ["1.000001"] });
    o.on_evt(&host(), ChatEvt::Changed(serde_json::from_value(changed).unwrap()), t0 + PACE);
    o.on_evt(&host(), ChatEvt::History { req, reply: page(&["1.000001", "2.000001"], None) }, t0 + PACE);
    let mut held: Vec<_> = o.watch.as_ref().unwrap().records.keys().cloned().collect();
    held.sort_by_key(|t| String::from(t.clone()));
    assert_eq!(held, [ts("1.000001"), ts("2.000001"), ts("3.000001")], "1 is a tombstone");
    assert!(o.is_deleted(&ts("1.000001")));
    assert!(o.watch.as_ref().unwrap().overlay.is_empty());
}

/// The link going and returning starts the watch over: nothing is sent while it is down, one watch when it is up, and no
/// gap, outdated read or old attempt count survives it.
#[test]
fn the_link_returning_starts_the_watch_over() {
    let t0 = Instant::now();
    let mut o = complete(t0);
    o.on_evt(&host(), status(false, &["C1"]), t0 + PACE);
    o.on_evt(&host(), ChatEvt::Watched { target: target("C1"), reply: ChatReply::Refused(ChatCode::Busy) }, t0 + PACE);
    assert_eq!(fill(&o), Fill::Gap(Some(ChatCode::Busy)));
    assert!(o.on_evt(&host(), status(false, &["C1"]), t0 + PACE * 2).is_empty(), "a gap ends with the link, not a status");
    assert_eq!(fill(&o), Fill::Gap(Some(ChatCode::Busy)));
    assert!(o.on_connection(&host(), false, t0 + PACE * 2).is_empty());
    assert!(o.tick(t0 + PACE * 9).is_empty(), "no send while the link is down");
    assert!(is_watch(&o.on_connection(&host(), true, t0 + PACE * 9)));
    assert_eq!(fill(&o), Fill::Needed);
    assert_eq!(o.watch.as_ref().unwrap().began, t0 + PACE * 9, "the recovery started over");
    history_req(&o.on_evt(&host(), watched(ChatWatchState::Complete), t0 + PACE * 9));

    let mut o = complete(t0);
    o.on_connection(&host(), false, t0);
    assert!(is_watch(&o.on_connection(&host(), true, t0)), "a registration does not outlive the link");

    let mut o = complete(t0);
    o.on_evt(&host(), status(false, &["C1"]), t0 + PACE);
    o.on_evt(&host(), watched(ChatWatchState::Complete), t0 + PACE);
    o.on_evt(&host(), status(false, &["C1"]), t0 + PACE);
    assert!(o.watch.as_ref().unwrap().stale);
    o.on_connection(&host(), false, t0 + PACE);
    assert!(is_watch(&o.on_connection(&host(), true, t0 + PACE)), "the read in flight died with the link");
    assert!(!o.watch.as_ref().unwrap().stale);
    history_req(&o.on_evt(&host(), watched(ChatWatchState::Complete), t0 + PACE));
}

/// A refused read goes back to the watch on the pace, not to another read at once, and the next read is not outdated.
#[test]
fn a_refused_read_goes_back_to_the_watch_on_the_pace() {
    let t0 = Instant::now();
    let mut o = complete(t0);
    o.on_evt(&host(), status(false, &["C1"]), t0 + PACE);
    let req = history_req(&o.on_evt(&host(), watched(ChatWatchState::Complete), t0 + PACE));
    let refused = ChatReply::Refused(ChatCode::Busy);
    assert!(o.on_evt(&host(), ChatEvt::History { req, reply: refused }, t0 + PACE).is_empty());
    assert_eq!(fill(&o), Fill::Needed);
    assert!(is_watch(&o.tick(t0 + PACE * 2)));
}

/// An older daemon's unknown-op answer, to a watch, a read or a post, stops the sends; the state stays needed, not gap.
#[test]
fn an_unsupported_answer_stops_the_sends_for_every_op() {
    let t0 = Instant::now();
    let mut o = complete(t0);
    o.on_evt(&host(), status(false, &["C1"]), t0 + PACE);
    o.on_evt(&host(), ChatEvt::Watched { target: target("C1"), reply: ChatReply::Unsupported }, t0 + PACE);
    assert!(o.tick(t0 + PACE * 20).is_empty());
    assert_eq!(fill(&o), Fill::Needed);

    let mut o = complete(t0);
    o.on_evt(&host(), status(false, &["C1"]), t0 + PACE);
    let req = history_req(&o.on_evt(&host(), watched(ChatWatchState::Complete), t0 + PACE));
    o.on_evt(&host(), ChatEvt::History { req, reply: ChatReply::Unsupported }, t0 + PACE);
    assert!(o.unsupported.contains(&host()));
    assert!(o.tick(t0 + PACE * 20).is_empty());
}

/// A service that is not running sends the watched channel to watch again.
#[test]
fn a_stopped_service_watches_again() {
    let t0 = Instant::now();
    let mut o = complete(t0);
    assert!(is_watch(&o.on_evt(&host(), stopped(), t0 + PACE)));
}

/// An event, reply or status for another link, workspace or channel changes nothing, and the late reply of one channel's
/// read never fills another's.
#[test]
fn what_is_for_another_link_or_channel_changes_nothing() {
    let t0 = Instant::now();
    let now = t0 + PACE;
    let other = "other".to_string();
    let mut o = complete_with(t0, &["1.000001"]);
    let put = |ws: &str, ch: &str| {
        let body = serde_json::json!({ "workspace": ws, "channel": ch, "put": [record("9.000001", ch)], "deleted": ["1.000001"] });
        ChatEvt::Changed(serde_json::from_value(body).unwrap())
    };
    assert!(o.on_evt(&other, put("T1", "C1"), now).is_empty());
    assert!(o.on_evt(&host(), put("T2", "C1"), now).is_empty());
    assert!(o.on_evt(&other, status(false, &["C1"]), now).is_empty());
    assert!(o.on_bus_lost(&other, "chat.changed", now).is_empty());
    assert!(o.on_connection(&other, false, now).is_empty());
    assert_eq!(fill(&o), Fill::Complete);
    assert_eq!(o.watch.as_ref().unwrap().records.keys().collect::<Vec<_>>(), [&ts("1.000001")]);
    o.on_evt(&host(), status(false, &["C1"]), now);
    assert!(o.on_evt(&other, watched(ChatWatchState::Complete), now).is_empty(), "another link's watch reply");
    let c2 = ChatEvt::Watched { target: target("C2"), reply: ChatReply::Done(ChatWatchRes { state: ChatWatchState::Complete }) };
    assert!(o.on_evt(&host(), c2, now).is_empty(), "another channel's watch reply");
    let c1_read = history_req(&o.on_evt(&host(), watched(ChatWatchState::Complete), now));
    assert!(o.on_evt(&other, ChatEvt::History { req: c1_read.clone(), reply: page(&[], None) }, now).is_empty());
    assert_eq!(fill(&o), Fill::Reading, "another link's read reply");
    let out = o.watch(host(), target("C2"), now + PACE);
    assert!(matches!(out.as_slice(), [(_, OutgoingReq::ChatUnwatch { .. }), (_, OutgoingReq::ChatWatch { .. })]), "{}", out.len());
    let c2 = ChatEvt::Watched { target: target("C2"), reply: ChatReply::Done(ChatWatchRes { state: ChatWatchState::Complete }) };
    history_req(&o.on_evt(&host(), c2, now + PACE));
    o.on_evt(&host(), ChatEvt::History { req: c1_read, reply: page(&["5.000001"], None) }, now + PACE);
    assert_eq!(fill(&o), Fill::Reading, "C1's late reply is not C2's read");
    assert!(o.watch.as_ref().unwrap().records.is_empty());
}

fn record_with(ts: &str, thread_ts: &str, channel: &str, text: &str) -> serde_json::Value {
    let mut r = record(ts, channel);
    r["thread_ts"] = thread_ts.into();
    r["text"] = text.into();
    r
}

fn snapshot(records: Vec<serde_json::Value>) -> ChatReply<ChatHistoryRes> {
    ChatReply::Done(serde_json::from_value(serde_json::json!({ "records": records, "next": null })).unwrap())
}

/// A snapshot that is not the whole channel: older history follows.
fn snapshot_with_next(records: Vec<serde_json::Value>) -> ChatReply<ChatHistoryRes> {
    ChatReply::Done(serde_json::from_value(serde_json::json!({ "records": records, "next": "older" })).unwrap())
}

fn changed(put: Vec<serde_json::Value>) -> ChatEvt {
    ChatEvt::Changed(serde_json::from_value(serde_json::json!({ "workspace": "T1", "channel": "C1", "put": put, "deleted": [] })).unwrap())
}

fn text_of(o: &ChatOwner, t: &str) -> String {
    o.watch.as_ref().unwrap().records[&ts(t)].text.clone()
}

/// The daemon answers a watch from its published state when the watch arrives and never says later that a snapshot
/// completed, so a `Gap` is asked again every minute, with no cap, and a `complete` reply ends it. The ticks inside the
/// minute send nothing.
#[test]
fn a_gap_is_asked_again_every_minute_and_a_complete_reply_ends_it() {
    let t0 = Instant::now();
    let pending = || watched(ChatWatchState::Pending);
    // A snapshot slower than the window, told by a status; the gap, then the minute's watch succeeds.
    let mut o = complete(t0);
    let mut now = t0 + PACE;
    assert!(is_watch(&o.on_evt(&host(), status(true, &["C1"]), now)));
    o.on_evt(&host(), pending(), now);
    let begun = now;
    for s in [1, 6, 11, 16, 21] {
        now = begun + Duration::from_secs(s);
        assert!(is_watch(&o.tick(now)));
        o.on_evt(&host(), pending(), now);
    }
    assert!(o.tick(now + PACE).is_empty());
    assert_eq!(fill(&o), Fill::Gap(None));
    assert!(o.tick(now + GAP_PACE - PACE * 2).is_empty(), "inside the minute");
    let late = now + GAP_PACE;
    assert!(is_watch(&o.tick(late)));
    let req = history_req(&o.on_evt(&host(), watched(ChatWatchState::Complete), late));
    assert_eq!(req.limit, MAX_PAGE, "the daemon's snapshot size");
    o.on_evt(&host(), ChatEvt::History { req, reply: page(&[], None) }, late);
    assert_eq!(fill(&o), Fill::Complete);

    // One snapshot slower than three paces: only ticks, every reply pending, a refusal stays a gap, and no cap.
    let mut o = complete(t0);
    let mut now = t0 + PACE;
    assert!(is_watch(&o.on_evt(&host(), status(true, &["C1"]), now)));
    o.on_evt(&host(), pending(), now);
    let begun = now;
    for s in [1, 6, 11, 16, 21] {
        now = begun + Duration::from_secs(s);
        assert!(is_watch(&o.tick(now)));
    }
    o.tick(now + PACE);
    assert_eq!(fill(&o), Fill::Gap(None));
    let mut at = now;
    for _ in 0..5 {
        at += GAP_PACE;
        assert!(is_watch(&o.tick(at)), "no cap in a gap");
        assert!(o.tick(at + PACE).is_empty());
        o.on_evt(&host(), ChatEvt::Watched { target: target("C1"), reply: ChatReply::Refused(ChatCode::Busy) }, at);
        assert_eq!(fill(&o), Fill::Gap(Some(ChatCode::Busy)), "a refusal stays a gap");
    }
    at += GAP_PACE;
    assert!(is_watch(&o.tick(at)));
    history_req(&o.on_evt(&host(), watched(ChatWatchState::Complete), at));
    assert_eq!(fill(&o), Fill::Reading);
}

/// The snapshot keeps the channel's top-level messages, as the daemon's model does (it never sends an edit or delete for a
/// thread reply sent to the channel); an edit of a record older than every held one is not added, a hole otherwise.
#[test]
fn the_owner_holds_only_what_the_daemon_tracks() {
    let t0 = Instant::now();
    let mut o = complete(t0);
    o.on_evt(&host(), status(false, &["C1"]), t0 + PACE);
    let req = history_req(&o.on_evt(&host(), watched(ChatWatchState::Complete), t0 + PACE));
    let reply = snapshot_with_next(vec![
        record_with("10.000001", "10.000001", "C1", "top"),
        record_with("11.000001", "9.000001", "C1", "a thread reply sent to the channel"),
        record_with("12.000001", "12.000001", "C2", "another channel"),
    ]);
    o.on_evt(&host(), ChatEvt::History { req, reply }, t0 + PACE);
    assert_eq!(o.watch.as_ref().unwrap().records.keys().collect::<Vec<_>>(), [&ts("10.000001")]);

    o.on_evt(&host(), changed(vec![record_with("2.000001", "2.000001", "C1", "edited below the page")]), t0 + PACE);
    assert_eq!(o.watch.as_ref().unwrap().records.len(), 1, "older than every held one");
    o.on_evt(&host(), changed(vec![record_with("10.000001", "10.000001", "C1", "edited")]), t0 + PACE);
    o.on_evt(&host(), changed(vec![record_with("9.000001", "9.000001", "C1", "new, older than 10 by a digit")]), t0 + PACE);
    assert_eq!(text_of(&o, "10.000001"), "edited", "an edit of a held record replaces it");
    assert_eq!(o.watch.as_ref().unwrap().records.len(), 1, "9 is below 10 whatever its length");
    o.on_evt(&host(), changed(vec![record_with("100.000001", "100.000001", "C1", "newest")]), t0 + PACE);
    assert_eq!(o.watch.as_ref().unwrap().records.len(), 2, "a newer record is added");
}

/// The same host and target watched again is nothing; the same channel on the same host is a watch alone, since the
/// daemon's watch replaces the connection's earlier one and an unwatch would let it drop the channel.
#[test]
fn watching_again_sends_only_what_changes() {
    let t0 = Instant::now();
    let mut o = complete(t0);
    assert!(o.watch(host(), target("C1"), t0).is_empty(), "unchanged");
    assert_eq!(fill(&o), Fill::Complete, "and the state is kept");
    let mut other_row = target("C1");
    other_row.row = "other__box".into();
    assert!(is_watch(&o.watch(host(), other_row, t0)), "same channel: the watch alone");
    let out = o.watch("other".to_string(), target("C1"), t0);
    assert!(matches!(out.as_slice(), [(_, OutgoingReq::ChatUnwatch { .. })]), "another host: the old host is released (the new one is not connected)");
}

/// The events kept over a snapshot are those of the read in flight: one from before a refused read, a link loss or with no
/// read at all never covers a newer snapshot. While the link is down the state is not whole.
#[test]
fn only_the_events_of_the_read_in_flight_cover_a_snapshot() {
    let t0 = Instant::now();
    let event = || changed(vec![record_with("5.000001", "5.000001", "C1", "event")]);
    let newer = || snapshot(vec![record_with("5.000001", "5.000001", "C1", "snapshot")]);

    // A change during a read, then the read is refused; the next snapshot wins.
    let mut o = complete(t0);
    o.on_evt(&host(), status(false, &["C1"]), t0 + PACE);
    let req = history_req(&o.on_evt(&host(), watched(ChatWatchState::Complete), t0 + PACE));
    o.on_evt(&host(), event(), t0 + PACE);
    o.on_evt(&host(), ChatEvt::History { req, reply: ChatReply::Refused(ChatCode::Busy) }, t0 + PACE);
    assert!(is_watch(&o.tick(t0 + PACE * 2)));
    let req = history_req(&o.on_evt(&host(), watched(ChatWatchState::Complete), t0 + PACE * 2));
    o.on_evt(&host(), ChatEvt::History { req, reply: newer() }, t0 + PACE * 2);
    assert_eq!(text_of(&o, "5.000001"), "snapshot");

    let mut down = complete(t0);
    down.on_connection(&host(), false, t0);
    assert_ne!(fill(&down), Fill::Complete, "a whole state does not stay whole over a dead link");

    // A change during a read, then the link flaps; the newer snapshot wins, and the state is not whole while it is down.
    let mut o = complete(t0);
    o.on_evt(&host(), status(false, &["C1"]), t0 + PACE);
    history_req(&o.on_evt(&host(), watched(ChatWatchState::Complete), t0 + PACE));
    o.on_evt(&host(), event(), t0 + PACE);
    o.on_connection(&host(), false, t0 + PACE);
    assert_ne!(fill(&o), Fill::Complete, "the link is down");
    assert!(is_watch(&o.on_connection(&host(), true, t0 + PACE)));
    let req = history_req(&o.on_evt(&host(), watched(ChatWatchState::Complete), t0 + PACE));
    o.on_evt(&host(), ChatEvt::History { req, reply: newer() }, t0 + PACE);
    assert_eq!(text_of(&o, "5.000001"), "snapshot");

    // A change with no read in flight is not kept for the next one.
    let mut o = complete(t0);
    o.on_evt(&host(), event(), t0);
    o.on_evt(&host(), status(false, &["C1"]), t0 + PACE);
    let req = history_req(&o.on_evt(&host(), watched(ChatWatchState::Complete), t0 + PACE));
    o.on_evt(&host(), ChatEvt::History { req, reply: newer() }, t0 + PACE);
    assert_eq!(text_of(&o, "5.000001"), "snapshot");
}

/// A post reply from another host, equal in value, answers nothing.
#[test]
fn a_post_reply_from_another_host_answers_nothing() {
    let now = Instant::now();
    let mut o = complete(now);
    assert!(o.post(host(), post_req("one")).is_some());
    let reply = ChatReply::Done(ChatPostRes::Posted { ts: ts("1700000000.000100") });
    o.on_evt(&"other".to_string(), ChatEvt::Posted { req: post_req("one"), reply }, now);
    assert!(o.posting.is_some() && o.shown.is_none());
}

fn held(o: &ChatOwner) -> usize {
    o.watch.as_ref().unwrap().records.len()
}

/// The owner keeps the daemon model's newest 100 records, as the daemon does: past them the oldest go, and an edit of one
/// of them is never sent, so it must not stay shown.
#[test]
fn the_owner_keeps_at_most_the_daemons_newest_records() {
    let t0 = Instant::now();
    let old: Vec<String> = (0..20).map(|i| format!("{}.000001", 10 + i)).collect();
    let mut o = complete_with(t0, &old.iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(held(&o), 20);
    for i in 0..KEPT {
        o.on_evt(&host(), changed(vec![record_with(&format!("{}.000001", 100 + i), &format!("{}.000001", 100 + i), "C1", "new")]), t0);
    }
    assert_eq!(held(&o), KEPT);
    assert!(!o.watch.as_ref().unwrap().records.contains_key(&ts("10.000001")), "the oldest are gone");
    assert!(o.watch.as_ref().unwrap().records.contains_key(&ts("199.000001")));
}

/// Deleted records stay held, so the floor of the snapshot stands: an older record the daemon holds is not added alone.
#[test]
fn a_put_below_the_floor_is_refused_after_deletes_too() {
    let t0 = Instant::now();
    let mut o = complete(t0);
    o.on_evt(&host(), status(false, &["C1"]), t0 + PACE);
    let req = history_req(&o.on_evt(&host(), watched(ChatWatchState::Complete), t0 + PACE));
    let reply = snapshot_with_next(vec![record_with("20.000001", "20.000001", "C1", "a"), record_with("30.000001", "30.000001", "C1", "b")]);
    o.on_evt(&host(), ChatEvt::History { req, reply }, t0 + PACE);
    let gone = serde_json::json!({ "workspace": "T1", "channel": "C1", "put": [], "deleted": ["20.000001", "30.000001"] });
    o.on_evt(&host(), ChatEvt::Changed(serde_json::from_value(gone).unwrap()), t0 + PACE);
    assert_eq!(held(&o), 2, "two tombstones");
    o.on_evt(&host(), changed(vec![record_with("10.000001", "10.000001", "C1", "older")]), t0 + PACE);
    assert_eq!(held(&o), 2, "below the floor");
}

/// A delete keeps the record's place, author and time and drops its text; the tombstone counts against the bound, shows as
/// `(message deleted)`, and a replacing snapshot drops it. A delete of a record not held adds nothing.
#[test]
fn a_delete_leaves_a_tombstone_in_place() {
    let t0 = Instant::now();
    let mut o = complete(t0);
    o.on_evt(&host(), changed(vec![record_with("5.000001", "5.000001", "C1", "secret")]), t0);
    let gone = |ts: &str| ChatEvt::Changed(serde_json::from_value(serde_json::json!({ "workspace": "T1", "channel": "C1", "put": [], "deleted": [ts] })).unwrap());
    o.on_evt(&host(), gone("9.000001"), t0);
    assert_eq!(held(&o), 1, "a delete of a record not held adds nothing");
    o.on_evt(&host(), gone("5.000001"), t0);
    assert_eq!(held(&o), 1, "the record stays");
    let r = o.records()[0].clone();
    assert_eq!((r.ts.clone(), r.author.name.as_str()), (ts("5.000001"), "Ada"), "identity, author and time kept");
    assert!(r.text.is_empty(), "text dropped");
    assert!(o.is_deleted(&ts("5.000001")), "the owner keeps that it was deleted");
    for i in 0..KEPT {
        o.on_evt(&host(), changed(vec![record_with(&format!("{}.000001", 100 + i), &format!("{}.000001", 100 + i), "C1", "new")]), t0);
    }
    assert_eq!(held(&o), KEPT, "a tombstone counts against the bound");
    assert!(!o.watch.as_ref().unwrap().records.contains_key(&ts("5.000001")), "and is the oldest to go");
    assert!(!o.is_deleted(&ts("5.000001")), "its flag goes with it");
    o.on_evt(&host(), changed(vec![record_with("500.000001", "500.000001", "C1", "x")]), t0);
    o.on_evt(&host(), gone("500.000001"), t0);
    o.on_evt(&host(), status(false, &["C1"]), t0 + PACE);
    let req = history_req(&o.on_evt(&host(), watched(ChatWatchState::Complete), t0 + PACE));
    o.on_evt(&host(), ChatEvt::History { req, reply: snapshot(vec![record_with("7.000001", "7.000001", "C1", "kept")]) }, t0 + PACE);
    assert_eq!(o.records().len(), 1, "a replacing snapshot drops every tombstone");
    assert!(!o.is_deleted(&ts("500.000001")), "and every flag");
}

/// A put after a delete for the same time brings the message back with the new text; a message the daemon could not read
/// is never a deleted one.
#[test]
fn a_put_after_a_delete_brings_the_message_back() {
    let t0 = Instant::now();
    let mut o = complete(t0);
    o.on_evt(&host(), changed(vec![record_with("5.000001", "5.000001", "C1", "first")]), t0);
    let gone = serde_json::json!({ "workspace": "T1", "channel": "C1", "put": [], "deleted": ["5.000001"] });
    o.on_evt(&host(), ChatEvt::Changed(serde_json::from_value(gone).unwrap()), t0);
    assert!(o.is_deleted(&ts("5.000001")));
    o.on_evt(&host(), changed(vec![record_with("5.000001", "5.000001", "C1", "again")]), t0);
    assert!(!o.is_deleted(&ts("5.000001")));
    assert_eq!(text_of(&o, "5.000001"), "again");
    let mut unreadable = record_with("6.000001", "6.000001", "C1", "");
    unreadable["omitted"] = true.into();
    unreadable["truncated"] = true.into();
    o.on_evt(&host(), changed(vec![unreadable]), t0);
    assert!(!o.is_deleted(&ts("6.000001")), "an unreadable message is not a deleted one");
}

/// A whole, empty channel has no floor: its first message is held.
#[test]
fn the_first_message_of_an_empty_channel_is_held() {
    let t0 = Instant::now();
    let mut o = complete(t0);
    o.on_evt(&host(), changed(vec![record_with("5.000001", "5.000001", "C1", "first")]), t0);
    assert_eq!(held(&o), 1);
}

/// A watch reply that is not `complete`, arriving during a read, means the daemon began a snapshot the read may not hold:
/// the read is outdated.
#[test]
fn a_pending_watch_reply_during_a_read_outdates_it() {
    let t0 = Instant::now();
    let mut o = complete(t0);
    o.on_evt(&host(), status(false, &["C1"]), t0 + PACE);
    let req = history_req(&o.on_evt(&host(), watched(ChatWatchState::Complete), t0 + PACE));
    assert!(!o.watch.as_ref().unwrap().stale);
    o.on_evt(&host(), watched(ChatWatchState::Pending), t0 + PACE);
    assert!(o.watch.as_ref().unwrap().stale, "the read in flight is not the snapshot");
    o.on_evt(&host(), ChatEvt::History { req, reply: page(&["5.000001"], None) }, t0 + PACE);
    assert_ne!(fill(&o), Fill::Complete);
}

/// A gap with no attempt on record asks at once.
#[test]
fn a_gap_with_no_last_try_asks_at_once() {
    let t0 = Instant::now();
    let mut o = complete(t0);
    let w = o.watch.as_mut().unwrap();
    (w.fill, w.last_try) = (Fill::Gap(None), None);
    assert!(is_watch(&o.tick(t0)));
}

/// What the drawer would say of the state: `None` when it claims nothing, else the gap and its reason.
fn gap_of(o: &ChatOwner) -> Option<Option<ChatCode>> {
    match fill(o) {
        Fill::Gap(code) => Some(code),
        _ => None,
    }
}

/// A watched channel on a clock of whole seconds: the daemon's reply to each watch sent at second `s` is `answer(s)`, a
/// history read is answered at once, and `status_at` injects a status that lists the channel (which must send nothing
/// inside the pace). Returns the seconds a watch went out, and the first second the state was a gap.
fn run_seconds(until: u64, status_at: Option<u64>, answer: impl Fn(u64) -> ChatReply<ChatWatchRes>) -> (ChatOwner, Vec<u64>, Option<u64>) {
    run_reads(until, status_at, answer, |_| page(&[], None))
}

/// `run_seconds` with the reply to each history read sent at second `s` given too.
fn run_reads(
    until: u64,
    status_at: Option<u64>,
    answer: impl Fn(u64) -> ChatReply<ChatWatchRes>,
    read: impl Fn(u64) -> ChatReply<ChatHistoryRes>,
) -> (ChatOwner, Vec<u64>, Option<u64>) {
    let t0 = Instant::now();
    let mut o = ChatOwner::default();
    o.on_connection(&host(), true, t0);
    let (mut sent, mut gap) = (Vec::new(), None);
    let mut out = o.watch(host(), target("C1"), t0);
    for s in 0..=until {
        let now = t0 + Duration::from_secs(s);
        if status_at == Some(s) {
            assert!(o.on_evt(&host(), status(true, &["C1"]), now).is_empty(), "a status inside the pace sends nothing");
        }
        if s > 0 {
            out = o.tick(now);
        }
        while !out.is_empty() {
            let mut next = Vec::new();
            for (_, req) in out.drain(..) {
                match req {
                    OutgoingReq::ChatWatch { .. } => {
                        sent.push(s);
                        next.extend(o.on_evt(&host(), ChatEvt::Watched { target: target("C1"), reply: answer(s) }, now));
                    }
                    OutgoingReq::ChatHistory { req } => next.extend(o.on_evt(&host(), ChatEvt::History { req, reply: read(s) }, now)),
                    _ => {}
                }
            }
            out = next;
        }
        if gap.is_none() && gap_of(&o).is_some() {
            gap = Some(s);
        }
    }
    (o, sent, gap)
}

/// The gap comes only after the daemon has had time to start its service and for its slowest snapshot (20 s from the first
/// watch it admits), and always says why; a target refused for good is not asked again.
#[test]
fn a_gap_comes_only_after_the_daemons_slowest_snapshot_and_says_why() {
    let pending = || ChatReply::Done(ChatWatchRes { state: ChatWatchState::Pending });
    let complete = || ChatReply::Done(ChatWatchRes { state: ChatWatchState::Complete });
    let refused = |c| ChatReply::Refused(c);

    // Pending to 16 s, then complete at 21 s.
    let (o, sent, gap) = run_seconds(40, None, |s| if s <= 16 { pending() } else { complete() });
    assert_eq!((fill(&o), gap, sent), (Fill::Complete, None, vec![0, 1, 6, 11, 16, 21]));

    // Always pending: the gap comes at 26 s and not before, and a status at 3 s changes nothing.
    for status_at in [None, Some(3)] {
        let (o, sent, gap) = run_seconds(30, status_at, |_| pending());
        assert_eq!((gap, gap_of(&o), sent), (Some(26), Some(None), vec![0, 1, 6, 11, 16, 21]));
    }

    // A starting service refuses unavailable to 11 s; its snapshot is then slow, and completes at 36 s.
    let (o, sent, gap) = run_seconds(50, None, |s| match s {
        0..=11 => refused(ChatCode::Unavailable),
        12..=31 => pending(),
        _ => complete(),
    });
    assert_eq!((fill(&o), gap, sent), (Fill::Complete, None, vec![0, 1, 6, 11, 16, 21, 26, 31, 36]));

    // Refusals that can clear: shown at once with their reason, asked again each minute.
    let (o, sent, gap) = run_seconds(200, None, |_| refused(ChatCode::TooManyWatches));
    assert_eq!((gap, gap_of(&o), sent), (Some(0), Some(Some(ChatCode::TooManyWatches)), vec![0, 60, 120, 180]));
    let (_, sent, gap) = run_seconds(60, None, |_| refused(ChatCode::Unavailable));
    assert_eq!((gap, sent), (Some(21), vec![0, 1, 6, 11, 16, 21]));
    let (o, _, _) = run_seconds(60, None, |_| refused(ChatCode::Unavailable));
    assert_eq!(gap_of(&o), Some(Some(ChatCode::Unavailable)));

    // Refused for good: asked once, and not again for an hour; the others are asked each minute.
    for (code, for_good) in [(ChatCode::StaleRow, true), (ChatCode::StaleMapping, true), (ChatCode::Unmapped, true), (ChatCode::TooManyWatches, false), (ChatCode::Unavailable, false)] {
        let (o, sent, _) = run_seconds(3600, None, |_| refused(code));
        assert_eq!(gap_of(&o), Some(Some(code)));
        assert_eq!(sent.len() == 1, for_good, "{code:?} asked at {sent:?}");
        assert!(for_good || sent.last() > Some(&3500), "{code:?} is asked each minute");
    }

    // The watch complete but every read refused: a gap with the read's reason, asked again each minute.
    let (o, sent, gap) = run_reads(300, None, |_| complete(), |_| ChatReply::Refused(ChatCode::Busy));
    assert_eq!((gap, gap_of(&o)), (Some(26), Some(Some(ChatCode::Busy))), "watched at {sent:?}");

    // A gap's reason does not outlive a pending reply: the service that was not running now is.
    let (o, _, _) = run_seconds(100, None, |s| if s < 30 { refused(ChatCode::Unavailable) } else { pending() });
    assert_eq!(gap_of(&o), Some(None));

    // A pending answer drops the read's refusal before the gap as well as inside it.
    let (o, _, _) = run_reads(40, None, |s| if s == 0 { complete() } else { pending() }, |_| ChatReply::Refused(ChatCode::Busy));
    assert_eq!(gap_of(&o), Some(None));

    // A refusal from before a complete snapshot is not the reason of a later gap (here a watch never answered).
    let (mut o, _, _) = run_reads(10, None, |_| complete(), |s| if s == 0 { ChatReply::Refused(ChatCode::Busy) } else { page(&[], None) });
    assert_eq!(fill(&o), Fill::Complete);
    let t = Instant::now() + Duration::from_secs(100);
    assert!(is_watch(&o.on_evt(&host(), status(true, &["C1"]), t)));
    for s in [1, 6, 11, 16, 21, 26] {
        o.tick(t + Duration::from_secs(s));
    }
    assert_eq!(gap_of(&o), Some(None));
}

/// A watch the daemon never answers still ends in a gap: the window runs from the recovery's start.
#[test]
fn a_watch_never_answered_still_ends_in_a_gap() {
    let t0 = Instant::now();
    let mut o = ChatOwner::default();
    o.on_connection(&host(), true, t0);
    assert!(is_watch(&o.watch(host(), target("C1"), t0)));
    for s in [1, 6, 11, 16, 21] {
        assert!(is_watch(&o.tick(t0 + Duration::from_secs(s))));
    }
    assert!(o.tick(t0 + Duration::from_secs(25)).is_empty());
    assert_eq!(gap_of(&o), None);
    assert!(o.tick(t0 + Duration::from_secs(26)).is_empty());
    assert_eq!(gap_of(&o), Some(None));
}

/// The window is dated from the last watch sent before the admitting answer, which can be later than the admitted watch when
/// the answer is slow; the last watch of the window gets one pace, and the gap comes then, not a pace later.
#[test]
fn a_late_admission_still_ends_the_window_at_twenty_seconds() {
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let mut o = ChatOwner::default();
    o.on_connection(&host(), true, t0);
    assert!(is_watch(&o.watch(host(), target("C1"), t0)));
    assert!(is_watch(&o.tick(at(1))));
    o.on_evt(&host(), watched(ChatWatchState::Pending), at(1) + Duration::from_millis(500));
    for s in [6, 11, 16, 21] {
        assert!(is_watch(&o.tick(at(s))));
    }
    assert!(o.tick(at(25)).is_empty());
    assert_eq!(gap_of(&o), None);
    assert!(o.tick(at(26)).is_empty());
    assert_eq!(gap_of(&o), Some(None));
}
