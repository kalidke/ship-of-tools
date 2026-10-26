//! Switching the nav mode: the active tree's key, the store swap and entering a mode.

use super::*;

impl State {
    /// The key the active tree currently belongs to. Computed, never stored —
    /// so it can't drift from `(mode, active host, active workspace)`.
    pub(in crate::ui) fn active_tree_key(&self) -> TreeKey {
        self.nav.active_key(&self.active_ws_key())
    }

    /// The single stash/load seam for the active tree. Every mode or
    /// workspace change routes through here: park the departing view under
    /// `old_key`, bring in `new_key`'s stored slot (or an empty view for a
    /// first visit). Callers compute `old_key` BEFORE changing the mode or
    /// the selected row and `new_key` after.
    ///
    /// The Files root-label reconcile lives here (moved from
    /// `restore_workspace_ui`): a loaded Files tree deliberately skips the
    /// re-fetch, so a root row captured under a stale label is patched to
    /// the active workspace's label on the way in.
    pub(in crate::ui) fn swap_active_tree(&mut self, old_key: TreeKey, new_key: TreeKey) {
        let label = self.active_workspace_label();
        self.nav.swap(old_key, new_key, label);
    }

    /// Force Files mode through the store seam. The badge/pending-nav
    /// consume paths flip the mode as a side-effect of driving a file
    /// preview; the flip must stash/load like any other mode change or the
    /// Files view would show the departing mode's rows (the old
    /// restore-validity hole).
    pub(in crate::ui) fn force_files_mode(&mut self) {
        if matches!(self.nav.mode(), Mode::Files) {
            return;
        }
        let old_key = self.active_tree_key();
        self.nav.set_mode(Mode::Files);
        let new_key = self.active_tree_key();
        self.swap_active_tree(old_key, new_key);
    }

    /// Switch the nav mode and fire that mode's data fetch. Shared by the
    /// f/m/s/h keybinds and the ADR-0019 `mode` command; no-op if already in
    /// `mode`.
    pub(in crate::ui) fn enter_mode(&mut self, mode: Mode) {
        if self.nav.mode() == mode {
            return;
        }
        tracing::info!(target: "sot::ui::nav::tree_store", from = ?self.nav.mode(), to = ?mode, "enter_mode (tree swap + refresh follows)");
        // Swap the active tree through the store: the departing mode's tree
        // (cursor, expansion, scroll) parks under its key and the entering
        // mode's parked tree — if any — comes back, which is what makes
        // cursor position per-mode-persistent across switches. The loader
        // below fires ONLY for an empty (never-loaded) view — the same
        // empty-slot gate switch_to_workspace uses. An unconditional refetch
        // here would defeat the persistence it restores: set_root/set_flat
        // rebuild root+level-1, so a cursor on a NESTED file (not in the
        // fresh level-1 rows) falls to 0 and every expansion collapses
        // (codex r3). Staleness of a restored view is the accepted residual
        // (parked-slot refresh policy, ops TODO) — identical to the
        // workspace-switch restore semantics.
        let old_key = self.active_tree_key();
        self.nav.set_mode(mode);
        let new_key = self.active_tree_key();
        self.swap_active_tree(old_key, new_key);
        // A mode change invalidates a one-shot Files reveal armed by
        // drive_same_ws_open / a workspace switch (the user navigated away from
        // the file they were being shown). Symmetric with the
        // switch_to_workspace clear. (The old cross-mode edge — a `files`
        // tree.root admitted while in Modules mode — is gone: installs route
        // by key now, so a Files reply can't touch a Modules view at all.)
        self.reveal.cancel_root();
        match mode {
            // Files/Modules: fire the loader ONLY for an empty (never-loaded)
            // view — a restored view keeps its nested cursor + expansion
            // (the refetch's set_root/set_flat would collapse them; codex
            // r3). Content staleness is the accepted parked-slot residual.
            Mode::Files => {
                if self.nav.tree().rows().is_empty() {
                    if let Err(e) = self.send(OutgoingReq::TreeRoot {
                        mode: "files".to_string(),
                        workspace_id: self.selected.workspace_id().clone(),
                    }) {
                        tracing::warn!(target: "sot::ui::nav::tree_store", error = %e, "drop tree.root request — channel closed");
                    }
                } else {
                    self.refresh_restored_files_tree();
                }
            }
            Mode::Modules => {
                if self.nav.tree().rows().is_empty() {
                    let generation = self
                        .next_project_scan_gen(self.selected.host().clone(), self.selected.workspace_id().clone());
                    if let Err(e) = self.send(OutgoingReq::ProjectScan {
                        workspace_id: self.selected.workspace_id().clone(),
                        generation,
                    }) {
                        tracing::warn!(target: "sot::ui::nav::tree_store", error = %e, "drop project.scan request — channel closed");
                    }
                }
            }
            // Sessions/Hosts: ALWAYS refresh — these are live status lists
            // where freshness beats restore, their rows are level-1 so
            // set_root's node-id re-anchor keeps the cursor losslessly, and
            // Sessions' parked slot deliberately goes stale between visits
            // (populated parks drop refreshes under the empty-only rule).
            Mode::Sessions => {
                // ADR 0042 L2a codex review, item A: Sessions mode's tree
                // spans EVERY connected host (host-grouped), so entering
                // it is exactly the "explicit global refresh" case — fan
                // out to every connection, not just active_host, or a
                // non-active host's rows go stale/empty while the user is
                // looking straight at them.
                for (host, _) in &self.conns {
                    if let Err(e) = self.send_to(host, OutgoingReq::WorkspaceList) {
                        tracing::warn!(target: "sot::ui::nav::tree_store", error = %e, %host, "drop workspace.list request — channel closed");
                    }
                }
            }
            Mode::Hosts => {
                self.populate_hosts_tree();
                self.select_active_host();
            }
        }
        self.persist_resume_state();
    }
}
