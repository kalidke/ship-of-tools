use super::*;

/// What `on_repl_eval` emits for one reply payload.
fn emitted(payload: serde_json::Value) -> Vec<IncomingEvt> {
    let got = std::cell::RefCell::new(Vec::new());
    on_repl_eval(Frame::res(9, op::REPL_EVAL, payload), &|e| got.borrow_mut().push(e), 8);
    got.into_inner()
}

/// Every error reply to an eval ends it with one error frame naming the daemon's words and code; the acceptance
/// ack still only acknowledges.
#[test]
fn an_error_reply_ends_the_eval_with_one_error_frame() {
    for (code, error) in [
        ("repl_busy", "the REPL's queue is full"),
        ("repl_eval_failed", "repl supervisor channel closed"),
        ("unknown_workspace", "unknown workspace: Some(\"gone\")"),
        ("handler_error", "repl.eval payload"),
    ] {
        let events = emitted(serde_json::json!({ "error": error, "code": code }));
        let shown: Vec<String> = events
            .iter()
            .map(|e| match e {
                IncomingEvt::ReplEvalDone { eval_id, elapsed_ms, frames } => match frames.as_slice() {
                    [ReplFrame::Error { message, stacktrace }] if stacktrace.is_empty() => {
                        format!("done eval_id={eval_id} elapsed_ms={elapsed_ms} error={message:?}")
                    }
                    other => format!("done eval_id={eval_id} frames={other:?}"),
                },
                _ => "another event".to_string(),
            })
            .collect();
        println!("repl-eval-error: code={code} events={shown:?}");
        assert_eq!(shown, [format!("done eval_id=8 elapsed_ms=0 error={:?}", format!("{error} [{code}]"))]);
    }
    let ack = emitted(serde_json::json!({ "eval_id": 8, "elapsed_ms": 0, "frames": [], "accepted": true }));
    assert!(
        matches!(ack.as_slice(), [IncomingEvt::ReplEvalDone { eval_id: 8, elapsed_ms: 0, frames }] if frames.is_empty()),
        "the acceptance ack is an empty ReplEvalDone"
    );
}
