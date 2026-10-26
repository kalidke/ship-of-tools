//! Per-row UI and REPL snapshots: what the window saves when it leaves a row and restores when it comes back.

use super::*;
use crate::ui::preview::source::InstalledPreviewSource;

/// One workspace's snapshot of the chrome's view state, captured on
/// workspace switch and restored when the user comes back. Goal: the
/// frame after a swap-in looks identical to the frame before the swap-
/// out, modulo events that have arrived in the interim.
///
/// Captures *everything* mode-bearing about the chrome EXCEPT the nav
/// tree: which mode it was rendering, the cached preview source (so the
/// rendered preview repaints without a backend round-trip), the
/// concept-annotation slot, drift badge bookkeeping, and the
/// Sessions-mode pane-capture dedup memo. The nav tree (cursor +
/// expanded folders + scroll) lives in `State.tree_store`, keyed by
/// (mode, scope) — see the tree-provenance redesign note on `mode`.
///
/// REPL pane state (scrollback / input / history) lives in a private
/// record in `ReplModel`, which routes replies for parked rows independently.
///
/// Held in `State.workspace_ui_snapshots`, keyed by workspace slug
/// (with `<default>` for the daemon-default workspace).
#[derive(Clone)]
pub(in crate::ui) struct WorkspaceUiSnapshot {
    /// Last mode the workspace was rendering. Dropped-then-restored so a
    /// workspace left in Modules mode comes back in Modules mode, not
    /// forced into Files.
    ///
    /// NOTE (tree-provenance redesign): the nav TREE no longer travels in
    /// this snapshot. Trees live in `State.tree_store`, keyed by
    /// `(Mode, TreeScope)` — `switch_to_workspace` stashes/loads through
    /// the store, so a snapshot can never hand back another (workspace,
    /// mode)'s rows (the Codex R4 "laundered provenance" class is
    /// structurally gone, along with the `files_tree_workspace` stamp and
    /// `tree_scroll` that used to ride here).
    mode: Mode,
    /// Tmux session the BL pane was attached to (`sot-be-<slug>`).
    /// Restored via `attach_session_to_bl` on swap-in.
    pub(in crate::ui) bl_pane_target: Option<String>,
    /// The node id we most recently asked the backend to preview, so
    /// switching back doesn't bounce-fetch the same preview again.
    preview_node_id_fired: Option<String>,
    /// C2 pin-and-leave state for this workspace. `Some` if a node was
    /// pinned when the user swapped away; restored on swap-in so the
    /// preview stays parked on the same file across workspace switches.
    pinned_preview_node_id: Option<String>,
    /// The installed preview source the chrome rendered: bytes, the node
    /// their reply answered, their physical scale and the markdown origin
    /// (see `InstalledPreviewSource`). On swap-in it is restored whole and
    /// its bytes fed back through `render_preview_source` to rebuild the
    /// preview pane without a fresh `preview.get`.
    source: InstalledPreviewSource,
    /// Terminally-failed figure URLs for the markdown doc in `source` —
    /// MUST travel with it (round-2 review finding: a provenance companion
    /// of the shown doc, not global state). Without snapshotting it, a
    /// workspace switch let workspace B's cursor-driven markdown reload
    /// clear workspace A's cached failures, switching back to A could
    /// refire A's already-failed figure, and B's surviving failures could
    /// wrongly collapse A's healthy one.
    figure_failed: std::collections::HashSet<String>,
    /// Concept-annotation backing data, including `synced_against`
    /// for the drift badge. The *shaped* MarkdownPreview is not in the
    /// snapshot (cosmic-text Buffer isn't Clone-able); preview_concept
    /// is cleared on swap-in and the cursor-tracking
    /// `maybe_fire_concept_read` re-shapes it on the next frame from
    /// the fresh wire reply. The brief no-concept frame is the cost.
    concept: Option<ConceptInfo>,
    /// Concept-write modal state (header / buffer / dirty flag /
    /// banners). Captured so swap-back returns the user to mid-edit
    /// without losing typed content. preview_edit is *not* in the
    /// snapshot — it gets re-shaped by `rebuild_edit_preview` from
    /// edit_state on restore.
    edit_state: Option<EditState>,
}

impl State {
    /// Capture the chrome's current view state into the per-workspace
    /// snapshot map. Called immediately before changing
    /// `active_workspace_id` so the workspace we're leaving keeps every
    /// mode-bearing UI bit: nav tree + scroll + focus, preview source
    /// for repaint, concept slot, drift bookkeeping, pty target.
    pub(in crate::ui) fn snapshot_current_workspace_ui(&mut self) {
        let key = self.active_ws_key();
        self.workspace_ui_snapshots.insert(
            key,
            WorkspaceUiSnapshot {
                mode: self.mode,
                // (tree + scroll deliberately absent — they stash into
                // `tree_store` under their own key in switch_to_workspace.)
                // Host dropped — redundant with this snapshot's own WsKey
                // (the map's key), which is `active_host` by construction
                // at snapshot time.
                bl_pane_target: self.agent_pane.snapshot_target(),
                preview_node_id_fired: self.preview_requests.fired_target().map(str::to_owned),
                pinned_preview_node_id: self.pinned_preview_node_id.clone(),
                source: self.preview_source.clone(),
                figure_failed: self.figure_failed.clone(),
                concept: self.concept.clone(),
                edit_state: self.edit_state.clone(),
            },
        );
    }

