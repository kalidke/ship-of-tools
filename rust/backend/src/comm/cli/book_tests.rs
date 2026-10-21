use super::*;
use crate::comm::test_home::TestHome;
use serde_json::json;

#[test]
fn leave_removes_this_sessions_entry_and_self_file_or_only_an_orphans_entry() {
    let t = TestHome::new();
    t.self_file(&t.nopane(), "me-box1");
    t.live("me-box1", &t.root());
    t.live("orphan-box1", "/o");
    let o = leave(&["--name".into(), "orphan-box1".into()]);
    assert_eq!(o.stdout, "Removed @orphan-box1 from the registry (row only; comm-despawn.sh does full teardown).\n", "{}", o.stderr);
    assert_eq!(leave(&["--name".into(), "orphan-box1".into()]).stdout, "@orphan-box1 not in registry \u{2014} nothing to do.\n");
    assert_eq!(leave(&["--bogus".into()]).code, 1, "an unknown argument never removes the caller's own row");
    assert!(t.agents().get("me-box1").is_some());
    assert_eq!(leave(&[]).stdout, "Left sot-comm (@me-box1 removed).\n");
    assert!(t.agents().get("me-box1").is_none() && !t.nopane().exists());
    assert_eq!(leave(&[]).stdout, "Not joined \u{2014} nothing to do.\n");
}

/// A handle a row holds is removed only where the daemon says no row does.
#[test]
fn leave_refuses_a_handle_another_row_holds() {
    let t = TestHome::new();
    t.put("rows-box1", json!({"host": "box1", "workspace_id": "ws-q"}));
    let o = leave(&["--name".into(), "rows-box1".into()]);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("REFUSING to remove @rows-box1") && o.stderr.contains("cannot remove it"), "{}", o.stderr);
    assert!(t.agents().get("rows-box1").is_some());
}

#[test]
fn list_names_each_handle_its_liveness_and_its_work_state() {
    let t = TestHome::new();
    t.self_file(&t.nopane(), "me-box1");
    t.live("me-box1", &t.root());
    t.put("old-box1", json!({"host": "box1", "repo": "o", "last_seen": "2020-01-01T00:00:00Z", "state": "blocked", "summary": "q?", "expertise": ["a", "b"]}));
    let o = list(&[]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stdout.contains("  @me-box1") && o.stdout.contains("live") && o.stdout.contains("<- me"), "{}", o.stdout);
    assert!(o.stdout.contains("[a/b]") && o.stdout.contains("    [blocked] q?") && o.stdout.contains("stale "), "{}", o.stdout);
    assert!(o.stdout.contains("attached frontend boxes"), "{}", o.stdout);
    assert_eq!(epoch_of("1970-01-02T00:00:01Z"), Some(86_401));
    assert_eq!(fmt_age(7200), "2h ago");
}
