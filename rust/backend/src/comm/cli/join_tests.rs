use super::*;
use crate::comm::test_home::TestHome;

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

fn tier1(t: &TestHome) -> String {
    tier(1, &t.root(), "box1", Default::default()).1
}

/// A plain join derives `<folder>-<host>`, writes the entry, the self file and the inbox, and says so.
#[test]
fn a_plain_join_takes_the_derived_handle_and_writes_entry_self_file_and_inbox() {
    let t = TestHome::new();
    let o = join(&t.env, &[]);
    let h = tier1(&t);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stdout.starts_with(&format!("Joined sot-comm as @{h}  ({} on box1).\n", t.repo())), "{}", o.stdout);
    let e = &t.agents()[&h];
    assert_eq!((e["host"].as_str(), e["root"].as_str(), e["workspace_id"].as_str()), (Some("box1"), Some(t.root().as_str()), Some("")));
    assert_eq!(std::fs::read_to_string(t.nopane()).unwrap(), format!("{h}\nrepo={}\nroot={}\n", t.repo(), t.root()));
    assert!(t.home.join("inbox").join(format!("{h}.jsonl")).is_file());
    // A second join keeps it: the self file names it for this root.
    assert!(join(&t.env, &[]).stdout.starts_with(&format!("Joined sot-comm as @{h} ")));
}

/// Handles are derived, never chosen (decision 0031): `--name`, a bare name and `SOT_COMM_NAME` choose nothing.
#[test]
fn no_argument_or_variable_chooses_a_handle() {
    let mut t = TestHome::new();
    for a in [&["--name", "named-box1"][..], &["--name=named-box1"], &["named-box1"]] {
        let o = join(&t.env, &args(a));
        assert_eq!(o.code, 2, "{a:?}");
        assert!(o.stderr.contains("handles are derived, never chosen"), "{}", o.stderr);
    }
    assert_eq!(join(&t.env, &args(&["--bogus"])).code, 2);
    assert!(t.agents().as_object().unwrap().is_empty(), "a refusal writes nothing");
    t.set("SOT_COMM_NAME", "pinned-box1");
    let o = join(&t.env, &[]);
    assert!(o.stdout.starts_with(&format!("Joined sot-comm as @{} ", tier1(&t))), "{}", o.stdout);
    assert!(t.agents().get("pinned-box1").is_none());
}

/// A row's slot never comes to name another project's session.
#[test]
fn a_slot_claimed_for_another_project_is_refused_unless_repinned() {
    let mut t = TestHome::new();
    // A private slot that names no row, so the gate passes outside any capsule.
    let slot = t.home.join("self").join("private-a.txt");
    t.set("SOT_COMM_SELF_FILE", &slot);
    crate::comm::identity::self_file::write(&slot, "other-box1", "other", "/other/project").unwrap();
    let o = join(&t.env, &[]);
    assert_eq!(o.code, 3, "{}", o.stderr);
    assert!(o.stderr.contains("REFUSING to put"), "{}", o.stderr);
    assert!(t.agents().as_object().unwrap().is_empty(), "a refusal costs no registry entry");
    assert_eq!(join(&t.env, &args(&["--repin"])).code, 0);
}

/// A session outside any row never takes a handle a row on another computer holds.
#[test]
fn outside_any_row_a_handle_bound_to_a_row_elsewhere_is_refused() {
    let t = TestHome::new();
    t.put("taken-box1", serde_json::json!({"host": "box2", "workspace_id": "ws-z"}));
    crate::comm::identity::self_file::write(&t.nopane(), "taken-box1", &t.repo(), &t.root()).unwrap();
    let o = join(&t.env, &[]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stderr.contains("@taken-box1 belongs to row ws-z; this process carries no row id"), "{}", o.stderr);
    assert!(o.stdout.starts_with(&format!("Joined sot-comm as @{} ", tier1(&t))), "{}", o.stdout);
    assert_eq!(t.agents()["taken-box1"]["workspace_id"].as_str(), Some("ws-z"), "untouched");
    // A derived join steps past it to the session's own.
    t.put(&tier1(&t), serde_json::json!({"host": "box2", "workspace_id": "ws-z"}));
    let o = join(&t.env, &[]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stderr.contains("is held by a row; a session outside any row joins as"), "{}", o.stderr);
}
