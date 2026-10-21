use super::*;

#[test]
fn build_new_file_node_id_joins_and_validates() {
    // Root dir (`files:`) → no separator before the bare name.
    assert_eq!(
        build_new_file_node_id("files:", "a.txt"),
        Ok("files:a.txt".to_string())
    );
    // Sub-directory → joined with a single `/`.
    assert_eq!(
        build_new_file_node_id("files:sub", "a.txt"),
        Ok("files:sub/a.txt".to_string())
    );
    assert_eq!(
        build_new_file_node_id("files:a/b", "c.jl"),
        Ok("files:a/b/c.jl".to_string())
    );
    // Surrounding whitespace is trimmed off the name.
    assert_eq!(
        build_new_file_node_id("files:sub", "  spaced.txt  "),
        Ok("files:sub/spaced.txt".to_string())
    );
    // Empty / whitespace-only names are rejected.
    assert!(build_new_file_node_id("files:", "").is_err());
    assert!(build_new_file_node_id("files:", "   ").is_err());
    // Path separators are rejected (the backend only accepts a bare
    // child segment — no nested-dir creation, no `..` traversal).
    assert!(build_new_file_node_id("files:", "a/b.txt").is_err());
    assert!(build_new_file_node_id("files:", "a\\b.txt").is_err());
    assert!(build_new_file_node_id("files:sub", "../escape.txt").is_err());
}

#[test]
fn nav_prompt_name_char_allowed_takes_one_trailing_slash_only() {
    // `\` is never a name character.
    assert!(!nav_prompt_name_char_allowed("", '\\'));
    assert!(!nav_prompt_name_char_allowed("sub", '\\'));
    // A leading `/` on an empty buffer is refused (a directory still
    // needs a name).
    assert!(!nav_prompt_name_char_allowed("", '/'));
    // Ordinary chars are always fine on an empty or plain buffer.
    assert!(nav_prompt_name_char_allowed("", 's'));
    assert!(nav_prompt_name_char_allowed("sub", 'x'));
    // A single trailing `/` is accepted once the buffer is non-empty.
    assert!(nav_prompt_name_char_allowed("sub", '/'));
    // Once the buffer ends with `/`, nothing more is accepted — neither
    // another `/` (no double slash) nor an ordinary char (no embedded
    // separator via "sub/" + "x" → "sub/x").
    assert!(!nav_prompt_name_char_allowed("sub/", '/'));
    assert!(!nav_prompt_name_char_allowed("sub/", 'x'));
}

#[test]
fn split_create_name_takes_a_trailing_slash_as_a_directory_marker() {
    assert_eq!(split_create_name("a.txt"), (false, "a.txt".to_string()));
    assert_eq!(split_create_name("sub/"), (true, "sub".to_string()));
    // Chained through the id builder, this is what actually reaches the
    // wire in `OutgoingReq::DirCreate`: "sub/" typed under `files:x`
    // becomes a create for `files:x/sub`.
    let (is_dir, bare) = split_create_name("sub/");
    assert!(is_dir);
    assert_eq!(
        build_new_file_node_id("files:x", &bare),
        Ok("files:x/sub".to_string())
    );
}

#[test]
fn new_file_collision_detected_against_existing_sibling() {
    // Mirror the confirm-path collision guard: scan the flat rows for the
    // would-be new id. A sibling `a.txt` already under `files:sub` means
    // creating another `a.txt` there collides; `b.txt` does not.
    let mut t = TreeView::new();
    t.set_root(
        node("files:sub", "sub", true),
        vec![node("files:sub/a.txt", "a.txt", false)],
    );
    let collide = build_new_file_node_id("files:sub", "a.txt").unwrap();
    let fresh = build_new_file_node_id("files:sub", "b.txt").unwrap();
    assert!(t.rows.iter().any(|r| r.node.id == collide));
    assert!(!t.rows.iter().any(|r| r.node.id == fresh));
}

#[test]
fn delete_refuses_directory_rows() {
    // Mirror the Ctrl+N tests: `begin_delete_file` pre-refuses dirs in v1,
    // so its dir test (`is_directory_row`) must reject a `dir`-kind node
    // and the files root, while accepting an ordinary file row. A directory
    // row (kind "dir").
    let dir = TreeNode {
        id: "files:sub".to_string(),
        label: "sub".to_string(),
        kind: "dir".to_string(),
        has_children: true,
        badges: Vec::new(),
        payload: Default::default(),
    };
    assert!(is_directory_row(&dir), "kind == dir is a directory");
    // The files root is itself a directory even without the "dir" kind.
    let root = TreeNode {
        id: "files:".to_string(),
        label: "/".to_string(),
        kind: "files".to_string(),
        has_children: true,
        badges: Vec::new(),
        payload: Default::default(),
    };
    assert!(is_directory_row(&root), "files: root is a directory");
    // An ordinary file row is deletable (not a directory). `node` builds a
    // file row (kind "files", no children).
    let file = node("files:sub/a.txt", "a.txt", false);
    assert!(!is_directory_row(&file), "a file row is deletable");
}
