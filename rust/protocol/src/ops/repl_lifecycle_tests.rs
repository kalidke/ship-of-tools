use super::{ReplFrame, WorkspaceListEntry};

#[test]
fn lifecycle_frame_wire_shape_is_kind_tagged() {
    // The daemon supervisor fabricates these; the FE routes on
    // kind == "lifecycle". Pin the exact wire shape.
    let f = ReplFrame::Lifecycle {
        state: "starting".into(),
    };
    let json = serde_json::to_value(&f).unwrap();
    assert_eq!(
        json,
        serde_json::json!({ "kind": "lifecycle", "state": "starting" })
    );
    let back: ReplFrame = serde_json::from_value(json).unwrap();
    assert!(matches!(back, ReplFrame::Lifecycle { state } if state == "starting"));
}

#[test]
fn unknown_frame_kind_fails_only_that_parse() {
    // The additive contract an OLD front-end relies on: a frame kind it
    // doesn't know fails ITS deserialization (the transport warn-drops
    // that one evt) — it must not silently mis-parse into another
    // variant. This is the same property `lifecycle` rode in on.
    let json = serde_json::json!({ "kind": "from_the_future", "x": 1 });
    assert!(serde_json::from_value::<ReplFrame>(json).is_err());
}

#[test]
fn workspace_list_entry_without_repl_state_defaults_empty() {
    // Mid-rollout tolerance: a daemon that predates `repl_state` (and the
    // other #[serde(default)] fields) still deserializes; the FE reads ""
    // and keeps any frame-driven lifecycle entry instead of regressing.
    let json = serde_json::json!({
        "workspace_id": "ws-a-123",
        "slug": "a",
        "label": "A",
        "project_root": "/p",
        "session_name": "t",
        "kernel_running": false,
        "is_default": false,
    });
    let e: WorkspaceListEntry = serde_json::from_value(json).unwrap();
    assert_eq!(e.repl_state, "");
}

#[test]
fn workspace_list_entry_repl_state_round_trips() {
    let json = serde_json::json!({
        "workspace_id": "ws-a-123",
        "slug": "a",
        "label": "A",
        "project_root": "/p",
        "session_name": "t",
        "kernel_running": false,
        "is_default": false,
        "repl_state": "starting",
    });
    let e: WorkspaceListEntry = serde_json::from_value(json).unwrap();
    assert_eq!(e.repl_state, "starting");
}

/// `color` is how a window says an eval is a person's: absent from every request that is not, so an older window's or an
/// agent's request is what it was, and it reads back as false.
#[test]
fn the_color_flag_is_additive_and_written_only_when_set() {
    use super::{ReplEvalReq, ReplRunFileReq};
    let eval: ReplEvalReq = serde_json::from_value(serde_json::json!({"eval_id": 1, "code": "1"})).unwrap();
    assert!(!eval.color);
    assert!(serde_json::to_value(&eval).unwrap().get("color").is_none());
    let run: ReplRunFileReq = serde_json::from_value(serde_json::json!({"path": "/a.jl"})).unwrap();
    assert!(!run.color);
    assert!(serde_json::to_value(&run).unwrap().get("color").is_none());
    let said: ReplEvalReq = serde_json::from_value(serde_json::json!({"eval_id": 1, "code": "1", "color": true})).unwrap();
    assert_eq!(serde_json::to_value(&said).unwrap()["color"], true);
    let said: ReplRunFileReq = serde_json::from_value(serde_json::json!({"path": "/a.jl", "color": true})).unwrap();
    assert_eq!(serde_json::to_value(&said).unwrap()["color"], true);
}
