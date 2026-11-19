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

/// The window's own runs say they are a person's (`color`), so the REPL child prints them with color; the one writer of
/// both requests is here, and an agent's `sot-fe repl run` is a request of another program that does not.
#[tokio::test]
async fn the_windows_eval_and_file_run_carry_the_color_flag() {
    let mut pending = HashMap::new();
    for (send_eval, op) in [(true, "repl.eval"), (false, "repl.run_file")] {
        let mut wire = Vec::new();
        if send_eval {
            send_repl_eval(&mut wire, &mut pending, 1, 7, "1+1".into(), None, None).await.unwrap();
        } else {
            send_repl_run_file(&mut wire, &mut pending, 2, 8, "/a.jl".into(), false, None).await.unwrap();
        }
        let frame = sot_protocol::codec::read_frame_blocking(&mut std::io::BufReader::new(&wire[..])).unwrap();
        assert_eq!(frame.op, op);
        assert_eq!(frame.payload["color"], true, "{op}: {}", frame.payload);
    }
}
