use super::*;

/// The request line the REPL child reads: fields in declaration order.
#[test]
fn wire_request_line_is_pinned() {
    let line = serde_json::to_string(&WireRequest {
        v: 1,
        id: 7,
        kind: "req",
        op: "x.y",
        payload: &serde_json::json!({"a": [1, "b"]}),
    })
    .unwrap();
    assert_eq!(line, r#"{"v":1,"id":7,"kind":"req","op":"x.y","payload":{"a":[1,"b"]}}"#);
}
