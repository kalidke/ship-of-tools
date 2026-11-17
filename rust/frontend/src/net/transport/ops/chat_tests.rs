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
/// and never written. A watch or a snapshot read waiting beside it is dropped too, with no event, so a later connection
/// never writes it ahead of the owner's new watch; a resolve is kept.
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
    let read = sot_protocol::ops::ChatHistoryReq { target: target(), thread: None, after: None, upper: None, limit: 20, cursor: None };
    out_tx.send(OutgoingReq::ChatHistory { req: read }).unwrap();
    out_tx.send(OutgoingReq::ChatResolve { row: "r".into() }).unwrap();
    outbox.retire(&evt_tx, &"h".to_string()).await;
    drop(out_tx);
    let ended: Vec<String> = evt_rx.try_iter().map(|(_, e)| format!("{e:?}")).collect();
    assert!(ended.len() == 2 && ended.iter().all(|e| e.contains("Posted") && e.contains("Done(Unknown)")), "{ended:?}");
    assert!(matches!(outbox.recv().await, Some(OutgoingReq::ChatResolve { .. })), "only the resolve is kept");
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

/// Fixture tokens built at run time (a literal would trip the secret scan); each carries a marker no other text holds.
fn token(kind: &str) -> sot_protocol::ops::ChatToken {
    serde_json::from_value(serde_json::json!(format!("{kind}-{}", "7".repeat(24)))).unwrap()
}

fn marker_in(text: &str) -> bool {
    text.contains(&"7".repeat(24))
}

/// A token the window sends reaches the daemon whole and nowhere else: not the request's `Debug`, not the pending
/// entry, not a log line of the write, not an event of any reply to it. A failure names the place, never the text.
#[tokio::test]
async fn a_connect_formats_and_logs_no_token_byte() {
    let log = sot_platform::test_log::capture();
    let req = OutgoingReq::ChatConnect { bot_token: token("xoxb"), user_token: token("xoxp"), app_token: token("xapp") };
    assert!(!marker_in(&format!("{req:?}")), "the request's Debug holds a token");
    let mut wire = Vec::new();
    let mut pending = HashMap::new();
    send_request(&mut wire, &mut pending, 7, req).await.unwrap();
    let line = String::from_utf8(wire).unwrap();
    assert!(line.contains("\"op\":\"chat.connect\"") && marker_in(&line), "the daemon is sent the tokens");
    assert_eq!(line.matches(&"7".repeat(24)).count(), 3, "all three tokens, whole, in the one request");
    assert!(!marker_in(&format!("{pending:?}")), "the pending entry holds a token");
    let replies = [
        serde_json::json!({ "workspace": "T1" }),
        serde_json::json!({ "error": "x", "code": "bot_token_refused" }),
        serde_json::json!({ "error": "x", "code": "user_token_refused" }),
        serde_json::json!({ "error": "handler failed", "code": "handler_error" }),
        serde_json::json!({ "error": "unknown op: chat.connect" }),
    ];
    for (n, reply) in replies.into_iter().enumerate() {
        let mut pending = HashMap::from([(1, PendingKind::ChatConnect)]);
        let shown = events(&mut pending, vec![Frame::res(1, op::CHAT_CONNECT, reply)]);
        assert!(!marker_in(&format!("{shown:?}")), "an event of reply {n} holds a token");
    }
    assert!(!marker_in(&log.text()), "a log line holds a token");
    assert!(log.text().contains("chat.connect"), "the write was logged by its op name");
}

