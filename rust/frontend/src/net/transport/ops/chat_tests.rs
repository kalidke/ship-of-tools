use super::*;

fn target() -> ChatTarget {
    serde_json::from_value(serde_json::json!({ "row": "repo__box", "workspace": "T1", "channel": "C1" })).unwrap()
}

fn post_req() -> ChatPostReq {
    serde_json::from_value(serde_json::json!({
        "target": target(), "kind": "person", "name": "Ada", "text": "hello"
    }))
    .unwrap()
}

fn events(pending: &mut HashMap<u64, PendingKind>, frames: Vec<Frame>) -> Vec<String> {
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    for frame in frames {
        handle_response_frame(frame, None, pending, &evt_tx, &"h".to_string());
    }
    evt_rx.try_iter().map(|(_, e)| format!("{e:?}")).collect()
}

fn post_answer(payload: serde_json::Value) -> String {
    let mut pending = HashMap::from([(1, PendingKind::ChatPost { req: post_req() })]);
    let shown = events(&mut pending, vec![Frame::res(1, op::CHAT_POST, payload)]);
    assert!(pending.is_empty(), "the reply consumed its entry");
    shown.into_iter().next().expect("one event")
}

/// A reply that proves the post was not made says so; one that may have been made is unknown, never refused, so a
/// window never offers to send it again: a panicking handler's `handler_error`, a code the wire does not name, no code,
/// an answer that does not parse. The reply carries its request.
#[test]
fn a_post_answered_by_a_handler_error_is_unknown_never_refused() {
    for (payload, want) in [
        (serde_json::json!({ "outcome": "posted", "ts": "1700000000.000100" }), "Done(Posted"),
        (serde_json::json!({ "error": "refused", "code": "stale_row" }), "Done(Refused { code: StaleRow })"),
        (serde_json::json!({ "outcome": "refused", "code": "busy" }), "Done(Refused { code: Busy })"),
        (serde_json::json!({ "error": "a panic's text", "code": "handler_error" }), "Done(Unknown)"),
        (serde_json::json!({ "error": "no code" }), "Done(Unknown)"),
        (serde_json::json!({ "error": "x", "code": 5 }), "Done(Unknown)"),
        (serde_json::json!({ "outcome": "something_new" }), "Done(Unknown)"),
        (serde_json::json!({ "unexpected": true }), "Done(Unknown)"),
        (serde_json::json!({ "error": "unknown op: chat.post" }), "Unsupported"),
    ] {
        let shown = post_answer(payload.clone());
        let reply = shown.split("reply: ").nth(1).unwrap_or(&shown);
        assert!(shown.contains("text: \"hello\"") && reply.starts_with(want), "{payload} -> {shown}");
    }
}

/// The other replies carry what their request named, an error reply is its code (`other` when none is known), and an
/// older daemon's unknown-op answer is `Unsupported`.
#[test]
fn a_chat_reply_carries_its_request_and_its_refusal() {
    let history: ChatHistoryReq = serde_json::from_value(serde_json::json!({ "target": target(), "limit": 5 })).unwrap();
    let mut pending = HashMap::from([
        (1, PendingKind::ChatResolve { row: "repo__box".into() }),
        (2, PendingKind::ChatWatch { target: target() }),
        (3, PendingKind::ChatHistory { req: history }),
    ]);
    let shown = events(
        &mut pending,
        vec![
            Frame::res(1, op::CHAT_RESOLVE, serde_json::json!({ "state": "unmapped" })),
            Frame::res(2, op::CHAT_WATCH, serde_json::json!({ "error": "x", "code": "too_many_watches" })),
            Frame::res(3, op::CHAT_HISTORY, serde_json::json!({ "error": "unknown op: chat.history" })),
        ],
    );
    assert!(shown[0].contains("Resolved { row: \"repo__box\", reply: Done(Unmapped)"), "{}", shown[0]);
    assert!(shown[1].contains("Watched") && shown[1].contains("Refused(TooManyWatches)"), "{}", shown[1]);
    assert!(shown[2].contains("limit: 5") && shown[2].contains("Unsupported"), "{}", shown[2]);
}

/// The pushed events decode into their typed events, not the generic one; one that does not parse is `Unreadable`, a loss
/// the chat owner resyncs from, never silence.
#[test]
fn chat_events_decode_into_their_typed_events() {
    let changed = serde_json::json!({ "workspace": "T1", "channel": "C1", "put": [], "deleted": ["1700000000.000100"] });
    let status = serde_json::json!({ "service": "running", "gap": true, "resync": ["C1"] });
    let shown = events(
        &mut HashMap::new(),
        vec![
            Frame::evt(op::CHAT_CHANGED, changed),
            Frame::evt(op::CHAT_STATUS, status),
            Frame::evt(op::CHAT_STATUS, serde_json::json!({ "service": "running" })),
        ],
    );
    assert_eq!(shown.len(), 3, "{shown:?}");
    assert!(shown[0].starts_with("Chat(Changed(") && shown[1].starts_with("Chat(Status("), "{shown:?}");
    assert_eq!(shown[2], "Chat(Unreadable)", "{shown:?}");
}

/// A post that got no answer is unknown when its connection ends, once; one still waiting then is ended the same way
/// and never written, while a read waiting beside it is kept.
#[tokio::test]
async fn a_chat_post_without_its_answer_is_unknown_and_never_written_again() {
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    {
        let mut guard = PendingGuard { map: HashMap::new(), evt_tx: &evt_tx, host: "h".into() };
        guard.insert(4, PendingKind::ChatPost { req: post_req() });
    }
    let (out_tx, out_rx) = outgoing_channel();
    let mut outbox = Outbox::new(out_rx);
    out_tx.send(OutgoingReq::ChatPost { req: post_req() }).unwrap();
    out_tx.send(OutgoingReq::ChatWatch { target: target() }).unwrap();
    outbox.retire(&evt_tx, &"h".to_string()).await;
    drop(out_tx);
    let ended: Vec<String> = evt_rx.try_iter().map(|(_, e)| format!("{e:?}")).collect();
    assert!(ended.len() == 2 && ended.iter().all(|e| e.contains("Posted") && e.contains("Done(Unknown)")), "{ended:?}");
    assert!(matches!(outbox.recv().await, Some(OutgoingReq::ChatWatch { .. })), "the watch is replayable and kept");
    assert!(outbox.recv().await.is_none());
}

/// A post whose write fails (an envelope over the cap ends the session) has already been recorded, so its connection's
/// end still gives it one `Done(Unknown)`.
#[tokio::test]
async fn a_chat_post_whose_write_fails_is_still_unknown() {
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    let mut req = post_req();
    req.text = "x".repeat(2 << 20);
    let mut entries = HashMap::new();
    let kind = Some(PendingKind::ChatPost { req: req.clone() });
    let sent = send_chat(&mut Vec::new(), &mut entries, 3, op::CHAT_POST, req, kind).await;
    assert!(sent.is_err(), "the oversize envelope is refused at the write");
    drop(PendingGuard { map: entries, evt_tx: &evt_tx, host: "h".into() });
    let ended: Vec<String> = evt_rx.try_iter().map(|(_, e)| format!("{e:?}")).collect();
    assert!(matches!(ended.as_slice(), [e] if e.contains("Posted") && e.contains("Done(Unknown)")), "{ended:?}");
}
