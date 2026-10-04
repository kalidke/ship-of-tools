use super::*;

#[test]
fn set_root_seeds_children_at_depth_one() {
    let mut t = TreeView::new();
    t.set_root(
        node("files:", "root", true),
        vec![node("files:a", "a", true), node("files:b", "b", false)],
    );
    assert_eq!(t.rows.len(), 3);
    assert_eq!(t.rows[0].depth, 0);
    assert!(t.rows[0].expanded);
    assert_eq!(t.rows[1].depth, 1);
    assert_eq!(t.rows[2].depth, 1);
    assert_eq!(t.selected, 0);
}

#[test]
fn apply_children_splices_under_parent_and_replaces_existing() {
    let mut t = TreeView::new();
    t.set_root(
        node("r", "r", true),
        vec![node("a", "a", true), node("b", "b", false)],
    );
    // Intentional expands mark the row at request time (the reply-side
    // collapsed-parent gate would otherwise drop the splice).
    t.rows[1].expanded = true;
    t.apply_children(
        "a",
        vec![node("a/1", "a1", false), node("a/2", "a2", false)],
    );
    // rows: r, a, a1, a2, b
    assert_eq!(
        t.rows.iter().map(|r| r.node.id.clone()).collect::<Vec<_>>(),
        vec!["r", "a", "a/1", "a/2", "b"]
    );
    assert!(t.rows[1].expanded);
    assert_eq!(t.rows[2].depth, 2);
    // re-applying with a different set replaces the previous children
    t.apply_children("a", vec![node("a/x", "ax", false)]);
    assert_eq!(
        t.rows.iter().map(|r| r.node.id.clone()).collect::<Vec<_>>(),
        vec!["r", "a", "a/x", "b"]
    );
}

#[test]
fn apply_children_merge_preserves_expanded_subtree_and_nested_cursor() {
    // Regression (2026-08-18, "nav pane keeps resetting"): a root-listing
    // refresh — fired by any root-level file event, editor temp-file
    // churn included — must NOT collapse expanded subtrees or kick a
    // nested cursor to row 0.
    let mut t = TreeView::new();
    t.set_root(
        node("r", "r", true),
        vec![node("a", "a", true), node("b", "b", false)],
    );
    t.rows[1].expanded = true; // request-time expand mark (see contract)
    t.apply_children(
        "a",
        vec![node("a/1", "a1", false), node("a/2", "a2", false)],
    );
    t.selected = 3; // nested, on a/2
                    // Root refresh: same surviving children plus a newcomer (the
                    // temp-file case). `a` must stay expanded with its subtree intact,
                    // the cursor must stay on a/2, and the newcomer arrives collapsed.
    t.apply_children(
        "r",
        vec![
            node("a", "a", true),
            node("b", "b", false),
            node("c.tmp", "ctmp", false),
        ],
    );
    assert_eq!(
        t.rows.iter().map(|r| r.node.id.clone()).collect::<Vec<_>>(),
        vec!["r", "a", "a/1", "a/2", "b", "c.tmp"]
    );
    assert!(t.rows[1].expanded, "surviving child keeps its expansion");
    assert!(!t.rows[5].expanded, "newcomer arrives collapsed");
    assert_eq!(t.selected_node_id().as_deref(), Some("a/2"));
}

#[test]
fn set_root_empty_listing_stays_open_for_live_refresh() {
    // A fresh/empty project: the root row must seed OPEN so a watcher-
    // driven `apply_children` (and a later same-root re-seed) can land
    // the first files — only a user collapse closes the root.
    let mut t = TreeView::new();
    t.set_root(node("files:", "root", true), Vec::new());
    assert_eq!(t.rows.len(), 1);
    assert!(t.rows[0].expanded, "an empty root is an open folder");
    t.apply_children("files:", vec![node("files:new.jl", "new.jl", false)]);
    assert_eq!(
        t.rows.iter().map(|r| r.node.id.clone()).collect::<Vec<_>>(),
        vec!["files:", "files:new.jl"]
    );
    // Same-root re-seed after the listing filled in: children shown.
    let mut u = TreeView::new();
    u.set_root(node("files:", "root", true), Vec::new());
    u.set_root(node("files:", "root", true), vec![node("files:a", "a", false)]);
    assert_eq!(u.rows.len(), 2);
    assert!(u.rows[0].expanded);
}

