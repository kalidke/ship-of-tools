//! Workspace confinement: whether a path lies under a workspace root (after the Windows
//! verbatim strip) and the canonical form of a path that may not exist yet.

use std::path::{Path, PathBuf};

use crate::paths::simplify_verbatim;
use crate::rows::Workspaces;

/// Canonicalize the longest EXISTING ancestor of `p`, walking up past
/// components that don't exist yet (e.g. a `file.write`/`concept.write`
/// target that hasn't been created). Used by the workspace-confinement
/// symlink-escape guards in `files/tree.rs` and `files/concept.rs`: string-level
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

/// Canonicalizes `path` and returns it if it resolves under `root`'s
/// canonical form — `None` on any canonicalization failure (missing path,
/// dangling symlink, ...) or if it escapes `root`.
fn canonical_under_root(
    path: &std::path::Path,
    root: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let canon_path = path.canonicalize().ok()?;
    let canon_root = root.canonicalize().ok()?;
    canon_path.starts_with(&canon_root).then_some(canon_path)
}

/// Confines `path` to ANY currently-registered workspace, not just the
/// default one (a non-default-workspace open would otherwise be wrongly
/// rejected) — the guard shared by `pluto.open` and `docs.open` (security
/// review). Returns the canonical path; callers MUST use this value for
/// everything downstream rather than re-deriving from the raw input, so the
/// checked path and the acted-upon path can't diverge (TOCTOU).
pub(crate) fn canonicalize_within_any_workspace(
    path: &std::path::Path,
    workspaces: &Workspaces,
) -> Option<std::path::PathBuf> {
    workspaces
        .list()
        .iter()
        .find_map(|ws| canonical_under_root(path, &ws.project_root))
}

/// Same confinement as `canonicalize_within_any_workspace`, but also hands
/// back the matching workspace's own canonical root — `docs.open`'s
/// site-root walk (its only caller) needs that bound to climb toward without
/// running a second, possibly-disagreeing confinement check of its own.
pub(crate) fn canonicalize_and_workspace_root(
    path: &std::path::Path,
    workspaces: &Workspaces,
) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    workspaces.list().iter().find_map(|ws| {
        let canon_root = ws.project_root.canonicalize().ok()?;
        canonical_under_root(path, &ws.project_root).map(|canon_path| (canon_path, canon_root))
    })
}

#[cfg(test)]
#[path = "confine_tests.rs"]
mod tests;
