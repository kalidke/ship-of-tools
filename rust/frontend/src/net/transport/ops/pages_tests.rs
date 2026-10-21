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
