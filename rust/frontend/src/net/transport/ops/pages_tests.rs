use super::*;

fn answer(payload: serde_json::Value) -> String {
    let seen = std::cell::RefCell::new(Vec::new());
    on_pluto_open(Frame::res(1, op::PLUTO_OPEN, payload), &|evt| seen.borrow_mut().push(format!("{evt:?}")));
    seen.into_inner().join("\n")
}

/// A `pluto_starting` reply is the start still going, its own event; any other error is a failed open.
#[test]
fn a_pluto_still_starting_is_not_a_failed_open() {
    let starting = serde_json::json!({ "error": "Pluto is still starting after 120 s", "code": "pluto_starting" });
    assert_eq!(answer(starting), r#"PlutoStarting { message: "Pluto is still starting after 120 s" }"#);
    let failed = serde_json::json!({ "error": "no READY", "code": "pluto_open_failed" });
    assert_eq!(answer(failed), r#"PlutoOpened { result: Err("no READY") }"#);
}

fn page_reply_answer(payload: serde_json::Value) -> String {
    let seen = std::cell::RefCell::new(Vec::new());
    on_page_reply(Frame::res(1, op::PAGE_REPLY, payload), &|evt| seen.borrow_mut().push(format!("{evt:?}")), "repo__box".into());
    seen.into_inner().join("\n")
}

/// The answer is about the handle the confirmed reply was sent for, whatever the window shows by then: the
/// daemon's outcome and reason, or an error reply's words as outcome `error`.
#[test]
fn a_page_reply_answer_carries_its_handle_outcome_and_reason() {
    let sent = serde_json::json!({ "outcome": "sent", "detail": "the marked line and Enter were recorded" });
    assert_eq!(
        page_reply_answer(sent),
        r#"PageReplied { handle: "repo__box", outcome: "sent", detail: "the marked line and Enter were recorded" }"#
    );
    let refused = serde_json::json!({ "outcome": "refused", "detail": "handle_changed" });
    assert_eq!(
        page_reply_answer(refused),
        r#"PageReplied { handle: "repo__box", outcome: "refused", detail: "handle_changed" }"#
    );
    let error = serde_json::json!({ "error": "unknown workspace", "code": "unknown_workspace" });
    assert_eq!(
        page_reply_answer(error),
        r#"PageReplied { handle: "repo__box", outcome: "error", detail: "unknown workspace [unknown_workspace]" }"#
    );
    let odd = serde_json::json!({ "something": "else" });
    assert!(page_reply_answer(odd).contains(r#"outcome: "error""#), "an unreadable answer is never a sent");
}
