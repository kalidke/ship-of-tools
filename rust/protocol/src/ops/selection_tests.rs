use serde_json::{json, Value};

use super::*;

fn sel() -> Value {
    json!({"node_id": "a/b.md", "start": {"line": 1, "byte": 2}, "end": {"line": 3, "byte": 0}, "len": 9, "digest": 5})
}

fn req(selection: Value) -> Value {
    json!({"workspace_id": "ws-1", "selection": selection})
}

fn refused(v: Value) -> bool {
    serde_json::from_value::<SelectionSetReq>(v).is_err()
}

#[test]
fn selection_set_round_trips_and_the_digest_is_fnv1a_64() {
    assert_eq!(op::SELECTION_SET, "selection.set");
    assert!(op::ALL.contains(&op::SELECTION_SET));
    let r = SelectionSetReq {
        workspace_id: "ws-1".into(),
        selection: Some(SelectedText {
            node_id: "a/b.md".into(),
            start: TextPos { line: 1, byte: 2 },
            end: TextPos { line: 3, byte: 0 },
            len: 9,
            digest: 5,
        }),
    };
    assert_eq!(serde_json::to_value(&r).unwrap(), req(sel()));
    assert_eq!(serde_json::from_value::<SelectionSetReq>(req(sel())).unwrap(), r);
    let clear = SelectionSetReq { workspace_id: "ws-1".into(), selection: None };
    assert_eq!(serde_json::to_value(&clear).unwrap(), req(Value::Null));
    assert_eq!(serde_json::from_value::<SelectionSetReq>(req(Value::Null)).unwrap(), clear);
    for (s, v) in [("shared", SelectionState::Shared), ("held", SelectionState::Held)] {
        let res = SelectionSetRes { state: v };
        assert_eq!(serde_json::to_value(res.clone()).unwrap(), json!({"state": s}));
        assert_eq!(serde_json::from_value::<SelectionSetRes>(json!({"state": s})).unwrap(), res);
    }
    // the published FNV-1a 64 vectors, then multi-byte and LF-joined texts from an independent implementation
    assert_eq!(selection_digest(""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(selection_digest("a"), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(selection_digest("foobar"), 0x8594_4171_f739_67e8);
    assert_ne!(selection_digest("a\nb"), selection_digest("a\r\nb"));
    assert_eq!(crate::selection_digest("é"), 0x0ac2_1707_b718_1e01);
    assert_eq!(selection_digest("a\nb"), 0xe5be_b119_0415_e670);
    assert_eq!(selection_digest("日本\n語"), 0xf969_a301_a4d5_b1d7);
}

#[test]
fn a_missing_selection_is_refused_not_read_as_a_clear() {
    assert!(refused(json!({"workspace_id": "ws-1"})));
}

#[test]
fn ids_are_bounded_at_decode() {
    let mut v = req(sel());
    v["workspace_id"] = json!("w".repeat(MAX_SELECTION_WORKSPACE));
    assert!(!refused(v.clone()));
    v["workspace_id"] = json!("w".repeat(MAX_SELECTION_WORKSPACE + 1));
    assert!(refused(v));
    let mut v = req(sel());
    v["selection"]["node_id"] = json!("n".repeat(MAX_SELECTION_NODE));
    assert!(!refused(v.clone()));
    v["selection"]["node_id"] = json!("n".repeat(MAX_SELECTION_NODE + 1));
    assert!(refused(v));
}

#[test]
fn a_range_starts_before_it_ends() {
    for (s, e, ok) in [((1, 2), (1, 3), true), ((1, 9), (2, 0), true), ((1, 2), (1, 2), false), ((2, 0), (1, 9), false), ((1, 3), (1, 2), false)] {
        let mut v = req(sel());
        v["selection"]["start"] = json!({"line": s.0, "byte": s.1});
        v["selection"]["end"] = json!({"line": e.0, "byte": e.1});
        assert_eq!(!refused(v), ok, "{s:?} to {e:?}");
    }
}

#[test]
fn len_is_one_to_two_mib() {
    for (len, ok) in [(0u64, false), (1, true), (u64::from(MAX_SELECTION_LEN), true), (u64::from(MAX_SELECTION_LEN) + 1, false)] {
        let mut v = req(sel());
        v["selection"]["len"] = json!(len);
        assert_eq!(!refused(v), ok, "len {len}");
    }
}

#[test]
fn a_wrong_type_is_refused_and_an_added_field_is_ignored() {
    for (field, bad) in [("len", json!("9")), ("digest", json!("5")), ("start", json!({"line": "1", "byte": 2})), ("end", json!({"line": 3, "byte": -1}))] {
        let mut v = req(sel());
        v["selection"][field] = bad;
        assert!(refused(v), "{field}");
    }
    let mut v = req(sel());
    v["zz"] = json!(1);
    v["selection"]["zz"] = json!(1);
    assert!(!refused(v));
    // a state that is neither value reads as `other`; a reader never takes it for shared
    let res: SelectionSetRes = serde_json::from_value(json!({"state": "later"})).unwrap();
    assert_eq!(res.state, SelectionState::Other);
}

#[test]
fn the_refusal_codes_are_fixed() {
    use selection_code::*;
    assert_eq!(
        [UNKNOWN_WORKSPACE, BAD_NODE_ID, NOT_FILE_TEXT, NO_AGENT, NOT_A_WINDOW, PUBLISH_FAILED],
        ["unknown_workspace", "bad_node_id", "not_file_text", "no_agent", "not_a_window", "publish_failed"]
    );
}
