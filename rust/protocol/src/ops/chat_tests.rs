use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use super::*;
use crate::chat::{ChatAuthor, Origin};

fn ts(s: &str) -> Ts {
    Ts::parse(s).unwrap()
}

fn target() -> ChatTarget {
    ChatTarget { row: "ws-a-1".into(), workspace: "T1".into(), channel: ChatChannel::try_from("C1".to_string()).unwrap() }
}

fn target_json() -> Value {
    json!({"row": "ws-a-1", "workspace": "T1", "channel": "C1"})
}

fn record(text: &str) -> ChatRecord {
    ChatRecord {
        workspace: "T1".into(),
        channel: "C1".into(),
        ts: ts("1.000001"),
        thread_ts: ts("1.000001"),
        author: ChatAuthor { id: "U1".into(), name: "Ada".into() },
        origin: Origin::Member,
        text: text.into(),
        truncated: false,
        reply_count: 0,
        files: vec![],
        latest_reply: None,
        limited: false,
        omitted: false,
        files_omitted: 0,
    }
}

/// The value is exactly `shape`, the shape decodes back to the value, and a field added later (in the payload and in
/// its target) is ignored: every chat payload grows by defaulted fields, requests included.
fn pinned<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: T, shape: Value) {
    assert_eq!(serde_json::to_value(&value).unwrap(), shape);
    assert_eq!(serde_json::from_value::<T>(shape.clone()).unwrap(), value);
    let mut grown = shape;
    grown["zz"] = json!(1);
    if let Some(t) = grown.get_mut("target") {
        t["zz"] = json!(1);
    }
    assert_eq!(serde_json::from_value::<T>(grown).unwrap(), value, "an added field is ignored");
}

#[test]
fn the_chat_ops_are_declared_in_the_one_table() {
    for name in ["chat.resolve", "chat.watch", "chat.unwatch", "chat.history", "chat.post", "chat.changed", "chat.status"] {
        assert!(op::ALL.contains(&name), "{name} is not in op::ALL");
    }
}

#[test]
fn every_request_reply_and_event_keeps_its_shape() {
    pinned(ChatResolveReq { row: "ws-a-1".into() }, json!({"row": "ws-a-1"}));
    pinned(
        ChatResolveRes::Mapped { workspace: "T1".into(), channel: target().channel, delivers: false },
        json!({"state": "mapped", "workspace": "T1", "channel": "C1", "delivers": false}),
    );
    pinned(ChatResolveRes::Unmapped, json!({"state": "unmapped"}));
    pinned(ChatWatchReq { target: target() }, json!({"target": target_json()}));
    pinned(ChatWatchRes { state: ChatWatchState::Pending }, json!({"state": "pending"}));
    pinned(ChatUnwatchReq { target: target() }, json!({"target": target_json()}));
    pinned(ChatUnwatchRes {}, json!({}));
    pinned(
        ChatHistoryReq {
            target: target(),
            thread: Some(ts("1.000001")),
            after: None,
            upper: Some(ChatUpper::Until(ts("2.000002"))),
            limit: 20,
            cursor: Some("c".into()),
        },
        json!({"target": target_json(), "thread": "1.000001", "after": null, "upper": {"until": "2.000002"}, "limit": 20, "cursor": "c"}),
    );
    pinned(
        ChatHistoryRes { records: vec![], next: None, limited: false },
        json!({"records": []}),
    );
    pinned(
        ChatPostReq { target: target(), thread: None, kind: ChatPostKind::Row, name: "r".into(), text: "hi".into() },
        json!({"target": target_json(), "thread": null, "kind": "row", "name": "r", "text": "hi"}),
    );
    pinned(ChatPostRes::Posted { ts: ts("3.000003") }, json!({"outcome": "posted", "ts": "3.000003"}));
    pinned(ChatPostRes::Refused { code: ChatCode::StaleRow }, json!({"outcome": "refused", "code": "stale_row"}));
    pinned(
        ChatPostRes::Limited { retry_after_secs: 30, assumed: true },
        json!({"outcome": "limited", "retry_after_secs": 30, "assumed": true}),
    );
    pinned(ChatPostRes::Unknown, json!({"outcome": "unknown"}));
    pinned(
        ChatChangedEvt { workspace: "T1".into(), channel: target().channel, put: vec![record("a")], deleted: vec![ts("1.000001")] },
        json!({"workspace": "T1", "channel": "C1", "put": [serde_json::to_value(record("a")).unwrap()], "deleted": ["1.000001"]}),
    );
    pinned(
        ChatStatusEvt { service: ChatService::Running, gap: true, resync: vec![target().channel] },
        json!({"service": "running", "gap": true, "resync": ["C1"]}),
    );
    pinned(ChatRefusal { code: Some(ChatCode::TooManyWatches) }, json!({"code": "too_many_watches"}));
}

