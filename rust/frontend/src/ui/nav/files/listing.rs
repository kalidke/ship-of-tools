//! Refreshes of the Files tree's directory listings: the hidden-files toggle and the watcher and restore re-lists.

use super::*;
/// Every expanded `files:` directory row (root included) — the listings a
/// restored parked Files tree must re-fetch on entry. A parked tree learns
/// nothing while parked: watcher refreshes touch only the ACTIVE tree
/// (`refresh_tree_dir_if_expanded`), and the daemon fans `preview.changed`
/// out only to connections whose active view is that workspace, so a file
/// created in a workspace the user isn't looking at is absent from its
/// parked tree on return. Collapsed dirs are left alone: a background
/// refresh must not reopen what the user closed (`apply_children` drops a
/// collapsed parent's reply anyway).
fn expanded_files_dirs(rows: &[TreeRow]) -> Vec<String> {
    rows.iter()
        .filter(|r| r.expanded && r.node.id.starts_with("files:"))
        .map(|r| r.node.id.clone())
        .collect()
}

impl State {
    /// Toggle the backend's Files-mode "show hidden files" flag for the active
    /// workspace (the `.` keybind → `nav.toggle_hidden`), then re-fetch the
    /// files tree root so the change is visible now. The two requests ride the
    /// same ordered connection, so the backend flips the flag before it serves
    /// the tree.root. Gated to nav focus + Files mode at the call site; the
    /// tree.root reply is dropped in non-Files modes anyway. Toggling collapses
    /// the tree to its root — deeper dirs pick up the new visibility on their
    /// next expand (tree.children reads the same flag).
    pub(in crate::ui) fn toggle_hidden_files(&mut self) {
        if let Err(e) = self.send(OutgoingReq::ToggleHidden {
            workspace_id: self.active_workspace_id.clone(),
        }) {
            tracing::warn!(error = %e, "drop nav.toggle_hidden — channel closed");
            return;
        }
        if matches!(self.mode, Mode::Files) {
            tracing::info!("tree.root requested: toggle_hidden refresh");
            if let Err(e) = self.send(OutgoingReq::TreeRoot {
                mode: "files".to_string(),
                workspace_id: self.active_workspace_id.clone(),
            }) {
                tracing::warn!(error = %e, "drop tree.root after nav.toggle_hidden — channel closed");
            } else {
                // The visible rows now show the WRONG visibility — the
                // backend flag already flipped. Clear the view so every
                // in-flight path self-corrects (codex r4): stay in Files →
                // the reply set_roots the active view as before; switch
                // modes before the reply → an EMPTY view parks, so the
                // reply is accepted by the empty-only park instead of
                // dropped, and a return-to-Files before it lands refetches
                // via the empty-slot loader gate. Without this, a populated
                // parked slot dropped the reply and the stale visibility
                // stuck until a manual reload.
                self.tree = TreeView::new();
                self.tree_scroll = 0;
            }
        }
    }

    /// Live-refresh one directory's listing in the Files nav tree by re-fetching
    /// its `tree.children` (the reply runs `apply_children`, which *replaces*
    /// that dir's rows). Used by the file-watcher (`preview.changed`) path so a
    /// create/remove on disk shows up without a manual re-nav — mirrors the
    /// post-create/post-delete refresh the Ctrl+N / Ctrl+D flows already do.
    ///
    /// Guarded so a watcher event never *surprise-expands* a folder: only fires
    /// when `dir_id` is an already-expanded row in the current Files tree. A
    /// no-op outside Files mode, or when the dir isn't shown (collapsed / not
    /// expanded / a different workspace's path), in which case the reply's
    /// `apply_children` would ignore the unknown parent anyway.
    pub(in crate::ui) fn refresh_tree_dir_if_expanded(&mut self, dir_id: &str) {
        if self.mode != Mode::Files {
            return;
        }
        let shown_expanded = self
            .tree
            .rows
            .iter()
            .any(|r| r.node.id == dir_id && r.expanded);
        if !shown_expanded {
            return;
        }
        if let Err(e) = self.send(crate::net::transport::OutgoingReq::TreeChildren {
            parent_id: dir_id.to_string(),
            workspace_id: self.active_workspace_id.clone(),
        }) {
            tracing::warn!(error = %e, %dir_id, "drop tree.children (watcher refresh)");
        }
    }

    /// Re-list every expanded dir of a restored parked Files tree (see
    /// `expanded_files_dirs`). Fired on BOTH entries to a parked Files view —
    /// mode return (`enter_mode`) and workspace return
    /// (`switch_to_workspace`) — since either way the view comes back
    /// exactly as it was parked, having heard no watcher event meanwhile
    /// (the parked view used to come back stale and stay stale: 2026-08-17
    /// report for the mode case, 2026-09-14 for the workspace case). Lossless
    /// since `apply_children` became a MERGE: expanded subtrees and a nested
    /// cursor survive, rows re-anchor by node id, so a preview of a file
    /// created while the tree was parked keeps a row to sit on.
    pub(in crate::ui) fn refresh_restored_files_tree(&mut self) {
        for dir_id in expanded_files_dirs(&self.tree.rows) {
            if let Err(e) = self.send(crate::net::transport::OutgoingReq::TreeChildren {
                parent_id: dir_id.clone(),
                workspace_id: self.active_workspace_id.clone(),
            }) {
                tracing::warn!(error = %e, %dir_id, "drop tree.children (restored-tree refresh)");
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
}