#[test]
fn set_root_same_root_preserves_expansion_and_collapsed_root() {
    // Codex round 3: a same-root re-seed (Sessions refresh) must keep a
    // surviving child's expansion + descendants (the workspace.list-vs-
    // session-expand race), and a user-collapsed root must stay
    // collapsed with its children dropped.
    let mut t = TreeView::new();
    t.set_root(
        node("r", "r", true),
        vec![node("a", "a", true), node("b", "b", false)],
    );
    t.rows[1].expanded = true; // request-time expand mark (see contract)
    t.apply_children("a", vec![node("a/1", "a1", false)]);
    t.selected = 2; // nested, on a/1
                    // Same-root re-seed with a newcomer: `a` keeps its subtree + cursor.
    t.set_root(
        node("r", "r", true),
        vec![
            node("a", "a", true),
            node("b", "b", false),
            node("c", "c", false),
        ],
    );
    assert_eq!(
        t.rows.iter().map(|r| r.node.id.clone()).collect::<Vec<_>>(),
        vec!["r", "a", "a/1", "b", "c"]
    );
    assert!(t.rows[1].expanded);
    assert_eq!(t.selected_node_id().as_deref(), Some("a/1"));
    // Collapsed root: re-seed keeps it closed, children dropped.
    t.selected = 0;
    assert!(t.collapse_selected());
    t.set_root(node("r", "r", true), vec![node("a", "a", true)]);
    assert_eq!(t.rows.len(), 1);
    assert!(!t.rows[0].expanded);
}

#[test]
fn set_root_different_root_is_a_fresh_seed() {
    let mut t = TreeView::new();
    t.set_root(node("r", "r", true), vec![node("a", "a", true)]);
    t.rows[1].expanded = true; // request-time expand mark (see contract)
    t.apply_children("a", vec![node("a/1", "a1", false)]);
    // New root id (workspace switch): nothing carries over.
    t.set_root(node("q", "q", true), vec![node("a", "a", true)]);
    assert_eq!(
        t.rows.iter().map(|r| r.node.id.clone()).collect::<Vec<_>>(),
        vec!["q", "a"]
    );
    assert!(!t.rows[1].expanded, "expansion never crosses a root change");
}

#[test]
fn set_flat_suppresses_previously_collapsed_subtrees() {
    // Codex round 3: the modules-root Right-then-Left race — a rebuild
    // reply must not reopen a container the user is showing collapsed.
    let rows = |expanded_root: bool| {
        vec![
            TreeRow {
                node: node("m", "m", true),
                depth: 0,
                expanded: expanded_root,
            },
            TreeRow {
                node: node("m/f", "f", false),
                depth: 1,
                expanded: false,
            },
        ]
    };
    let mut t = TreeView::new();
    t.set_flat(rows(true));
    t.selected = 0;
    assert!(t.collapse_selected());
    // The in-flight scan reply arrives with the full expanded tree.
    t.set_flat(rows(true));
    assert_eq!(t.rows.len(), 1, "collapsed root keeps its subtree dropped");
    assert!(!t.rows[0].expanded);
}

#[test]
fn apply_children_ignores_reply_for_collapsed_parent() {
    // Codex finding (collapse race): a background refresh whose reply
    // lands AFTER the user collapsed the parent must not reopen it or
    // splice rows back in.
    let mut t = TreeView::new();
    t.set_root(node("r", "r", true), vec![node("a", "a", true)]);
    t.rows[1].expanded = true; // request-time expand mark (see contract)
    t.apply_children("a", vec![node("a/1", "a1", false)]);
    t.selected = 1; // on `a`
    assert!(t.collapse_selected());
    // The stale reply arrives for the now-collapsed `a`.
    t.apply_children(
        "a",
        vec![node("a/1", "a1", false), node("a/2", "a2", false)],
    );
    assert_eq!(
        t.rows.iter().map(|r| r.node.id.clone()).collect::<Vec<_>>(),
        vec!["r", "a"]
    );
    assert!(!t.rows[1].expanded, "collapse must survive the stale reply");
    // Same rule for a collapsed ROOT.
    t.selected = 0;
    assert!(t.collapse_selected());
    t.apply_children("r", vec![node("a", "a", true)]);
    assert!(!t.rows[0].expanded);
    assert_eq!(t.rows.len(), 1);
}

