//! The paths of the cursored and the previewed file in the Files tree, and the id of a parent row.

use super::*;
/// The `files:` node id of a file row's parent directory. `files:foo/bar.txt`
/// → `files:foo`; a root-level `files:bar.txt` → `files:` (the root). Non-
/// `files:` ids pass through unchanged. Used to refresh the upload target dir.
pub(in crate::ui) fn parent_files_node_id(node_id: &str) -> String {
    match node_id.strip_prefix("files:") {
        Some(rel) => match rel.rsplit_once('/') {
            Some((parent, _)) => format!("files:{parent}"),
            None => "files:".to_string(),
        },
        None => node_id.to_string(),
    }
}

impl State {
    /// Project root for the active workspace, falling back to the
    /// daemon-startup root (from the hello response) if no
    /// `workspace.list` reply has populated `workspace_project_roots`
    /// yet. The active workspace's root — *not* the daemon startup
    /// root — is the right base for joining `files:<rel>` ids on the
    /// backend, because a workspace swap changes the file tree's
    /// meaning of `<rel>` without changing the daemon startup root.
    pub(in crate::ui) fn active_project_root(&self) -> Option<&str> {
        match self.active_workspace_id.as_deref() {
            Some(slug) => {
                let key: WsKey = (self.active_host.clone(), slug.to_string());
                if let Some(root) = self.workspace_project_roots.get(&key) {
                    return Some(root.as_str());
                }
                // Lookup miss for a known non-default slug (workspace.list
                // not yet processed, or a session outside the registry):
                // return None, NOT the daemon root — a wrong root is worse
                // than none. The old fallback mistranslated paths here:
                // `preview.changed` events from the daemon-root repo
                // resolved as if they were the active workspace's files
                // (phantom tree refreshes, observed live 2026-08-17), while
                // the active workspace's own events missed translation and
                // were dropped — the "nav never updates" bug. The default
                // slug IS the daemon root, so it keeps the fallback.
                if self.default_workspace_slug.as_deref() == Some(slug) {
                    self.daemon_project_root.as_deref()
                } else {
                    None
                }
            }
            None => self.daemon_project_root.as_deref(),
        }
    }

    /// Resolve the cursored NavTree row to a backend-absolute path,
    /// when the row is a `files:<rel>` node and we know the active
    /// workspace's project_root. Used by `o` (open-in-external-tool)
    /// for Pluto-flavored `.jl` dispatch — the backend needs an
    /// absolute path to hand to `SessionActions.open`.
    pub(in crate::ui) fn cursored_files_path(&self) -> Option<String> {
        let row = self.tree.rows.get(self.tree.selected)?;
        let rel = row.node.id.strip_prefix("files:")?;
        if rel.is_empty() {
            return None;
        }
        let root = self.active_project_root()?;
        let trimmed = root.trim_end_matches(['/', '\\']);
        Some(format!("{trimmed}/{rel}"))
    }

    /// Preview-pane analogue of `cursored_files_path`: the path of the file
    /// whose preview is currently SHOWING. This can differ from the nav
    /// cursor — badge-consumed previews, or a cursor that's outrun its own
    /// preview reply — so open-style keys pressed with preview focus act
    /// on what the user is LOOKING AT. Callers fall back to the cursored
    /// row when the shown preview isn't a files-mode node.
    ///
    /// Deliberately reads `preview_src_node_id` (the node the INSTALLED
    /// reply answered) alone — not `preview_node_id_fired` (the node the
    /// most recent REQUEST asked for: field report round 2, a request
    /// racing ahead of its own reply) and not `pinned_preview_node_id`
    /// (round-2 ruling: a pin is stamped from the cursor row, not from an
    /// installed reply, so it can equally outrun what's shown — and
    /// persistently, since `maybe_fire_preview` refuses to fetch anything
    /// while pinned). `o`/`W`/`O` act on what is VISIBLE, period; when a
    /// pinned preview IS what's installed, `preview_src_node_id` already
    /// equals it.
    pub(in crate::ui) fn previewed_files_path(&self) -> Option<String> {
        resolve_previewed_path(
            self.preview_src_node_id.as_deref(),
            self.active_project_root().as_deref(),
        )
    }

    /// Push the cursored NavTree row's file path to the OS clipboard. Only
    /// fires for rows whose node id starts with `files:` (Files mode + the
    /// scan-derived rows in Modules mode that route through preview.get).
    /// Joins with `daemon_project_root` when known to yield an absolute
    /// backend-side path; falls back to the workspace-relative path if
    /// the hello response didn't carry a project_root. Returns true iff
    /// something was written.
    pub(in crate::ui) fn copy_navtree_path(&self) -> bool {
        let row = self.tree.rows.get(self.tree.selected);
        let Some(rel) = row.and_then(|r| r.node.id.strip_prefix("files:")) else {
            return false;
        };
        if rel.is_empty() {
            return false;
        }
        let out = match self.active_project_root() {
            Some(root) => {
                let trimmed = root.trim_end_matches(['/', '\\']);
                format!("{trimmed}/{rel}")
            }
            None => rel.to_string(),
        };
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(out.clone())) {
            Ok(()) => {
                tracing::info!(path = %out, "navtree.copy_path → clipboard");
                true
            }
            Err(e) => {
                tracing::warn!(error = %e, "clipboard write failed; nav path not copied");
                false
            }
        }
    }

    /// Absolute backend-side path for a `files:<rel>` node id (ADR 0022) —
    /// joins the active workspace's project root so the in-pane LLM (on the
    /// backend) can locate the source. Falls back to the bare relative path.
    pub(in crate::ui) fn backend_abs_path(&self, node_id: &str) -> String {
        let rel = node_id.strip_prefix("files:").unwrap_or(node_id);
        match self.active_project_root() {
            Some(root) => format!("{}/{}", root.trim_end_matches(['/', '\\']), rel),
            None => rel.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
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
}
