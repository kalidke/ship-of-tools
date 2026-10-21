use super::*;

#[test]
fn parent_files_node_id_strips_last_segment() {
    assert_eq!(parent_files_node_id("files:foo/bar.txt"), "files:foo");
    assert_eq!(parent_files_node_id("files:a/b/c"), "files:a/b");
    // Root-level file → the root node.
    assert_eq!(parent_files_node_id("files:bar.txt"), "files:");
    // Non-files ids pass through.
    assert_eq!(parent_files_node_id("modules:Foo"), "modules:Foo");
}
