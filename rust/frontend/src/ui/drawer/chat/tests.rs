use super::*;
use sot_protocol::ops::ChatHistoryRes;

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

fn page(next: Option<&str>) -> ChatReply<ChatHistoryRes> {
    ChatReply::Done(serde_json::from_value(serde_json::json!({ "records": [], "next": next })).unwrap())
}

fn history_req(out: &[Out]) -> ChatHistoryReq {
    match out {
        [(_, OutgoingReq::ChatHistory { req })] => req.clone(),
        other => panic!("one history read expected, got {} requests", other.len()),
    }
}

/// A connected, watched owner whose first snapshot read has been answered whole: `Complete`.
fn complete(now: Instant) -> ChatOwner {
    let mut o = ChatOwner::default();
    o.on_connection(&host(), true);
    o.watch(host(), target("C1"));
    let read = o.on_evt(&host(), ChatEvt::Watched { target: target("C1"), reply: ChatReply::Done(ChatWatchRes { state: sot_protocol::ops::ChatWatchState::Needed }) }, now);
    let req = history_req(&read);
    assert!(o.on_evt(&host(), ChatEvt::History { req, reply: page(None) }, now).is_empty());
    assert_eq!(o.watch.as_ref().unwrap().fill, Fill::Complete);
    o
}

/// The loss rule: a lost chat bus marks the watched state incomplete and starts one `chat.history` read; no mutation is
/// sent; a lost bus of another kind says nothing about the chat.
#[test]
fn a_lost_chat_bus_marks_the_state_incomplete_and_reads_once() {
    let now = Instant::now();
    let mut o = complete(now);
    assert!(o.on_bus_lost(&host(), "file.changed", now + READ_PACE).is_empty());
    assert_eq!(o.watch.as_ref().unwrap().fill, Fill::Complete);
    let out = o.on_bus_lost(&host(), "chat.changed", now + READ_PACE);
    history_req(&out);
    assert_eq!(o.watch.as_ref().unwrap().fill, Fill::Reading);
    assert!(o.on_bus_lost(&host(), "chat.changed", now + READ_PACE).is_empty(), "one read at a time");
}

/// Rule 1: a post goes out only after the last one's reply.
#[test]
fn a_post_is_sent_only_after_the_last_ones_reply() {
    let mut o = complete(Instant::now());
    assert!(o.post(host(), post_req("one")).is_some());
    assert!(o.post(host(), post_req("two")).is_none(), "the first has no reply yet");
    let reply = ChatReply::Done(ChatPostRes::Posted { ts: Ts::parse("1700000000.000100").unwrap() });
    o.on_evt(&host(), ChatEvt::Posted { req: post_req("one"), reply }, Instant::now());
    assert!(matches!(o.shown, Some(PostShown::Posted(_))));
    assert!(o.post(host(), post_req("two")).is_some());
}

/// Rule 2: a status that says events were lost starts one read; losses within the pace start none, the pace's end
/// starts the owed one, and reads that never complete end in a shown gap instead of a loop.
#[test]
fn a_lost_status_resyncs_with_a_paced_and_capped_read() {
    let t0 = Instant::now();
    let mut o = complete(t0);
    let mut now = t0 + READ_PACE;
    history_req(&o.on_evt(&host(), status(true, &[]), now));
    assert!(o.on_evt(&host(), status(true, &[]), now).is_empty(), "the read in flight is the one");
    for _ in 1..MAX_READS {
        now += READ_PACE;
        assert!(o.on_evt(&host(), status(true, &[]), now - Duration::from_secs(1)).is_empty(), "inside the pace");
        history_req(&o.tick(now));
    }
    now += READ_PACE;
    assert!(o.on_evt(&host(), status(true, &[]), now).is_empty());
    assert!(o.tick(now + READ_PACE).is_empty());
    assert_eq!(o.watch.as_ref().unwrap().fill, Fill::Gap, "recovery cannot complete: the gap is shown");
}

/// Rule 3: a status entry for a channel the window does not watch, or any status once nothing is watched, starts nothing.
#[test]
fn a_status_entry_for_an_unwatched_channel_is_ignored() {
    let t0 = Instant::now();
    let now = t0 + READ_PACE;
    let mut o = complete(t0);
    assert!(o.on_evt(&host(), status(false, &["C2"]), now).is_empty());
    assert_eq!(o.watch.as_ref().unwrap().fill, Fill::Complete);
    history_req(&o.on_evt(&host(), status(false, &["C1"]), now));
    o.watch = None;
    assert!(o.on_evt(&host(), status(false, &["C1"]), now).is_empty());
}

/// Rule 4: the unsupported state is per reply, and the connection coming back clears it.
#[test]
fn the_unsupported_state_clears_when_the_connection_comes_back() {
    let mut o = complete(Instant::now());
    o.on_evt(&host(), ChatEvt::Watched { target: target("C1"), reply: ChatReply::Unsupported }, Instant::now());
    assert!(o.unsupported.contains(&host()));
    o.on_connection(&host(), false);
    assert!(o.unsupported.contains(&host()), "a lost link does not clear it");
    o.on_connection(&host(), true);
    assert!(o.unsupported.is_empty());
}

/// Rule 5: two replies to one queued read apply once.
#[test]
fn a_second_reply_to_one_read_is_not_applied() {
    let t0 = Instant::now();
    let now = t0 + READ_PACE;
    let mut o = complete(t0);
    let req = history_req(&o.on_evt(&host(), status(true, &[]), now));
    o.on_evt(&host(), ChatEvt::History { req: req.clone(), reply: page(None) }, now);
    assert_eq!(o.watch.as_ref().unwrap().fill, Fill::Complete);
    o.watch.as_mut().unwrap().fill = Fill::Needed;
    o.on_evt(&host(), ChatEvt::History { req, reply: page(None) }, now);
    assert_eq!(o.watch.as_ref().unwrap().fill, Fill::Needed, "the second reply found no read in flight");
}

/// Rule 6: a chat event that did not parse is a gap to resync from, not silence.
#[test]
fn an_unreadable_chat_event_resyncs() {
    let mut o = complete(Instant::now());
    history_req(&o.on_evt(&host(), ChatEvt::Unreadable, Instant::now() + READ_PACE));
}

/// Rule 7: a post sent while disconnected, or waiting when the link ends, reads as unknown, and is never sent.
#[test]
fn a_post_while_disconnected_reads_as_unknown() {
    let mut o = complete(Instant::now());
    assert!(o.post(host(), post_req("one")).is_some());
    o.on_connection(&host(), false);
    assert_eq!(o.shown, Some(PostShown::Unknown), "the waiting post is unknown when the link ends");
    o.shown = None;
    assert!(o.post(host(), post_req("two")).is_none());
    assert_eq!(o.shown, Some(PostShown::Unknown), "neither sent nor failed");
}
