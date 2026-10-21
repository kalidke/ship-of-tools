use super::*;

#[test]
fn path_within_root_is_component_based() {
    assert!(path_within_root(Path::new("/a/b/c"), Path::new("/a/b")));
    assert!(path_within_root(Path::new("/a/b"), Path::new("/a/b")));
    assert!(!path_within_root(Path::new("/a/bc"), Path::new("/a/b")));
}

#[cfg(windows)]
#[test]
fn path_within_root_accepts_verbatim_child() {
    assert!(path_within_root(
        &PathBuf::from(r"\\?\C:\Users\k\proj\src\lib.rs"),
        &PathBuf::from(r"C:\Users\k\proj")
    ));
}

/// A plain (non-verbatim) Windows path — what a `notify` watcher event
/// actually carries (`to_string_lossy()` off the raw OS path, never
/// canonicalized to `\\?\...`). This wasn't exercised until the
/// preview.changed per-connection fan-out filter (`server/events.rs`'s
/// `preview_changed_visible`) started depending on `path_within_root` for
/// exactly this shape — the OLD filter did a bare `/`-only string
/// `strip_prefix`, which a `\`-separated event path never matched at all.
#[cfg(windows)]
#[test]
fn path_within_root_accepts_plain_windows_child() {
    assert!(path_within_root(
        &PathBuf::from(r"C:\a\b\file.jl"),
        &PathBuf::from(r"C:\a\b")
    ));
    assert!(path_within_root(
        &PathBuf::from(r"C:\a\b\sub\file.jl"),
        &PathBuf::from(r"C:\a\b")
    ));
    // The lookalike-sibling rejection holds here too — component-based
    // comparison, not a string prefix.
    assert!(!path_within_root(
        &PathBuf::from(r"C:\a\bx\file.jl"),
        &PathBuf::from(r"C:\a\b")
    ));
}
