use super::*;

#[test]
fn expanded_files_dirs_lists_root_and_open_subdirs_only() {
    let mut t = TreeView::new();
    t.set_root(
        node("files:", "root", true),
        vec![
            node("files:src", "src", true),
            node("files:docs", "docs", true),
            node("files:a.jl", "a.jl", false),
        ],
    );
    t.rows[1].expanded = true;
    t.apply_children("files:src", vec![node("files:src/x.jl", "x.jl", false)]);
    // Root + the one open subdir; the collapsed `docs` and every file
    // row stay out (a refresh must not reopen a closed dir).
    assert_eq!(expanded_files_dirs(&t.rows), vec!["files:", "files:src"]);
    // Collapsing everything leaves nothing to refresh — the user closed it.
    t.rows[0].expanded = false;
    t.rows[1].expanded = false;
    assert!(expanded_files_dirs(&t.rows).is_empty());
    // A parked non-Files tree (session rows) never triggers a Files refresh.
    let mut s = TreeView::new();
    s.set_root(node("sessions:", "hosts", true), vec![node("session_host:a", "a", true)]);
    assert!(expanded_files_dirs(&s.rows).is_empty());
}
