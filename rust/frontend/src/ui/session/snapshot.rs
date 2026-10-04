//! Per-row UI and REPL snapshots: what the window saves when it leaves a row and restores when it comes back.

use super::*;

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
/// REPL pane state (scrollback / input / history) lives in a sibling
/// snapshot type so the per-workspace eval routing in [`State`] can
/// land replies for non-active workspaces directly.
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
    /// Last preview source (mime + raw bytes) the chrome rendered. On
    /// swap-in we feed this back through `render_preview_source` to
    /// rebuild the preview pane without a fresh `preview.get`.
    preview_src: Option<(String, Vec<u8>)>,
    /// The node id the `Preview` reply that installed `preview_src`
    /// actually answered — MUST travel with it (same discipline as
    /// `preview_scale` below). Distinct from `preview_node_id_fired`,
    /// which flips the moment a NEW request is dispatched: between firing
    /// request B and B's reply landing, `preview_node_id_fired` already
    /// says B while `preview_src` (and this field) still hold A. Field
    /// report round 2: `previewed_files_path()` used to read
    /// `preview_node_id_fired`, so `o`/`W`/`O` pressed in that window
    /// routed against the file that was ABOUT to be shown, not the one
    /// on screen.
    preview_src_node_id: Option<String>,
    /// Terminally-failed figure URLs for the markdown doc in `preview_src`
    /// — MUST travel with it, alongside `current_md_node_id` and
    /// `current_md_workspace_id` below. Round-2 review finding: these
    /// three are provenance companions of the shown doc, not global
    /// state. Without snapshotting them, a workspace switch let workspace
    /// B's cursor-driven markdown reload clear workspace A's cached
    /// failures (a global `figure_failed.clear()`, since the field lived
    /// only on `State`), switching back to A could refire A's already-
    /// failed figure, B's surviving failures could wrongly collapse A's
    /// healthy one, and A's restored figure fetch could resolve against
    /// B's `current_md_node_id`/`current_md_workspace_id` — the wrong
    /// directory or project entirely.
    figure_failed: std::collections::HashSet<String>,
    /// See `figure_failed` above.
    current_md_node_id: Option<String>,
    /// See `figure_failed` above.
    current_md_workspace_id: Option<String>,
    /// Physical scale (ADR 0034) of the raster in `preview_src`. MUST travel
    /// with it: `preview_scale` is otherwise only ever set by a `preview.get`
    /// wire reply, and swap-in deliberately rebuilds the pane from the cached
    /// bytes WITHOUT re-fetching — so without this the restored image keeps
    /// whichever workspace's calibration was last on the wire. Showing a 2
    /// nm/px image labelled with another workspace's 10 nm/px bar is worse
    /// than showing no bar at all (Codex review of v0.4.3, F2).
    preview_scale: Option<PhysicalScale>,
    /// Concept-annotation backing data, including `synced_against`
    /// for the drift badge. The *shaped* MarkdownPreview is not in the
    /// snapshot (cosmic-text Buffer isn't Clone-able); preview_concept
    /// is cleared on swap-in and the cursor-tracking
    /// `maybe_fire_concept_read` re-shapes it on the next frame from
    /// the fresh wire reply. The brief no-concept frame is the cost.
    concept: Option<ConceptInfo>,
    /// Drift-badge bookkeeping — paths whose AST hash we know, and
    /// paths we've already asked `file.parse` for. Per-workspace so
    /// hashes from workspace A don't leak into workspace B's tree.
    file_ast_hashes: std::collections::HashMap<String, String>,
    file_parse_fired: std::collections::HashSet<String>,
    /// Concept-write modal state (header / buffer / dirty flag /
    /// banners). Captured so swap-back returns the user to mid-edit
    /// without losing typed content. preview_edit is *not* in the
    /// snapshot — it gets re-shaped by `rebuild_edit_preview` from
    /// edit_state on restore.
    edit_state: Option<EditState>,
}