/// A connect or a pair that got no answer is unknown once when its connection ends, and one still waiting then is ended
/// the same way and never written; a channel read waiting beside them is kept for the next connection.
#[tokio::test]
async fn a_connect_or_pair_without_its_reply_is_unknown_once_and_never_written_again() {
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    let channel: sot_protocol::ops::ChatChannel = serde_json::from_value(serde_json::json!("C9")).unwrap();
    {
        let mut guard = PendingGuard { map: HashMap::new(), evt_tx: &evt_tx, host: "h".into() };
        guard.insert(4, PendingKind::ChatConnect);
        guard.insert(5, PendingKind::ChatPair { row: "r1".into(), channel: Some(channel.clone()) });
        guard.insert(6, PendingKind::ChatChannels);
    }
    let (out_tx, out_rx) = outgoing_channel();
    let mut outbox = Outbox::new(out_rx);
    out_tx.send(OutgoingReq::ChatConnect { bot_token: token("xoxb"), user_token: token("xoxp"), app_token: token("xapp") }).unwrap();
    out_tx.send(OutgoingReq::ChatPair { row: "r2".into(), channel: None }).unwrap();
    out_tx.send(OutgoingReq::ChatChannels).unwrap();
    outbox.retire(&evt_tx, &"h".to_string()).await;
    drop(out_tx);
    let mut ended: Vec<String> = evt_rx.try_iter().map(|(_, e)| format!("{e:?}")).collect();
    ended.sort();
    assert_eq!(ended.len(), 4, "{ended:?}");
    assert_eq!(ended.iter().filter(|e| e.starts_with("Chat(ConnectUnknown)")).count(), 2, "{ended:?}");
    assert!(ended.iter().any(|e| e.contains("PairUnknown { row: \"r1\", channel: Some(ChatChannel(\"C9\")) }")), "{ended:?}");
    assert!(ended.iter().any(|e| e.contains("PairUnknown { row: \"r2\", channel: None }")), "{ended:?}");
    assert!(matches!(outbox.recv().await, Some(OutgoingReq::ChatChannels)), "only the read is kept");
    assert!(outbox.recv().await.is_none());
}

/// A reply carries the request it answers (a connect's names nothing of its tokens); an error reply is its code; an
/// older daemon's unknown-op answer is `Unsupported`; a connect or pair is unknown unless the reply proves it was not
/// carried out.
#[test]
fn the_setup_replies_carry_their_request_and_refusal() {
    let channel: sot_protocol::ops::ChatChannel = serde_json::from_value(serde_json::json!("C9")).unwrap();
    let mut pending = HashMap::from([
        (1, PendingKind::ChatConnect),
        (2, PendingKind::ChatChannels),
        (3, PendingKind::ChatPair { row: "r".into(), channel: Some(channel.clone()) }),
        (4, PendingKind::ChatPair { row: "r".into(), channel: None }),
        (5, PendingKind::ChatConnect),
        (6, PendingKind::ChatPair { row: "r".into(), channel: None }),
        (7, PendingKind::ChatChannels),
        (8, PendingKind::ChatConnect),
    ]);
    let list = serde_json::json!({ "channels": [{ "id": "C9", "name": "lab", "private": false, "member": true }], "more": true });
    let shown = events(
        &mut pending,
        vec![
            Frame::res(1, op::CHAT_CONNECT, serde_json::json!({ "workspace": "T1" })),
            Frame::res(2, op::CHAT_CHANNELS, list),
            Frame::res(3, op::CHAT_PAIR, serde_json::json!({ "error": "x", "code": "channel_taken" })),
            Frame::res(4, op::CHAT_PAIR, serde_json::json!({})),
            Frame::res(5, op::CHAT_CONNECT, serde_json::json!({ "error": "x", "code": "app_token_refused" })),
            Frame::res(6, op::CHAT_PAIR, serde_json::json!({ "error": "a panic's text", "code": "handler_error" })),
            Frame::res(7, op::CHAT_CHANNELS, serde_json::json!({ "error": "unknown op: chat.channels" })),
            Frame::res(8, op::CHAT_CONNECT, serde_json::json!({ "workspace": 5 })),
        ],
    );
    assert!(pending.is_empty(), "every reply consumed its entry");
    assert!(shown[0].starts_with("Chat(Connect { reply: Done(ChatConnectRes { workspace: \"T1\" }) })"), "{}", shown[0]);
    assert!(shown[1].starts_with("Chat(Channels { reply: Done(") && shown[1].contains("more: true"), "{}", shown[1]);
    assert!(shown[2].contains("Paired { row: \"r\", channel: Some(ChatChannel(\"C9\")), reply:") && shown[2].contains("Refused(ChannelTaken)"), "{}", shown[2]);
    assert!(shown[3].contains("Paired { row: \"r\", channel: None, reply: Done("), "{}", shown[3]);
    assert!(shown[4].starts_with("Chat(Connect { reply: Refused(AppTokenRefused) })"), "{}", shown[4]);
    assert_eq!(shown[5], "Chat(PairUnknown { row: \"r\", channel: None })");
    assert!(shown[6].starts_with("Chat(Channels { reply: Unsupported })"), "{}", shown[6]);
    assert_eq!(shown[7], "Chat(ConnectUnknown)");
}

