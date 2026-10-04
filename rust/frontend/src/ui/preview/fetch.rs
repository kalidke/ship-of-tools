//! The cursor-follow preview fetch: preview.get requests and their generations.

use crate::ui::*;

impl State {
    /// Whether the current preview source is a code shaper (`new_tokens` /
    /// `new_plain`) rather than markdown/image — i.e. one where line anchoring
    /// is meaningful. Keyed off the cached `preview_src` mime.
    fn preview_is_code(&self) -> bool {
        self.preview_src
            .as_ref()
            .map(|(m, _)| {
                m.starts_with("application/vnd.sot.tokens+json")
                    || (m.starts_with("text/") && m != "text/markdown" && m != "text/x-markdown")
            })
            .unwrap_or(false)
    }

    /// Switch-latency Phase 1: mint the next preview-slot request
    /// generation. Call this once per fired `preview.get`/
    /// `preview.set_scale` and stamp the result into the request — every
    /// `IncomingEvt::Preview` handler compares its echoed generation
    /// against `self.preview_req_gen`'s CURRENT value (read fresh at reply
    /// time, not the value captured here) to tell a stale reply from the
    /// latest one asked for. See the field doc for the full rationale.
    pub(in crate::ui) fn next_preview_gen(&mut self) -> u64 {
        self.preview_req_gen += 1;
        self.preview_req_gen
    }

    /// Same mechanism as `next_preview_gen`, for the concept/annotation slot.
    pub(in crate::ui) fn next_concept_gen(&mut self) -> u64 {
        self.concept_req_gen += 1;
        self.concept_req_gen
    }

    /// If the selected tree row's node id differs from the last one we
    /// asked for a preview of, fire a fresh `preview.get`. The Preview
    /// handler routes the response to the right pane based on mime.
    /// Modules-mode rows (no `files:` prefix) have no backend preview
    /// today; skip them rather than asking and getting an error back.
    /// C2: when a preview is pinned, cursor moves DON'T refresh the
    /// preview — the user is parked on the pinned node and the cursor
    /// is free to roam.
    pub(in crate::ui) fn maybe_fire_preview(&mut self) {
        if self.pinned_preview_node_id.is_some() {
            return;
        }
        // `--capture-preview` runs must show the requested node, full stop.
        // Restored nav state (the previous session's cursor) would otherwise
        // auto-fire its own preview and overwrite the captured one — the
        // root-row suppression at the dispatch site doesn't cover a restored
        // non-root cursor.
        if self.capture_preview_armed {
            return;
        }
        let Some(row) = self.tree.rows.get(self.tree.selected) else {
            return;
        };
        // Files-mode rows are already keyed `files:<relpath>` — fire
        // directly. Modules-mode rows carry an absolute `file` on
        // their payload; we synthesize the `files:<relpath>` id from
        // the cached scan project_root so `preview.get` reuses the
        // same backend codepath.
        let node_id = if row.node.id.starts_with("files:") {
            Some(row.node.id.clone())
        } else if let Some(file) = row.node.payload.get("file").and_then(|v| v.as_str()) {
            if file.is_empty() {
                None
            } else {
                let rel = match &self.scan_project_root {
                    Some(root) if !root.is_empty() => file
                        .strip_prefix(root)
                        .map(|s| s.trim_start_matches(['/', '\\']).to_string())
                        .unwrap_or_else(|| file.to_string()),
                    _ => file.to_string(),
                };
                Some(format!("files:{rel}"))
            }
        } else {
            None
        };
        // Capture the row's definition line (modules-mode rows carry it on
        // `line`) so the Preview reply handler can anchor the code preview to
        // the item. Files-mode rows have no `line` → None → opens at the top.
        let anchor_line = row
            .node
            .payload
            .get("line")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        let Some(id) = node_id else { return };
        // Blink guard: a freshly *driven* preview (fe-command / nav.preview) that
        // targeted a node not in the tree left the cursor parked on `id`'s row.
        // Don't fire `id`'s preview over the driven one while the cursor is still
        // parked there — that's the deep-path blink. The hold lifts as soon as
        // the cursor moves to a different row (then normal follow resumes).
        if let Some(held) = self.driven_preview_hold_cursor.clone() {
            if held == id {
                return;
            }
            self.driven_preview_hold_cursor = None;
        }
        if self.preview_node_id_fired.as_ref() == Some(&id) {
            // Same file already shown — no re-fetch needed. But items within a
            // module share a file, so the cursor moving between them must still
            // re-anchor the (already rendered) code preview to the new item.
            // Guard on a real change in target line so we don't re-anchor every
            // redraw and fight the user's manual scroll on a stable selection.
            if anchor_line != self.preview_anchored_to {
                if let Some(line) = anchor_line {
                    if line > 0 && self.preview_is_code() {
                        self.preview_scroll =
                            self.preview_md.anchor_scroll_for_def_line(line as usize);
                        self.window.request_redraw();
                    }
                }
                self.preview_anchored_to = anchor_line;
            }
            return;
        }
        // New target: drop any in-flight zoom re-raster so its reply can't
        // be mistaken for this fetch.
        self.preview_page_raster_pending = None;
        let (fit_w, fit_h) = self.preview_fit_px();
        let generation = self.next_preview_gen();
        if let Err(e) = self.send(crate::transport::OutgoingReq::PreviewGet {
            node_id: id.clone(),
            workspace_id: self.active_workspace_id.clone(),
            // Cursor-driven fetch always opens at page 1; the reply's
            // extras re-seed `preview_page` for the n/p transport.
            page: None,
            fit_w,
            fit_h,
            generation,
        }) {
            tracing::warn!(error = %e, %id, "drop preview.get request — channel closed");
            return;
        }
        self.preview_node_id_fired = Some(id);
        self.preview_anchor_line = anchor_line;
    }
}