/// Per-workspace REPL pane state. Captured at swap-out and restored at
/// swap-in alongside [`WorkspaceUiSnapshot`]. Lives in its own type so
/// reply routing (`ReplEvalDone` for non-active workspaces) can mutate
/// just this slice without touching general UI state.
///
/// Each workspace's REPL runs on its own kernel child (ADR 0014), so
/// the eval counter is naturally per-workspace too — when we route
/// replies back to the right log we keep the counter and the log in
/// sync.
#[derive(Clone)]
pub(in crate::ui) struct WorkspaceReplSnapshot {
    /// Submitted evals + the kernel's reply frames. Bounded the same
    /// way the live log is (last 256 entries) when captured.
    pub(in crate::ui) repl_log: Vec<ReplEntry>,
    /// Mid-typed input at swap time. Restored verbatim on swap-in so
    /// the user can keep editing whatever they were composing.
    repl_input: String,
    /// Per-workspace eval id counter. Backend doesn't require these
    /// to be globally unique; matching `eval_id → entry` works the
    /// same on every workspace.
    repl_eval_counter: u64,
    /// `]`/Backspace prompt-mode toggle (julia> vs pkg>) is per-
    /// workspace too — switching to a workspace mid-pkg-shell returns
    /// you to pkg>.
    repl_pkg_mode: bool,
    /// Scrollback offset captured at swap-out.
    repl_scroll: u16,
    /// History-walk state. `Some` means the workspace was in the
    /// middle of an Up/Down history walk; restoring puts the user
    /// back exactly where they were.
    history_pos: Option<usize>,
    history_saved: Option<String>,
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
                bl_pane_target: self.bl_pane_target.as_ref().map(|(_, s)| s.clone()),
                preview_node_id_fired: self.preview_node_id_fired.clone(),
                pinned_preview_node_id: self.pinned_preview_node_id.clone(),
                preview_src: self.preview_src.clone(),
                preview_src_node_id: self.preview_src_node_id.clone(),
                figure_failed: self.figure_failed.clone(),
                current_md_node_id: self.current_md_node_id.clone(),
                current_md_workspace_id: self.current_md_workspace_id.clone(),
                preview_scale: self.preview_scale.clone(),
                concept: self.concept.clone(),
                file_ast_hashes: self.file_ast_hashes.clone(),
                file_parse_fired: self.file_parse_fired.clone(),
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
        self.bl_pane_target = snap.bl_pane_target.map(|s| (key.0.clone(), s));
        self.preview_node_id_fired = snap.preview_node_id_fired;
        self.pinned_preview_node_id = snap.pinned_preview_node_id;
        // preview_concept gets re-shaped on the next frame by the
        // cursor-tracking concept.read path (memo cleared below). The
        // backing concept data is restored so the drift badge keeps
        // its synced_against until the fresh reply lands.
        self.preview_concept = None;
        self.concept = snap.concept;
        self.concept_target_fired = None;
        self.file_ast_hashes = snap.file_ast_hashes;
        self.file_parse_fired = snap.file_parse_fired;
        // Drop latched fires that never produced a hash: either long-dead
        // in-flights or a failed parse whose retry record was lost on
        // switch-away (`file_parse_retry` is deliberately not snapshotted),
        // which would otherwise restore as an un-re-armable latch — an
        // eternal "checking…" for exactly that path. Re-firing once on
        // restore is cheap and correct.
        self.file_parse_fired
            .retain(|p| self.file_ast_hashes.contains_key(p));
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
        // ADR 0034: the calibration travels with the cached bytes. Restored
        // BEFORE render_preview_source so the repaint (and any scalebar drawn
        // on it) uses THIS workspace's scale, never the one the departing
        // workspace happened to leave in place.
        self.preview_scale = snap.preview_scale.clone();
        // Travels with preview_src for the same reason preview_scale does:
        // the restored bytes are THIS workspace's, and `o`/`W`/`O` must
        // route against this workspace's file, not whatever the departing
        // workspace last had in flight.
        self.preview_src_node_id = snap.preview_src_node_id.clone();
        // Round-2 review finding: these three are provenance companions of
        // `preview_src`, not global state — restore them BEFORE
        // render_preview_source below, since its markdown branch reads all
        // three (figure_failed via figure_already_handled,
        // current_md_node_id/current_md_workspace_id to resolve relative
        // `![](url)`s). Restoring after would let this workspace's figures
        // dispatch against whichever OTHER workspace happened to leave
        // these set last.
        self.figure_failed = snap.figure_failed.clone();
        self.current_md_node_id = snap.current_md_node_id.clone();
        self.current_md_workspace_id = snap.current_md_workspace_id.clone();
        if let Some((mime, bytes)) = snap.preview_src.clone() {
            self.preview_src = Some((mime.clone(), bytes.clone()));
            self.render_preview_source(&mime, &bytes);
        } else {
            self.preview_src = None;
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
        self.workspace_repl_snapshots.insert(
            key,
            WorkspaceReplSnapshot {
                repl_log: self.repl_log.clone(),
                repl_input: self.repl_input.clone(),
                repl_eval_counter: self.repl_eval_counter,
                repl_pkg_mode: self.repl_pkg_mode,
                repl_scroll: self.repl_scroll,
                history_pos: self.history_pos,
                history_saved: self.history_saved.clone(),
            },
        );
    }

