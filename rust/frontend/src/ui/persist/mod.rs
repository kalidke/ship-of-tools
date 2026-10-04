//! Persisted window state: layered settings, config-file discovery and the per-host resume snapshot.

use super::*;

pub(crate) mod discover;
pub(crate) mod resume;
pub(crate) mod settings;

impl State {
    /// B5: persist (last_mode, last_bl_target) so a fresh launch resumes
    /// where the user left off. Called from each Mode-switch and each
    /// attach_session_to_bl; not from every cursor move (those don't
    /// reflect coarse resume state).
    pub(super) fn persist_resume_state(&self) {
        // Harness instances never write the per-host shared state (B8
        // single-writer rule) — a driver/capture FE would clobber the
        // primary FE's resume state.
        if self.ephemeral {
            return;
        }
        // A minimized window's own size/position reads as (near-)zero on
        // some platforms; persisting that produced an unfindable 0x0
        // window on the next launch (field report, 2026-09-17). Skip the
        // whole snapshot (this is a full-overwrite write, not a merge)
        // while minimized rather than trying to filter just the geometry
        // fields out of it — whatever was last saved non-minimized stays on
        // disk untouched.
        if self.window.is_minimized().unwrap_or(false) {
            return;
        }
        // Snapshot window geometry in *logical* pixels so the next
        // launch's `with_inner_size` / `with_position` lands cleanly
        // regardless of the current monitor's DPR.
        let scale = self.window.scale_factor();
        let inner = self.window.inner_size().to_logical::<f64>(scale);
        let pos = self
            .window
            .outer_position()
            .ok()
            .map(|p| p.to_logical::<f64>(scale));
        let s = crate::ui::persist::resume::GlobalState {
            last_mode: Some(self.mode.label().to_string()),
            // The persisted GlobalState is still single-host -- drop the
            // owner, keep the session name. `last_workspace_id` and
            // `last_bl_target` are meaningful only paired with
            // `last_host` below (State::new's `resume_matches_last_host`
            // gate is the read side of that pairing: it discards both
            // when the resumed active_host isn't the one they were
            // saved for).
            last_bl_target: self.bl_pane_target.as_ref().map(|(_, s)| s.clone()),
            last_workspace_id: self.active_workspace_id.clone(),
            // ADR 0042 L2a codex review, item H: REPURPOSED. This field
            // was ADR 0015's "pick a host, Ctrl+Q + relaunch" signal to
            // the LAUNCHER (which host to route the SSH tunnel + remote
            // daemon spawn to) -- superseded by L2a's multi-host
            // connection set, and the launcher's own read of it is
            // deleted (item I; per-host tunnels are a later slice). Its
            // NEW meaning, entirely FE-internal: the active host at
            // quit, so a daily launch resumes wherever the user actually
            // left off rather than always the configured default (see
            // State::new's active_host resolution).
            last_host: Some(self.active_host.clone()),
            window_w: Some(inner.width),
            window_h: Some(inner.height),
            window_x: pos.as_ref().map(|p| p.x),
            window_y: pos.as_ref().map(|p| p.y),
            fullscreen: Some(self.window.fullscreen().is_some()),
            font_scale: Some(self.text_scale_mult as f64),
            nav_selected_id: self
                .tree
                .rows
                .get(self.tree.selected)
                .map(|r| r.node.id.clone()),
            nav_scroll: Some(self.tree_scroll),
        };
        crate::ui::persist::resume::save(&s);
    }
}