fn file_req(variant: &str) -> ChatFileReq {
    serde_json::from_value(serde_json::json!({ "target": target(), "file": "F0PIC", "variant": variant })).unwrap()
}

fn file_answer(len: usize) -> serde_json::Value {
    serde_json::json!({ "mime": "image/png", "blob": { "len": len, "mime": "image/png" } })
}

/// A `chat.file` reply carries the request it answers and the frame's blob as its bytes; a blob past its own variant's
/// bound (a thumbnail's `MAX_THUMB`, though within the wire's `MAX_CHAT_FILE`) or other than it declares is
/// `Refused(Failed)`; a refusal is its code, and an older daemon's unknown-op answer is `Unsupported`.
#[test]
fn a_file_reply_carries_its_request_and_its_bytes() {
    let over = MAX_THUMB + 1;
    let cases: Vec<(&str, serde_json::Value, Option<Vec<u8>>, &str)> = vec![
        ("thumb", file_answer(3), Some(b"png".to_vec()), "Done(ChatPicture { mime: \"image/png\", bytes: 3 })"),
        ("thumb", file_answer(MAX_THUMB), Some(vec![0; MAX_THUMB]), "Done(ChatPicture { mime: \"image/png\", bytes: 262144 })"),
        ("original", file_answer(over), Some(vec![0; over]), "Done(ChatPicture { mime: \"image/png\", bytes: 262145 })"),
        ("thumb", file_answer(over), Some(vec![0; over]), "Refused(Failed)"),
        ("thumb", file_answer(4), Some(b"png".to_vec()), "Refused(Failed)"),
        ("thumb", file_answer(3), None, "Refused(Failed)"),
        ("original", serde_json::json!({ "error": "x", "code": "not_an_image" }), None, "Refused(NotAnImage)"),
        ("original", serde_json::json!({ "error": "x", "code": "too_large" }), None, "Refused(TooLarge)"),
        ("thumb", serde_json::json!({ "error": "unknown op: chat.file" }), None, "Unsupported"),
    ];
    for (variant, payload, blob, want) in cases {
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let mut pending = HashMap::from([(1, PendingKind::ChatFile { req: file_req(variant) })]);
        handle_response_frame(Frame::res(1, op::CHAT_FILE, payload.clone()), blob, &mut pending, &evt_tx, &"h".to_string());
        assert!(pending.is_empty(), "the reply consumed its entry");
        let shown: Vec<String> = evt_rx.try_iter().map(|(_, e)| format!("{e:?}")).collect();
        assert_eq!(shown.len(), 1, "{shown:?}");
        let reply = shown[0].split("reply: ").nth(1).unwrap_or(&shown[0]);
        let named = if variant == "thumb" { "variant: Thumb" } else { "variant: Original" };
        assert!(shown[0].contains("file: \"F0PIC\"") && shown[0].contains(named), "{shown:?}");
        assert!(reply.starts_with(want), "{variant} {payload} -> {}", shown[0]);
    }
}

