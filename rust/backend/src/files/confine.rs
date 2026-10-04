//! Workspace confinement: whether a path lies under a workspace root (after the Windows
//! verbatim strip) and the canonical form of a path that may not exist yet.

use std::path::{Path, PathBuf};

use crate::paths::simplify_verbatim;

/// Canonicalize the longest EXISTING ancestor of `p`, walking up past
/// components that don't exist yet (e.g. a `file.write`/`concept.write`
/// target that hasn't been created). Used by the workspace-confinement
/// symlink-escape guards in `files_mode.rs` and `concept.rs`: string-level
/// `..`/absolute-path checks on a node id can't catch a symlink INSIDE the
/// root pointing outside it (a real risk on NFS-shared homes, where a
/// symlink can legitimately cross machines/mounts). A target that doesn't
/// exist yet can't itself BE a symlink escaping the root, so checking its
/// nearest existing ancestor is exactly as strong a guarantee as checking
/// the full path once it's created — any escape has to go through a
/// component that already exists. Returns `None` only if not even the
/// filesystem root canonicalizes, which shouldn't happen.
pub fn canonicalize_existing_ancestor(p: &Path) -> Option<PathBuf> {
    let mut cur = p;
    loop {
        if let Ok(c) = cur.canonicalize() {
            return Some(simplify_verbatim(c));
        }
        cur = cur.parent()?;
    }
}

/// True when `candidate` is exactly `root` or a descendant of it, after
/// applying the same Windows verbatim-prefix normalization used for canonical
/// project roots. The comparison is component-wise, so `/a/bc` is not treated
/// as being under `/a/b`.
pub fn path_within_root(candidate: &Path, root: &Path) -> bool {
    let candidate = simplify_verbatim(candidate.to_path_buf());
    let root = simplify_verbatim(root.to_path_buf());
    let mut candidate_components = candidate.components();
    for root_component in root.components() {
        match candidate_components.next() {
            Some(candidate_component) if candidate_component == root_component => {}
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
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
    /// preview.changed per-connection fan-out filter (server.rs
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
}
