use super::*;

fn clear_req() -> SelectionSetReq {
    serde_json::from_value(serde_json::json!({ "workspace_id": "repo__box", "selection": null })).unwrap()
}

fn set_req() -> SelectionSetReq {
    serde_json::from_value(serde_json::json!({
        "workspace_id": "repo__box",
        "selection": { "node_id": "a.txt", "start": { "line": 0, "byte": 0 }, "end": { "line": 0, "byte": 3 }, "len": 3, "digest": 1 }
    }))
    .unwrap()
}

fn answer_to(req: SelectionSetReq, payload: serde_json::Value) -> Vec<String> {
    let mut pending = HashMap::from([(1, PendingKind::SelectionSet { req })]);
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    handle_response_frame(Frame::res(1, op::SELECTION_SET, payload), None, &mut pending, &evt_tx, &"h".to_string());
    assert!(pending.is_empty(), "the reply consumed its entry");
    evt_rx.try_iter().map(|(_, e)| format!("{e:?}")).collect()
}

fn answer(payload: serde_json::Value) -> Vec<String> {
    answer_to(clear_req(), payload)
}

/// Every reply is exactly one typed event that carries the request it answers (a set and a clear differ): a state, a refusal by its code, an older
/// daemon's unknown-op answer, or an answer that does not parse.
#[test]
fn a_selection_reply_is_one_typed_event() {
    for (payload, want) in [
        (serde_json::json!({ "state": "shared" }), "Done(Shared)"),
        (serde_json::json!({ "state": "held" }), "Done(Held)"),
        (serde_json::json!({ "state": "later" }), "Done(Other)"),
        (serde_json::json!({ "error": "no", "code": "not_file_text" }), "Refused { code: Some(\"not_file_text\"), error: \"no\" }"),
        (serde_json::json!({ "error": "no" }), "Refused { code: None, error: \"no\" }"),
        (serde_json::json!({ "error": "unknown op: selection.set" }), "Unsupported"),
        (serde_json::json!({ "unexpected": true }), "Refused { code: None, error: \"unreadable reply"),
    ] {
        let shown = answer(payload.clone());
        assert_eq!(shown.len(), 1, "{payload} -> {shown:?}");
        assert!(shown[0].starts_with("Selection { req: ") && shown[0].contains("workspace_id: \"repo__box\"") && shown[0].contains(want), "{payload} -> {shown:?}");
    }
}

/// Not replayable: a set waiting when its connection ends yields one `Ended` and is never handed to a later connection; one
/// written and unanswered yields one `Ended` when the guard drops. A write that fails still leaves its entry (recorded
/// before the write), so that too ends once.
#[tokio::test]
async fn a_selection_set_ends_once_when_its_connection_ends_and_is_never_replayed() {
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    let ended = |rx: &std::sync::mpsc::Receiver<(HostKey, IncomingEvt)>| -> Vec<String> { rx.try_iter().map(|(_, e)| format!("{e:?}")).collect() };
    let (out_tx, out_rx) = outgoing_channel();
    let mut outbox = Outbox::new(out_rx);
    out_tx.send(OutgoingReq::SelectionSet { req: clear_req() }).unwrap();
    outbox.retire(&evt_tx, &"h".to_string()).await;
    drop(out_tx);
    let shown = ended(&evt_rx);
    assert!(matches!(shown.as_slice(), [e] if e.starts_with("Selection { req: ") && e.ends_with("reply: Ended }")), "{shown:?}");
    assert!(outbox.recv().await.is_none(), "not handed to a later connection");

    let mut pending = HashMap::new();
    send_selection_set(&mut Vec::new(), &mut pending, 7, clear_req()).await.unwrap();
    drop(PendingGuard { map: pending, evt_tx: &evt_tx, host: "h".into() });
    assert_eq!(ended(&evt_rx).len(), 1, "written and unanswered ends once");

    let mut entries = HashMap::new();
    let mut huge = clear_req();
    huge.workspace_id = "x".repeat(2 << 20);
    assert!(send_selection_set(&mut Vec::new(), &mut entries, 3, huge).await.is_err(), "the oversize envelope is refused at the write");
    drop(PendingGuard { map: entries, evt_tx: &evt_tx, host: "h".into() });
    assert_eq!(ended(&evt_rx).len(), 1, "a failed write still ends once");
}

#[test]
fn a_reply_to_a_set_differs_from_the_reply_to_a_clear() {
    let state = serde_json::json!({ "state": "shared" });
    let (set, clear) = (answer_to(set_req(), state.clone()), answer_to(clear_req(), state));
    assert_ne!(set, clear);
    assert!(set[0].contains("a.txt") && !clear[0].contains("a.txt"), "{set:?} {clear:?}");
}
