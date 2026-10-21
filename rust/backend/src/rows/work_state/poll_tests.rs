//! Tests of the poll's projection: it changes exactly when what a row shows changes.

use super::*;
use serde_json::json;
use std::path::PathBuf;

fn rows() -> Vec<Arc<Workspace>> {
    let ws = Workspace::from_label("x", PathBuf::from("/p/x"), false, "claude".into(), "x-h".into(), String::new());
    vec![Arc::new(ws)]
}

fn agents(host: &str, state: &str, last_seen: &str) -> Value {
    json!({"x-h": {"host": host, "state": state, "summary": "s", "status_at": "t", "last_seen": last_seen},
           "other": {"host": host, "state": state}})
}

/// A row's state changing, or its entry passing to another host, changes the projection.
#[test]
fn a_state_change_or_a_host_change_changes_it() {
    let rows = rows();
    let before = project(&rows, Some(&agents("hostA", "idle", "1")), "hostA");
    assert_eq!(before[0].1.state, "idle");
    assert_ne!(before, project(&rows, Some(&agents("hostA", "working", "1")), "hostA"));
    assert_ne!(before, project(&rows, Some(&agents("hostB", "idle", "1")), "hostA"), "another host's entry shows nothing here");
}

/// A field the window does not show (`last_seen`, the liveness stamp) and an entry no row binds change nothing.
#[test]
fn an_unshown_field_or_an_unbound_entry_changes_nothing() {
    let rows = rows();
    let before = project(&rows, Some(&agents("hostA", "idle", "1")), "hostA");
    assert_eq!(before, project(&rows, Some(&agents("hostA", "idle", "2")), "hostA"));
    let mut other = agents("hostA", "idle", "1");
    other["other"]["state"] = json!("blocked");
    assert_eq!(before, project(&rows, Some(&other), "hostA"));
}
