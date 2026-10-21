use super::*;
use crate::comm::test_home::TestHome;
use serde_json::json;

fn resolve_here(t: &TestHome, rebind: bool) -> (Context, Vec<String>) {
    let mut notes = Vec::new();
    let c = resolve(rebind, &t.project, || true, &mut notes).expect("resolve");
    (c, notes)
}

/// A process outside any row: the shared slot, its project's root, no row.
#[test]
fn outside_a_row_the_identity_is_the_box_s_shared_slot_and_this_project() {
    let t = TestHome::new();
    let (c, _) = resolve_here(&t, false);
    assert_eq!((c.host.as_str(), c.handle_host.as_str(), c.row_id.as_str()), ("box1", "box1", ""));
    assert_eq!((c.repo.clone(), c.project_root.clone()), (t.repo(), t.root()));
    assert_eq!(c.self_file, t.home.join("self").join("box1__nopane.txt"));
    assert_eq!(c.name, "", "not joined");
    t.self_file(&c.self_file, "repo-box1");
    assert_eq!(resolve_here(&t, false).0.name, "repo-box1");
}

/// In a row the daemon pins the self file; its name names the row, whatever the environment says.
#[test]
fn a_pinned_self_file_names_the_row_and_a_handle_another_row_binds_is_discarded() {
    let t = TestHome::new();
    let pinned = t.home.join("self").join("box1__ws-a.txt");
    std::env::set_var("SOT_COMM_SELF_FILE", &pinned);
    std::env::set_var("SOT_WORKSPACE_ID", "ws-zzz");
    t.self_file(&pinned, "repo-box1");
    let (c, notes) = resolve_here(&t, false);
    assert_eq!((c.row_id.as_str(), c.name.as_str()), ("ws-a", "repo-box1"));
    assert!(notes.is_empty(), "{notes:?}");
    t.put("repo-box1", json!({"host": "box1", "workspace_id": "ws-b"}));
    let (c, notes) = resolve_here(&t, false);
    assert_eq!((c.name.as_str(), c.name_held_by.as_str()), ("", "ws-b"));
    assert!(notes[0].contains("@repo-box1 belongs to row ws-b; this process carries ws-a"), "{notes:?}");
    // A join from the row passes --rebind: the handle stays, so the join moves it back.
    assert_eq!(resolve_here(&t, true).0.name, "repo-box1");
    // A pinned file of any other name names no row, even with SOT_WORKSPACE_ID set.
    std::env::set_var("SOT_COMM_SELF_FILE", t.home.join("self").join("private.txt"));
    assert_eq!(resolve_here(&t, false).0.row_id, "");
}

/// A process with no row id never reads a handle a row binds.
#[test]
fn outside_any_row_a_handle_a_row_binds_names_nobody() {
    let t = TestHome::new();
    let slot = t.home.join("self").join("box1__nopane.txt");
    t.self_file(&slot, "repo-box1");
    t.put("repo-box1", json!({"host": "box1", "workspace_id": "ws-b"}));
    let (c, notes) = resolve_here(&t, true);
    assert_eq!(c.name, "", "--rebind needs a row id");
    assert!(notes[0].contains("carries no row id"), "{notes:?}");
}

#[test]
fn the_eval_lines_are_what_comm_context_printed_and_bash_reads_them_back() {
    let c = Context {
        host: "box1".into(),
        repo: "my repo".into(),
        project_root: "/a b/it's \"here\"".into(),
        name: String::new(),
        self_file: PathBuf::from("/c/self/box1__nopane.txt"),
        comm_home: PathBuf::from("/c"),
        ..Context::default()
    };
    let text = c.eval_lines();
    assert!(text.starts_with("HOST=box1\nHANDLE_HOST=\n"), "{text}");
    let registry = if cfg!(windows) { "C:/registry.json" } else { "/c/registry.json" };
    assert!(text.contains("\nNAME=\n") && text.contains(&format!("\nREGISTRY={registry}\n")), "{text}");
    if cfg!(unix) {
        let mut bash = std::process::Command::new("bash");
        bash.arg("-c").arg(r#"eval "$1"; printf '%s|%s|%s' "$REPO" "$PROJECT_ROOT" "$INBOX_DIR""#).arg("bash").arg(&text);
        let ran = sot_log::test_wait::run_within(&mut bash, std::time::Duration::from_secs(30)).ok();
        assert_eq!(ran.stdout, "my repo|/a b/it's \"here\"|/c/inbox");
    }
    assert_eq!(shell_quote("a\nb"), "$'a\\nb'");
    assert_eq!(shell_quote("plain-1.2_x@h:%+"), "plain-1.2_x@h:%+");
}