#[test]
fn a_page_carries_records_that_round_trip() {
    let page = ChatHistoryRes { records: vec![record("a"), record("b")], next: Some("n".into()), limited: true };
    let back: ChatHistoryRes = serde_json::from_str(&serde_json::to_string(&page).unwrap()).unwrap();
    assert_eq!(back, page);
}

#[test]
fn a_code_added_later_reads_as_other() {
    let r: ChatRefusal = serde_json::from_value(json!({"error": "text", "code": "from_the_future"})).unwrap();
    assert_eq!(r.code, Some(ChatCode::Other));
}

#[test]
fn an_older_daemons_unknown_op_refusal_reads_as_a_refusal_with_no_code() {
    let r: ChatRefusal = serde_json::from_value(json!({"error": "unknown op: chat.post"})).unwrap();
    assert_eq!(r.code, None);
}

#[test]
fn a_phase_or_outcome_added_later_reads_as_the_safe_one() {
    let status: ChatStatusEvt = serde_json::from_value(json!({"service": "paused", "gap": false, "resync": ["C1"]})).unwrap();
    assert_eq!((status.service, status.resync.len()), (ChatService::Other, 1));
    let post: ChatPostRes = serde_json::from_value(json!({"outcome": "queued", "ts": "1.000001"})).unwrap();
    assert_eq!(post, ChatPostRes::Unknown);
}

#[test]
fn a_wait_is_clamped_to_an_hour() {
    let limited = |n: u64| json!({"outcome": "limited", "retry_after_secs": n, "assumed": false});
    let wait = |n: u64| match serde_json::from_value::<ChatPostRes>(limited(n)).unwrap() {
        ChatPostRes::Limited { retry_after_secs, .. } => retry_after_secs,
        other => panic!("{other:?}"),
    };
    assert_eq!((wait(30), wait(MAX_RETRY_SECS), wait(MAX_RETRY_SECS + 1), wait(u64::MAX)), (30, 3600, 3600, 3600));
}

#[test]
fn a_state_added_later_reads_as_a_safe_one() {
    let watch: ChatWatchRes = serde_json::from_value(json!({"state": "stale"})).unwrap();
    assert_eq!(watch.state, ChatWatchState::Other);
    let resolve: ChatResolveRes = serde_json::from_value(json!({"state": "disabled"})).unwrap();
    assert_eq!(resolve, ChatResolveRes::Invalid);
}

#[test]
fn a_record_with_an_origin_added_later_does_not_spoil_its_page() {
    let mut rec = serde_json::to_value(record("x")).unwrap();
    rec["origin"] = json!("webhook");
    let page: ChatHistoryRes = serde_json::from_value(json!({"records": [rec, serde_json::to_value(record("y")).unwrap()]})).unwrap();
    assert_eq!(page.records[0].origin, Origin::Other);
    assert_eq!(page.records.len(), 2);
}

#[test]
fn a_legal_page_fits_the_envelope() {
    let page = MAX_PAGE_RECORDS * crate::chat::MAX_RECORD_BYTES + MAX_CURSOR;
    assert!(page < crate::codec::MAX_ENVELOPE_BYTES, "{page} bytes of a full page");
}

#[test]
fn a_number_where_a_string_belongs_is_refused() {
    let post = json!({"target": target_json(), "kind": "row", "name": 5, "text": "t"});
    assert!(serde_json::from_value::<ChatPostReq>(post).is_err());
    let history = json!({"target": target_json(), "limit": 5, "after": 1.5});
    assert!(serde_json::from_value::<ChatHistoryReq>(history).is_err());
}

#[test]
fn a_timestamp_with_the_wrong_digits_is_refused_in_a_request() {
    let history = json!({"target": target_json(), "limit": 5, "after": "1700000000.1234567"});
    assert!(serde_json::from_value::<ChatHistoryReq>(history).is_err());
    let post = json!({"target": target_json(), "thread": "1.12345", "kind": "row", "name": "n", "text": "t"});
    assert!(serde_json::from_value::<ChatPostReq>(post).is_err());
}

#[test]
fn both_upper_bounds_cannot_be_asked_for() {
    let both = json!({"target": target_json(), "limit": 5, "upper": {"before": "1.000001", "until": "2.000002"}});
    assert!(serde_json::from_value::<ChatHistoryReq>(both).is_err());
}

#[test]
fn a_limit_is_one_to_twenty() {
    for (n, ok) in [(0, false), (1, true), (20, true), (21, false)] {
        let history = json!({"target": target_json(), "limit": n});
        assert_eq!(serde_json::from_value::<ChatHistoryReq>(history).is_ok(), ok, "limit {n}");
    }
}

