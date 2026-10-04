//! ADR 0019's command-file directory and the fe-state.json readback.

use super::*;

impl State {
    /// Write `fe-state.json` (ADR 0019) when the observable state changed
    /// since the last write. Called from the redraw path — a cheap signature
    /// check makes it a no-op on unchanged frames, so the readback file only
    /// touches disk when active workspace / mode / focus / slugs / rev move.
    pub(in crate::ui) fn maybe_write_fe_state(&mut self) {
        // B8 single-writer rule: only the primary FE owns fe-state.json.
        if self.ephemeral {
            return;
        }
        let Some(path) = fe_state_path() else {
            return;
        };
        let mode = self.mode.label();
        let focus = match self.focus {
            PaneFocus::NavTree => "nav",
            PaneFocus::Preview => "preview",
            PaneFocus::Llm => "llm",
            PaneFocus::Repl => "repl",
        };
        let active = self.active_workspace_id.clone();
        let host = self.host.clone();
        let rev = self.last_revision;
        let workspaces = self.workspace_slugs.clone();
        // ADR 0022: the active image preview, so the LLM pane knows what the
        // user is zoomed into. The signature buckets zoom (0.1) + ROI origin/
        // size (32 px) so a continuous pan/zoom gesture rewrites at a coarse
        // cadence instead of every frame; the body carries the exact ROI.
        let preview_sig: Option<(String, u32, u32, u32, u32, u32)> =
            self.preview_roi.as_ref().map(|r| {
                (
                    r.node_id.clone(),
                    (r.zoom * 10.0) as u32,
                    r.x >> 5,
                    r.y >> 5,
                    r.w >> 5,
                    r.h >> 5,
                )
            });
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        mode.hash(&mut hasher);
        focus.hash(&mut hasher);
        active.hash(&mut hasher);
        host.hash(&mut hasher);
        rev.hash(&mut hasher);
        workspaces.hash(&mut hasher);
        preview_sig.hash(&mut hasher);
        let sig = hasher.finish();
        if self.fe_state_sig == Some(sig) {
            return;
        }
        let preview = match &self.preview_roi {
            Some(r) => serde_json::json!({
                "node_id": r.node_id,
                "path": r.path,
                "dims": [r.src_w, r.src_h],
                "zoom": r.zoom,
                "roi": { "x": r.x, "y": r.y, "w": r.w, "h": r.h },
            }),
            None => serde_json::Value::Null,
        };
        let json = serde_json::json!({
            "rev": rev,
            "active_workspace": active,
            "workspaces": workspaces,
            "mode": mode,
            "focus": focus,
            "host": host,
            "preview": preview,
        });
        let body = match serde_json::to_vec(&json) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, "serialize fe-state failed");
                return;
            }
        };
        // Atomic temp+rename so a reader never sees a half-written file.
        let tmp = path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, &body).and_then(|_| std::fs::rename(&tmp, &path)) {
            tracing::warn!(error = %e, "write fe-state.json failed");
            return;
        }
        self.fe_state_sig = Some(sig);
    }
}

/// Directory of pending FE-control command files (ADR 0019). An in-terminal
/// agent (or the user) drops one JSON object per file here; the FE's command
/// watcher reads, deletes, and enqueues each for main-thread dispatch.
pub(in crate::ui) fn fe_commands_dir() -> Option<std::path::PathBuf> {
    sot_log::host::state_dir::sot_state_dir().map(|d| d.join("fe-commands"))
}

/// Path of the FE state-readback file (ADR 0019). The FE rewrites it
/// (atomic temp+rename) whenever the observable state changes, so an
/// in-terminal agent can poll it instead of screenshotting.
pub(in crate::ui) fn fe_state_path() -> Option<std::path::PathBuf> {
    sot_log::host::state_dir::sot_state_dir().map(|d| d.join("fe-state.json"))
}

/// Starts the persistent fe-command watcher thread (ADR 0019): it parses and
/// queues each JSON file dropped in `cmd_dir`, then wakes the window. `resumed`
/// calls it.
pub(in crate::ui) fn spawn_command_watcher(
    cmd_dir: std::path::PathBuf,
    queue: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<FeCommand>>>,
    waker: std::sync::Arc<winit::window::Window>,
) {
    if let Err(e) = std::thread::Builder::new()
        .name("sot-fe-command-watch".to_string())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(400));
            let entries = match std::fs::read_dir(&cmd_dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            // Sort by filename so a burst is processed roughly
            // FIFO (writers can prefix a counter/timestamp).
            let mut paths: Vec<std::path::PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
                .collect();
            paths.sort();
            let mut woke = false;
            for path in paths {
                let bytes = match std::fs::read(&path) {
                    Ok(b) => b,
                    Err(_) => continue,
                };
                // Delete first so a malformed file can't loop
                // forever on the next tick.
                let _ = std::fs::remove_file(&path);
                match serde_json::from_slice::<FeCommand>(&bytes) {
                    Ok(cmd) => {
                        if let Ok(mut q) = queue.lock() {
                            q.push_back(cmd);
                            woke = true;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            path = %path.display(),
                            "bad fe-command file dropped"
                        );
                    }
                }
            }
            if woke {
                waker.request_redraw();
            }
        })
    {
        tracing::warn!(error = %e, "failed to spawn fe-command watcher");
    }
}
