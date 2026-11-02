use super::*;

#[test]
fn delivery_outcomes_never_turn_uncertainty_into_success() {
    for (result, expected) in [
        (Ok(WakeOutcome::Woke), "sent"),
        (Ok(WakeOutcome::NotFree { reason: "dialog", row: String::new(), draft: String::new() }), "not_free"),
        (Ok(WakeOutcome::TypedNoEnter { reason: "wrapped", border: String::new(), row: String::new() }), "typed_not_sent"),
        (Ok(WakeOutcome::EnterRefused { detail: "stale".into() }), "typed_not_sent"),
        (Ok(WakeOutcome::Unconfirmed { step: "text", detail: "stale run".into() }), "not_free"),
        (Ok(WakeOutcome::Unconfirmed { step: "enter", detail: "lost answer".into() }), "unknown"),
        (Ok(WakeOutcome::TextUnknown { detail: "lost text answer".into() }), "unknown"),
        (Ok(WakeOutcome::RunChanged), "not_free"),
        (Err(HeadlessError { phase: "record", detail: "timeout".into(), submitted: true }), "unknown"),
        (Err(HeadlessError { phase: "attach", detail: "absent".into(), submitted: false }), "not_free"),
    ] {
        let (outcome, detail, _) = delivery(result);
        assert_eq!(outcome, expected);
        assert!(!detail.is_empty());
        let frames = answer(42, outcome, &detail);
        assert_eq!(frames.len(), 1);
        let (frame, blob) = &frames[0];
        assert_eq!(frame.id, 42);
        assert_eq!(frame.op, "page.reply");
        assert_eq!(frame.payload, json!({"outcome": expected, "detail": detail}));
        assert!(blob.is_none());
    }
}

/// A refusal at a draft quotes what the box holds, cut and with a direction character replaced, logs the row the gate
/// read, and is still a draft: refused, never held. Any other refusal carries the gate's words alone.
#[test]
fn a_draft_refusal_quotes_the_box_and_is_still_refused() {
    let not_free = |reason, draft: &str| delivery(Ok(WakeOutcome::NotFree { reason, row: "col 3: U+276F U+00A0 w".into(), draft: draft.into() }));
    let (outcome, detail, row) = not_free("cursor not at the prompt", "w");
    assert_eq!((outcome, detail.as_str(), row.as_str()), ("not_free", "cursor not at the prompt; the box holds \"w\"", "col 3: U+276F U+00A0 w"));
    assert_eq!(not_free("input not empty", &"a".repeat(50)).1, format!("input not empty; the box holds \"{}\u{2026}\"", "a".repeat(40)));
    assert_eq!(not_free("input not empty", "x\u{202e}y").1, "input not empty; the box holds \"x\u{fffd}y\"");
    assert_eq!(not_free("stop hook running", "w").1, "stop hook running");
    assert_eq!(not_free("cursor not at the prompt", "").1, "cursor not at the prompt");
    let held = Held { request_id: "draft-quote".into(), workspace_id: "ws-draft-quote".into(), handle: "h".into(),
        line: "[sot-page] a".into(), question: String::new(), last_attempt: None };
    let receipt = settle(held, outcome, detail);
    assert_eq!((receipt.outcome, receipt.detail.as_str()), ("refused", "draft: cursor not at the prompt; the box holds \"w\""));
}
