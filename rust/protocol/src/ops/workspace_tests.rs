use super::*;

/// A list entry as a daemon before `active_at` sends it: every field that has no default, and nothing else.
fn previous_entry() -> serde_json::Value {
    serde_json::json!({
        "workspace_id": "ws-proj-1",
        "slug": "proj",
        "label": "proj",
        "project_root": "/projects/proj",
        "session_name": "sot-be-proj",
        "kernel_running": false,
        "is_default": false,
        "agent_status_at": "2026-09-08T09:00:00Z",
    })
}

#[test]
fn a_list_entry_grows_by_its_last_activity_and_stays_additive() {
    // An older daemon's entry decodes, its last activity empty: the window's signal to sort by the status time.
    let old: WorkspaceListEntry = serde_json::from_value(previous_entry()).unwrap();
    let old = serde_json::to_value(old).unwrap();
    assert_eq!(old["active_at"], serde_json::json!(""));
    assert_eq!(old["agent_status_at"], serde_json::json!("2026-09-08T09:00:00Z"));
    // A newer daemon's entry, with the field and one this reader does not know, decodes and keeps both times apart.
    let mut newer = previous_entry();
    newer["active_at"] = serde_json::json!("2026-09-08T09:05:00Z");
    newer["a_field_of_a_later_daemon"] = serde_json::json!(7);
    let entry: WorkspaceListEntry = serde_json::from_value(newer.clone()).unwrap();
    let entry = serde_json::to_value(entry).unwrap();
    assert_eq!(entry["active_at"], serde_json::json!("2026-09-08T09:05:00Z"));
    assert_eq!(entry["agent_status_at"], serde_json::json!("2026-09-08T09:00:00Z"));
    // An older reader, which has no such field, reads the newer entry as before.
    #[derive(serde::Deserialize)]
    struct OlderEntry {
        slug: String,
        agent_status_at: String,
    }
    let older: OlderEntry = serde_json::from_value(newer).unwrap();
    assert_eq!((older.slug.as_str(), older.agent_status_at.as_str()), ("proj", "2026-09-08T09:00:00Z"));
}