#[test]
fn apply_children_drops_saved_subtree_when_child_became_a_leaf() {
    // Codex finding (ghost subtree): an expanded DIRECTORY replaced by a
    // same-named node that is no longer a container (kind change or
    // has_children=false) must arrive as a plain collapsed leaf — never
    // resurrect the old descendants under it.
    let mut t = TreeView::new();
    t.set_root(node("r", "r", true), vec![node("a", "a", true)]);
    t.rows[1].expanded = true; // request-time expand mark (see contract)
    t.apply_children("a", vec![node("a/1", "a1", false)]);
    // Root refresh where `a` is now a childless leaf (dir -> file).
    let mut leaf = node("a", "a", false);
    leaf.kind = "file".to_string();
    t.apply_children("r", vec![leaf]);
    assert_eq!(
        t.rows.iter().map(|r| r.node.id.clone()).collect::<Vec<_>>(),
        vec!["r", "a"],
        "old descendants must not survive under the leaf"
    );
    assert!(!t.rows[1].expanded);
}

#[test]
fn apply_children_merge_drops_vanished_childs_subtree() {
    let mut t = TreeView::new();
    t.set_root(
        node("r", "r", true),
        vec![node("a", "a", true), node("b", "b", false)],
    );
    t.rows[1].expanded = true; // request-time expand mark (see contract)
    t.apply_children("a", vec![node("a/1", "a1", false)]);
    t.selected = 2; // on a/1
                    // `a` vanished from the fresh listing: its subtree goes with it,
                    // and the orphaned cursor falls back to the refreshed parent.
    t.apply_children("r", vec![node("b", "b", false)]);
    assert_eq!(
        t.rows.iter().map(|r| r.node.id.clone()).collect::<Vec<_>>(),
        vec!["r", "b"]
    );
    assert_eq!(t.selected, 0);
}

#[test]
fn collapse_selected_drops_descendants_and_unflags() {
    let mut t = TreeView::new();
    t.set_root(node("r", "r", true), vec![node("a", "a", true)]);
    t.rows[1].expanded = true; // request-time expand mark (see contract)
    t.apply_children("a", vec![node("a/1", "a1", false)]);
    t.selected = 1; // on `a`
    assert!(t.collapse_selected());
    assert_eq!(
        t.rows.iter().map(|r| r.node.id.clone()).collect::<Vec<_>>(),
        vec!["r", "a"]
    );
    assert!(!t.rows[1].expanded);
    // collapsing a leaf is a no-op
    t.selected = 1;
    assert!(!t.collapse_selected());
}

#[test]
fn parent_of_selected_walks_back_to_lesser_depth() {
    let mut t = TreeView::new();
    t.set_root(node("r", "r", true), vec![node("a", "a", true)]);
    t.rows[1].expanded = true; // request-time expand mark (see contract)
    t.apply_children("a", vec![node("a/1", "a1", false)]);
    t.selected = 2; // on a/1, depth=2
    assert_eq!(t.parent_of_selected(), Some(1));
    t.selected = 0;
    assert_eq!(t.parent_of_selected(), None);
}

#[test]
fn move_up_and_down_clamp_at_edges() {
    let mut t = TreeView::new();
    t.set_root(
        node("r", "r", true),
        vec![node("a", "a", false), node("b", "b", false)],
    );
    t.move_up(); // already at 0
    assert_eq!(t.selected, 0);
    t.move_down();
    t.move_down();
    t.move_down(); // last row is 2; should clamp
    assert_eq!(t.selected, 2);
}

