use super::*;

#[test]
fn delivery_outcomes_never_turn_uncertainty_into_success() {
    for (result, expected) in [
        (Ok(WakeOutcome::Woke), "sent"),
        (Ok(WakeOutcome::NotFree { reason: "dialog", row: String::new() }), "not_free"),
        (Ok(WakeOutcome::TypedNoEnter { reason: "wrapped", border: String::new(), row: String::new() }), "typed_not_sent"),
        (Ok(WakeOutcome::EnterRefused { detail: "stale".into() }), "typed_not_sent"),
        (Ok(WakeOutcome::Unconfirmed { step: "text", detail: "stale run".into() }), "not_free"),
        (Ok(WakeOutcome::Unconfirmed { step: "enter", detail: "lost answer".into() }), "unknown"),
        (Ok(WakeOutcome::TextUnknown { detail: "lost text answer".into() }), "unknown"),
        (Ok(WakeOutcome::RunChanged), "not_free"),
        (Err(HeadlessError { phase: "record", detail: "timeout".into(), submitted: true }), "unknown"),
        (Err(HeadlessError { phase: "attach", detail: "absent".into(), submitted: false }), "not_free"),
    ] {
        let (outcome, detail) = delivery(result);
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