#[test]
fn a_cursor_a_text_and_a_name_stop_at_their_bounds() {
    let history = |c: usize| json!({"target": target_json(), "limit": 5, "cursor": "c".repeat(c)});
    assert!(serde_json::from_value::<ChatHistoryReq>(history(1024)).is_ok());
    assert!(serde_json::from_value::<ChatHistoryReq>(history(1025)).is_err());
    let res = |c: usize| json!({"records": [], "next": "c".repeat(c)});
    assert!(serde_json::from_value::<ChatHistoryRes>(res(1025)).is_err());
    let post = |name: usize, text: usize| {
        json!({"target": target_json(), "kind": "row", "name": "n".repeat(name), "text": "t".repeat(text)})
    };
    assert!(serde_json::from_value::<ChatPostReq>(post(160, 8192)).is_ok());
    assert!(serde_json::from_value::<ChatPostReq>(post(160, 8193)).is_err());
    assert!(serde_json::from_value::<ChatPostReq>(post(161, 1)).is_err());
}

#[test]
fn a_target_names_a_real_channel_and_bounded_ids() {
    for good in ["C1", "c_1-X"] {
        let t = json!({"row": "r", "workspace": "T1", "channel": good});
        assert!(serde_json::from_value::<ChatTarget>(t).is_ok(), "{good:?}");
    }
    for bad in ["", "c 1", "c/1", "cé", &"c".repeat(65)] {
        let t = json!({"row": "r", "workspace": "T1", "channel": bad});
        assert!(serde_json::from_value::<ChatTarget>(t).is_err(), "{bad:?}");
    }
    let long_row = json!({"row": "r".repeat(257), "workspace": "T1", "channel": "C1"});
    assert!(serde_json::from_value::<ChatTarget>(long_row).is_err());
    let long_workspace = json!({"row": "r", "workspace": "T".repeat(65), "channel": "C1"});
    assert!(serde_json::from_value::<ChatTarget>(long_workspace).is_err());
    let long = "W".repeat(65);
    let mapped = json!({"state": "mapped", "workspace": long, "channel": "C1", "delivers": true});
    assert!(serde_json::from_value::<ChatResolveRes>(mapped).is_err());
    let changed = json!({"workspace": long, "channel": "C1", "put": [], "deleted": []});
    assert!(serde_json::from_value::<ChatChangedEvt>(changed).is_err());
}

#[test]
fn a_list_of_records_stops_at_a_page_and_an_event_at_its_bounds() {
    let page = |n: usize| json!({"records": vec![serde_json::to_value(record("x")).unwrap(); n]});
    assert!(serde_json::from_value::<ChatHistoryRes>(page(21)).is_ok());
    assert!(serde_json::from_value::<ChatHistoryRes>(page(22)).is_err());
    let put = |n: usize| json!({"workspace": "T", "channel": "C1", "put": vec![serde_json::to_value(record("x")).unwrap(); n], "deleted": []});
    assert!(serde_json::from_value::<ChatChangedEvt>(put(21)).is_ok());
    assert!(serde_json::from_value::<ChatChangedEvt>(put(22)).is_err());
    let event = |n: usize| json!({"workspace": "T", "channel": "C1", "put": [], "deleted": vec!["1.000001"; n]});
    assert!(serde_json::from_value::<ChatChangedEvt>(event(21)).is_ok());
    assert!(serde_json::from_value::<ChatChangedEvt>(event(22)).is_err());
    let status = |n: usize| json!({"service": "running", "gap": false, "resync": vec!["C1"; n]});
    assert!(serde_json::from_value::<ChatStatusEvt>(status(32)).is_ok());
    assert!(serde_json::from_value::<ChatStatusEvt>(status(33)).is_err());
}

#[test]
fn every_code_has_its_one_text() {
    let table = [
        (ChatCode::Unavailable, "the chat is not available here"),
        (ChatCode::Unmapped, "the row's folder maps to no channel"),
        (ChatCode::StaleRow, "the row is gone or was replaced"),
        (ChatCode::StaleMapping, "the row now maps to another channel"),
        (ChatCode::BadRequest, "the request is not valid"),
        (ChatCode::TooManyWatches, "too many channels are watched"),
        (ChatCode::Limited, "the chat service is rate limited"),
        (ChatCode::Busy, "the chat service is busy"),
        (ChatCode::Failed, "the chat request failed"),
        (ChatCode::Other, "the chat request failed"),
    ];
    for (code, words) in table {
        assert_eq!(code.text(), words, "{code:?}");
    }
}