// ---- cursor-by-node-id preservation across row-mutating ops ----
//
// Regression coverage for the "nav cursor resets to row 0" bug. set_root,
// set_flat, and apply_children all used to unconditionally reset
// `selected` to 0, which kicked the cursor back to the top of the nav
// every transport reconnect, every project.scan, and every stale
// tree.children splice. The fix re-anchors on the previously-cursored
// node id; these tests pin that behaviour.

#[test]
fn set_root_preserves_cursor_when_node_still_present() {
    let mut t = TreeView::new();
    t.set_root(
        node("r", "r", true),
        vec![node("a", "a", false), node("b", "b", false)],
    );
    t.selected = 2; // on `b`
    t.set_root(
        node("r", "r", true),
        vec![
            node("a", "a", false),
            node("b", "b", false),
            node("c", "c", false),
        ],
    );
    assert_eq!(t.rows[t.selected].node.id, "b");
}

#[test]
fn set_root_falls_back_to_zero_when_cursored_node_gone() {
    let mut t = TreeView::new();
    t.set_root(
        node("r", "r", true),
        vec![node("a", "a", false), node("b", "b", false)],
    );
    t.selected = 2; // on `b`
    t.set_root(
        node("r2", "r2", true),
        vec![node("x", "x", false), node("y", "y", false)],
    );
    assert_eq!(t.selected, 0);
    assert_eq!(t.rows[0].node.id, "r2");
}

#[test]
fn set_flat_preserves_cursor_when_node_still_present() {
    let mut t = TreeView::new();
    let rows = |ids: &[&str]| -> Vec<TreeRow> {
        ids.iter()
            .map(|id| TreeRow {
                node: node(id, id, false),
                depth: 0,
                expanded: false,
            })
            .collect()
    };
    t.set_flat(rows(&["m1", "m2", "m3"]));
    t.selected = 2; // on `m3`
    t.set_flat(rows(&["m1", "m2", "m3", "m4"]));
    assert_eq!(t.rows[t.selected].node.id, "m3");
}

#[test]
fn set_flat_falls_back_to_zero_when_cursored_node_gone() {
    let mut t = TreeView::new();
    let rows = |ids: &[&str]| -> Vec<TreeRow> {
        ids.iter()
            .map(|id| TreeRow {
                node: node(id, id, false),
                depth: 0,
                expanded: false,
            })
            .collect()
    };
    t.set_flat(rows(&["m1", "m2", "m3"]));
    t.selected = 2;
    t.set_flat(rows(&["n1", "n2"]));
    assert_eq!(t.selected, 0);
}

#[test]
fn apply_children_preserves_cursor_below_splice() {
    let mut t = TreeView::new();
    t.set_root(
        node("r", "r", true),
        vec![node("a", "a", true), node("b", "b", false)],
    );
    t.selected = 2; // on `b`, after the splice point under `a`
    t.rows[1].expanded = true; // request-time expand mark (see contract)
    t.apply_children(
        "a",
        vec![node("a/1", "a1", false), node("a/2", "a2", false)],
    );
    // rows: r, a, a/1, a/2, b — cursor follows `b` to its new index
    assert_eq!(t.rows[t.selected].node.id, "b");
    assert_eq!(t.selected, 4);
}

#[test]
fn apply_children_falls_back_to_parent_when_cursored_child_disappears() {
    let mut t = TreeView::new();
    t.set_root(node("r", "r", true), vec![node("a", "a", true)]);
    t.rows[1].expanded = true; // request-time expand mark (see contract)
    t.apply_children(
        "a",
        vec![node("a/1", "a1", false), node("a/2", "a2", false)],
    );
    t.selected = 2; // on a/1
                    // Re-apply with a different set; a/1 is gone.
    t.apply_children("a", vec![node("a/x", "ax", false)]);
    // rows: r, a, a/x — cursor falls back to the parent (`a`) so the
    // user stays anchored to the surrounding context.
    assert_eq!(t.rows[t.selected].node.id, "a");
    assert_eq!(t.selected, 1);
}
