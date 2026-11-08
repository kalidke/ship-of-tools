use super::*;

fn clear_req() -> SelectionSetReq {
    serde_json::from_value(serde_json::json!({ "workspace_id": "repo__box", "selection": null })).unwrap()
}

fn answer(payload: serde_json::Value) -> Vec<String> {
    let mut pending = HashMap::from([(1, PendingKind::SelectionSet { workspace_id: "repo__box".into() })]);
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    handle_response_frame(Frame::res(1, op::SELECTION_SET, payload), None, &mut pending, &evt_tx, &"h".to_string());
    assert!(pending.is_empty(), "the reply consumed its entry");
    evt_rx.try_iter().map(|(_, e)| format!("{e:?}")).collect()
}

/// Every reply is exactly one typed event that names the row its request named: a state, a refusal by its code, an older
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
        assert!(shown[0].starts_with("Selection { workspace_id: \"repo__box\", reply: ") && shown[0].contains(want), "{payload} -> {shown:?}");
    }
}

/// A selection is state: one still waiting when its connection ends is kept for the next, not ended, and the pending
/// entry is recorded before the write.
#[tokio::test]
async fn a_selection_set_is_replayed_after_a_reconnect_with_what_it_carried() {
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    let (out_tx, out_rx) = outgoing_channel();
    let mut outbox = Outbox::new(out_rx);
    out_tx.send(OutgoingReq::SelectionSet { req: clear_req() }).unwrap();
    outbox.retire(&evt_tx, &"h".to_string()).await;
    drop(out_tx);
    assert!(evt_rx.try_iter().next().is_none(), "no event ends it");
    let Some(OutgoingReq::SelectionSet { req }) = outbox.recv().await else { panic!("kept for the next connection") };
    assert_eq!(req, clear_req());
    let mut pending = HashMap::new();
    let mut wire = Vec::new();
    send_selection_set(&mut wire, &mut pending, 7, req).await.unwrap();
    assert!(matches!(pending.get(&7), Some(PendingKind::SelectionSet { workspace_id }) if workspace_id == "repo__box"));
    assert!(String::from_utf8_lossy(&wire).contains("selection.set"));
}