    /// If a snapshot exists for the workspace keyed by `key`, restore
    /// the chrome to it and return `true`. Caller skips the `tree.root`
    /// re-fetch in that case and the preview repaints from the cached
    /// source. Returns `false` if there's no prior state for this
    /// workspace — caller falls back to fetching fresh.
    pub(in crate::ui) fn restore_workspace_ui(&mut self, key: &WsKey) -> bool {
        let Some(snap) = self.workspace_ui_snapshots.get(key).cloned() else {
            return false;
        };
        // The nav tree does NOT restore from this snapshot — it swaps
        // through `tree_store` in `switch_to_workspace` (this fn's only
        // caller), keyed by (restored mode, entering workspace). Restoring
        // it here from a per-workspace blob is exactly what used to hand
        // back a foreign tree (Codex R4). Only the mode is restored, so the
        // caller's post-restore key computation picks the right slot.
        self.mode = snap.mode;
        // focus is global across workspaces — don't restore. The user
        // expects pane focus to follow their last interaction regardless
        // of which workspace is active.
        // Re-pair with `key`'s own host (ADR 0042 L2a) -- this snapshot
        // was captured while that host was active, so its bare session
        // name always belonged to it.
        self.agent_pane.restore_snapshot_target(key.0.clone(), snap.bl_pane_target);
        self.preview_requests.restore_fired(snap.preview_node_id_fired);
        self.pinned_preview_node_id = snap.pinned_preview_node_id;
        // preview_concept gets re-shaped on the next frame by the
        // cursor-tracking concept.read path (memo cleared below). The
        // backing concept data is restored so the drift badge keeps
        // its synced_against until the fresh reply lands.
        self.preview_concept = None;
        self.concept = snap.concept;
        self.preview_requests.reset_concept_follow();
        // Not snapshotted: while this workspace was parked the daemon sent no `preview.changed` for it, so a
        // cached hash may be of bytes since edited. Each file parses again at its next view.
        clear_drift(&mut self.file_ast_hashes, &mut self.file_parse_fired, &mut self.file_parse_retry);
        // Restore the edit modal — including dirty/discard/stale
        // banners — and re-shape its preview from the buffer.
        self.edit_state = snap.edit_state;
        self.rebuild_edit_preview();
        // Clear the stale Quads then repaint from the cached source.
        // render_preview_source rebuilds preview_md/png/svg for the
        // restored mime; if the leaving workspace had nothing rendered
        // we just leave the panes empty.
        self.preview_png = None;
        self.preview_svg = None;
        // The installed source travels whole: restored BEFORE
        // render_preview_source so the repaint (and any scalebar drawn on
        // it) uses THIS workspace's calibration (ADR 0034), `o`/`W`/`O`
        // route against this workspace's file, and the markdown branch
        // resolves relative `![](url)`s against this workspace's document —
        // never whatever the departing workspace left in place. Its
        // figure_failed is restored first too, since that branch reads it
        // (via figure_already_handled).
        self.preview_source = snap.source.clone();
        self.figure_failed = snap.figure_failed.clone();
        if let Some((mime, bytes)) = self.preview_source.source_copy() {
            self.render_preview_source(&mime, &bytes);
        }
        // NOTE: no foreign-provenance corrective reload here or anywhere —
        // the class is retired. The caller swaps the tree in from the store
        // by (mode, workspace) key after this returns; a slot can only ever
        // hold its own key's tree.
        self.window.request_redraw();
        true
    }

    /// Capture the current REPL pane state into the per-workspace
    /// snapshot map. Called from `switch_to_workspace` alongside
    /// `snapshot_current_workspace_ui` so leaving a workspace mid-eval
    /// (or mid-typing) survives the round trip.
    pub(in crate::ui) fn snapshot_current_workspace_repl(&mut self) {
        let key = self.active_ws_key();
        self.repl_model.capture_row(key);
    }

    /// Restore REPL state from a per-workspace snapshot. Returns true
    /// if a snapshot was found; otherwise resets to a clean REPL.
    pub(in crate::ui) fn restore_workspace_repl(&mut self, key: &WsKey) -> bool {
        let (found, effects) = self.repl_model.restore_row(key);
        self.apply_repl_effects(effects);
        found
    }
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
