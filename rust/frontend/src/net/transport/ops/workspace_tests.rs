use super::*;

// --- ADR 0042 slice L1b: the attach_direct switch. ---

#[test]
fn is_attach_direct_declines_every_other_response_shape() {
    let attach = serde_json::json!({
        "error": "this workspace's agent pane is a capsule; attach directly instead of pty.open",
        "code": "attach_direct",
        "state_dir": "/state/workspaces/ws-1",
    });
    assert!(is_attach_direct(&attach));
    // Still recognized when the daemon couldn't resolve a state root —
    // the code alone gates this now, not the (ignored) path.
    let attach_no_dir = serde_json::json!({
        "error": "this workspace's agent pane is a capsule; attach directly instead of pty.open",
        "code": "attach_direct",
        "state_dir": serde_json::Value::Null,
    });
    assert!(is_attach_direct(&attach_no_dir));
    // Any other response shape — this build's daemon never sends
    // one for `pty.open`, but the check must still decline it.
    let ok = serde_json::json!({"cols": 80, "rows": 24});
    assert!(!is_attach_direct(&ok));
    // A DIFFERENT error code must not be mistaken for attach_direct —
    // only the exact literal switches the pane to the attach path.
    let other_error = serde_json::json!({"error": "boom", "code": "bad_target"});
    assert!(!is_attach_direct(&other_error));
}

#[test]
fn pty_open_failure_reason_prefers_code_falls_back_when_absent() {
    let coded = serde_json::json!({"error": "boom", "code": "bad_target"});
    assert_eq!(pty_open_failure_reason(&coded), "bad_target");
    let uncoded = serde_json::json!({"cols": 80, "rows": 24});
    assert_eq!(pty_open_failure_reason(&uncoded), "unsupported daemon reply");
}

/// The listing's one parse site copies each row's last activity beside its work-state stamp: the newer daemon's as it
/// sent it, an older daemon's (which sends none) empty.
#[test]
fn a_listing_carries_each_rows_last_activity() {
    let entry = |slug: &str| {
        serde_json::json!({
            "workspace_id": format!("ws-{slug}-1"),
            "slug": slug,
            "label": slug,
            "project_root": format!("/projects/{slug}"),
            "session_name": format!("sot-be-{slug}"),
            "kernel_running": false,
            "is_default": false,
            "agent_status_at": "2026-09-08T09:00:00Z",
        })
    };
    let mut told = entry("told");
    told["active_at"] = serde_json::json!("2026-09-08T09:05:00Z");
    let payload = serde_json::json!({ "workspaces": [told, entry("older-daemon")] });
    let seen = std::cell::RefCell::new(Vec::new());
    on_workspace_list(Frame::res(1, op::WORKSPACE_LIST, payload), &|evt| {
        if let IncomingEvt::Workspaces { workspaces } = evt {
            for w in workspaces {
                seen.borrow_mut().push((w.slug, w.active_at, w.agent_status_at));
            }
        }
    });
    assert_eq!(
        seen.into_inner(),
        [
            ("told".to_string(), "2026-09-08T09:05:00Z".to_string(), "2026-09-08T09:00:00Z".to_string()),
            ("older-daemon".to_string(), String::new(), "2026-09-08T09:00:00Z".to_string()),
        ]
    );
}
