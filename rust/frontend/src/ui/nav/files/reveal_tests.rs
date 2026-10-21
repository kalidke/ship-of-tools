use super::*;

#[test]
fn ancestor_rels_is_deepest_first() {
    // Deep-path reveal expands the deepest *visible* ancestor each round-
    // trip, so the ordering must be deepest-first to walk down toward the
    // file one level at a time.
    assert_eq!(ancestor_rels("a/b/c.jl"), vec!["a/b", "a"]);
    // Root-level file: no ancestor dirs to expand (already a child of the
    // expanded root) → empty, so drive_reveal_step lands directly.
    assert!(ancestor_rels("README.md").is_empty());
    // Single dir.
    assert_eq!(ancestor_rels("src/edge.jl"), vec!["src"]);
    // Trailing slash (dir target) still yields its parents, deepest-first.
    assert_eq!(ancestor_rels("a/b/"), vec!["a/b", "a"]);
}