/// A picture read waiting for its answer when its connection ends gives no event, and one still queued then is dropped
/// unwritten: the owner asks again at `Connected`, never a later connection on its own.
#[tokio::test]
async fn a_file_read_without_its_answer_ends_with_no_event_and_is_never_written_again() {
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    {
        let mut guard = PendingGuard { map: HashMap::new(), evt_tx: &evt_tx, host: "h".into() };
        guard.insert(4, PendingKind::ChatFile { req: file_req("original") });
    }
    let (out_tx, out_rx) = outgoing_channel();
    let mut outbox = Outbox::new(out_rx);
    out_tx.send(OutgoingReq::ChatFile { req: file_req("thumb") }).unwrap();
    out_tx.send(OutgoingReq::ChatResolve { row: "r".into() }).unwrap();
    outbox.retire(&evt_tx, &"h".to_string()).await;
    drop(out_tx);
    assert_eq!(evt_rx.try_iter().count(), 0, "a picture read ended with an event");
    assert!(matches!(outbox.recv().await, Some(OutgoingReq::ChatResolve { .. })), "only the resolve is kept");
    assert!(outbox.recv().await.is_none());
}

fn file_post_req() -> ChatPostFileReq {
    serde_json::from_value(serde_json::json!({
        "target": target(), "kind": "person", "name": "", "node_id": "files:fig.png", "comment": "the fit"
    }))
    .unwrap()
}

/// A file post is a post: a reply that proves it was not shared says so, one that may have shared it is unknown, and an
/// older daemon's unknown-op answer is `Unsupported`. A file post waiting for its answer when its connection ends, and
/// one still queued then, are each unknown once, and the queued one is never written.
#[tokio::test]
async fn a_file_post_without_its_answer_is_unknown_and_never_written_again() {
    for (payload, want) in [
        (serde_json::json!({ "outcome": "posted", "file": "F0FILE" }), "Done(Posted { file: \"F0FILE\", ts: None })"),
        (serde_json::json!({ "error": "x", "code": "not_a_file" }), "Done(Refused { code: NotAFile })"),
        (serde_json::json!({ "error": "a panic's text", "code": "handler_error" }), "Done(Unknown)"),
        (serde_json::json!({ "unexpected": true }), "Done(Unknown)"),
        (serde_json::json!({ "error": "unknown op: chat.post_file" }), "Unsupported"),
    ] {
        let mut pending = HashMap::from([(1, PendingKind::ChatPostFile { req: file_post_req() })]);
        let shown = events(&mut pending, vec![Frame::res(1, op::CHAT_POST_FILE, payload.clone())]);
        assert!(pending.is_empty(), "the reply consumed its entry");
        let reply = shown[0].split("reply: ").nth(1).unwrap_or(&shown[0]);
        assert!(shown[0].contains("node_id: \"files:fig.png\"") && reply.starts_with(want), "{payload} -> {shown:?}");
    }
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    {
        let mut guard = PendingGuard { map: HashMap::new(), evt_tx: &evt_tx, host: "h".into() };
        guard.insert(4, PendingKind::ChatPostFile { req: file_post_req() });
    }
    let (out_tx, out_rx) = outgoing_channel();
    let mut outbox = Outbox::new(out_rx);
    out_tx.send(OutgoingReq::ChatPostFile { req: file_post_req() }).unwrap();
    out_tx.send(OutgoingReq::ChatResolve { row: "r".into() }).unwrap();
    outbox.retire(&evt_tx, &"h".to_string()).await;
    drop(out_tx);
    let ended: Vec<String> = evt_rx.try_iter().map(|(_, e)| format!("{e:?}")).collect();
    assert_eq!(ended.len(), 2, "{ended:?}");
    assert!(ended.iter().all(|e| e.starts_with("Chat(FilePosted") && e.ends_with("reply: Done(Unknown) })")), "{ended:?}");
    assert!(matches!(outbox.recv().await, Some(OutgoingReq::ChatResolve { .. })), "only the resolve is kept");
    assert!(outbox.recv().await.is_none());
}
