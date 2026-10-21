use super::{PtyCursor, PtyEnter, PtyInputReq, PtyInputRes, PtyScreenReq, PtyScreenRes};

#[test]
fn pty_write_req_wire_shape_is_untouched() {
    // ADR 0042 amendment: `PtyWriteReq`/`PTY_WRITE` are NOT touched by
    // this change — pin the old wire shape byte-for-byte so a future
    // edit to this file cannot silently widen it.
    let req = super::PtyWriteReq { data_b64: "aGVsbG8=".to_string() };
    let json = serde_json::to_value(&req).unwrap();
    assert_eq!(json, serde_json::json!({ "data_b64": "aGVsbG8=" }));
}

#[test]
fn pty_input_req_origin_absent_serializes_to_nothing() {
    let req = PtyInputReq {
        workspace_id: "ws-1".into(),
        data_b64: "aGk=".into(),
        enter: false,
        origin: None,
    };
    let json = serde_json::to_value(&req).unwrap();
    assert_eq!(
        json,
        serde_json::json!({ "workspace_id": "ws-1", "data_b64": "aGk=", "enter": false })
    );
}

#[test]
fn pty_input_req_enter_absent_defaults_false() {
    // A pre-this-change caller (there is none on the wire yet, but a
    // minimal payload) must still parse with `enter: false`.
    let json = serde_json::json!({ "workspace_id": "ws-1", "data_b64": "aGk=" });
    let req: PtyInputReq = serde_json::from_value(json).expect("minimal PtyInputReq parses");
    assert!(!req.enter);
    assert!(req.origin.is_none());
}

#[test]
fn pty_input_req_round_trips_with_origin_and_enter() {
    let req = PtyInputReq {
        workspace_id: "ws-2".into(),
        data_b64: "Zm9v".into(),
        enter: true,
        origin: Some("host-4-dev".into()),
    };
    let json = serde_json::to_string(&req).unwrap();
    let back: PtyInputReq = serde_json::from_str(&json).unwrap();
    assert_eq!(back.workspace_id, "ws-2");
    assert_eq!(back.data_b64, "Zm9v");
    assert!(back.enter);
    assert_eq!(back.origin.as_deref(), Some("host-4-dev"));
}

#[test]
fn pty_input_res_round_trips() {
    let res = PtyInputRes { ok: true, runtime: "capsule".into(), bytes: 5, enter: PtyEnter::NotSent };
    let json = serde_json::to_value(&res).unwrap();
    assert_eq!(
        json,
        serde_json::json!({ "ok": true, "runtime": "capsule", "bytes": 5, "enter": "not_sent" })
    );
}

#[test]
fn pty_input_res_enter_round_trips_and_defaults_unknown() {
    let json = serde_json::json!({ "ok": true, "runtime": "capsule", "bytes": 5 });
    let res: PtyInputRes = serde_json::from_value(json).expect("minimal PtyInputRes parses");
    assert_eq!(res.enter, PtyEnter::Unknown);

    for (v, name) in [(PtyEnter::Sent, "sent"), (PtyEnter::NotSent, "not_sent"), (PtyEnter::Unknown, "unknown")] {
        let res = PtyInputRes { ok: true, runtime: "capsule".into(), bytes: 5, enter: v };
        let json = serde_json::to_value(&res).unwrap();
        assert_eq!(json["enter"], serde_json::json!(name));
    }
}

#[test]
fn pty_screen_req_round_trips() {
    let req = PtyScreenReq { workspace_id: "ws-3".into() };
    let json = serde_json::to_string(&req).unwrap();
    let back: PtyScreenReq = serde_json::from_str(&json).unwrap();
    assert_eq!(back.workspace_id, "ws-3");
}

#[test]
fn pty_screen_res_cursor_absent_serializes_to_nothing_and_round_trips() {
    let res = PtyScreenRes {
        runtime: "tmux".into(),
        cols: 80,
        rows: 24,
        lines: vec!["hello".into(), "".into()],
        cursor: None,
    };
    let json = serde_json::to_value(&res).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "runtime": "tmux",
            "cols": 80,
            "rows": 24,
            "lines": ["hello", ""],
        })
    );
    let back: PtyScreenRes = serde_json::from_value(json).unwrap();
    assert!(back.cursor.is_none());
}

#[test]
fn pty_screen_res_cursor_present_round_trips() {
    let res = PtyScreenRes {
        runtime: "capsule".into(),
        cols: 120,
        rows: 40,
        lines: vec!["x".into()],
        cursor: Some(PtyCursor { row: 3, col: 7 }),
    };
    let json = serde_json::to_string(&res).unwrap();
    let back: PtyScreenRes = serde_json::from_str(&json).unwrap();
    let cursor = back.cursor.expect("cursor present");
    assert_eq!((cursor.row, cursor.col), (3, 7));
}