    /// Restore REPL state from a per-workspace snapshot. Returns true
    /// if a snapshot was found; otherwise resets to a clean REPL.
    pub(in crate::ui) fn restore_workspace_repl(&mut self, key: &WsKey) -> bool {
        if let Some(snap) = self.workspace_repl_snapshots.get(key).cloned() {
            self.repl_log = snap.repl_log;
            self.repl_input = snap.repl_input;
            self.repl_eval_counter = snap.repl_eval_counter;
            self.repl_pkg_mode = snap.repl_pkg_mode;
            self.repl_scroll = snap.repl_scroll;
            // The incoming log is a different length: nothing to pin against.
            self.repl_build_anchor = None;
            self.history_pos = snap.history_pos;
            self.history_saved = snap.history_saved;
            true
        } else {
            // First visit — empty REPL.
            self.repl_log.clear();
            self.repl_input.clear();
            self.repl_eval_counter = 0;
            self.repl_pkg_mode = false;
            self.repl_scroll = 0;
            self.repl_build_anchor = None;
            self.history_pos = None;
            self.history_saved = None;
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_slug_across_hosts_produces_distinct_ws_keys_and_snapshot_slots() {
        // ADR 0042 L2a codex review, item B: workspace_ui_snapshots (and
        // every sibling map) used to key by bare slug, so switching from
        // "sot" on host alpha to "sot" on host beta was the SAME map key
        // — restore_workspace_ui found alpha's stashed snapshot and
        // repainted it under beta. WsKey = (host, slug) makes the two
        // entries distinct slots, so a same-slug cross-host switch can
        // only ever hit its own host's entry.
        let key_alpha: WsKey = ("alpha".to_string(), "sot".to_string());
        let key_beta: WsKey = ("beta".to_string(), "sot".to_string());
        assert_ne!(
            key_alpha, key_beta,
            "same slug on two different hosts must not collide"
        );

        let mut snaps: HashMap<WsKey, &'static str> = HashMap::new();
        snaps.insert(key_alpha.clone(), "alpha's snapshot");
        snaps.insert(key_beta.clone(), "beta's snapshot");
        assert_eq!(snaps.get(&key_alpha), Some(&"alpha's snapshot"));
        assert_eq!(
            snaps.get(&key_beta),
            Some(&"beta's snapshot"),
            "beta's own snapshot must survive alpha's insert under the same slug"
        );
    }
}
