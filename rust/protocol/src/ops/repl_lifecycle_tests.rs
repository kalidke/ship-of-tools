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
