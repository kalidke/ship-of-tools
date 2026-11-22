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

/// A row's input time moving is told once, on a tick whose registry read and on one whose registry did not (a computer
/// used only for sessions with no agent has none); nothing is told when nothing moved, and a tick with no registry keeps
/// what the rows last showed for the next comparison.
#[test]
fn a_rows_input_time_alone_publishes_a_change_even_with_no_registry() {
    let rows = rows();
    let shown = |state: &str| Some(project(&rows, Some(&agents("hostA", state, "1")), "hostA"));
    let mut seen = Seen::default();
    assert!(!changed(&mut seen, shown("idle"), typed_of(&rows)), "the first tick has nothing to compare with");
    assert!(!changed(&mut seen, shown("idle"), typed_of(&rows)), "nothing moved");
    rows[0].note_input(100);
    assert!(changed(&mut seen, shown("idle"), typed_of(&rows)), "the row's input time moved");
    assert!(!changed(&mut seen, shown("idle"), typed_of(&rows)), "told once");
    rows[0].note_input(200);
    assert!(changed(&mut seen, None, typed_of(&rows)), "the input time moved on a tick whose registry could not be read");
    assert!(!changed(&mut seen, None, typed_of(&rows)), "an unreadable registry alone is no change");
    assert!(changed(&mut seen, shown("working"), typed_of(&rows)), "a state change is compared with the last tick that read");
    // A daemon that never had a registry.
    let mut seen = Seen::default();
    assert!(!changed(&mut seen, None, typed_of(&rows)));
    rows[0].note_input(300);
    assert!(changed(&mut seen, None, typed_of(&rows)), "typing is told with no registry at all");
    assert!(!changed(&mut seen, None, typed_of(&rows)));
}
