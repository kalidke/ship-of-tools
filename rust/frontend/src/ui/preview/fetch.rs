//! The cursor-follow preview fetch: preview.get requests and their generations.

use crate::ui::*;
use crate::net::transport::PreviewLink;
use super::requests::{PageRequestContext, PreviewRequests};

impl PageRequestContext for State {
    fn page_requests(&mut self) -> &mut PreviewRequests { &mut self.preview_requests }

    fn shown_page(&self) -> Option<(u32, u32)> {
        if self.preview_source.node_id()? != self.preview_requests.refetch_target()? { return None; }
        self.pages.page()
    }

    fn send_page_request(&mut self, page: u32, generation: u64) -> Result<(), String> {
        let node_id = self.preview_requests.refetch_target().ok_or("no preview target")?.to_string();
        let (fit_w, fit_h) = self.preview_fit_px();
        self.send(OutgoingReq::PreviewGet {
            node_id, workspace_id: self.selected.workspace_id().clone(), page: Some(page), fit_w, fit_h, generation,
        }).map_err(|e| e.to_string())
    }
}

impl State {
    /// Whether the current preview source is a code shaper (`new_tokens` /
    /// `new_plain`) rather than markdown/image — i.e. one where line anchoring
    /// is meaningful. Keyed off the installed source's mime.
    fn preview_is_code(&self) -> bool {
        self.preview_source
            .mime()
            .map(|m| {
                m.starts_with("application/vnd.sot.tokens+json")
                    || (m.starts_with("text/") && m != "text/markdown" && m != "text/x-markdown")
            })
            .unwrap_or(false)
    }

    /// Switch-latency Phase 1: mint the next preview-slot request
    /// generation. Call this once per fired `preview.get`/
    /// `preview.set_scale` and stamp the result into the request — every
    /// `IncomingEvt::Preview` handler compares its echoed generation
    /// against the owner's CURRENT value (read fresh at reply
    /// time, not the value captured here) to tell a stale reply from the
    /// latest one asked for. See the field doc for the full rationale.
    pub(in crate::ui) fn next_preview_gen(&mut self) -> u64 {
        self.preview_requests.issue_preview()
    }

    /// `next_preview_gen` for a re-fetch the person did not ask for: what its reply raises goes to the preview's box alone.
    pub(in crate::ui) fn next_refetch_gen(&mut self) -> u64 {
        self.preview_requests.issue_refetch()
    }

    pub(in crate::ui) fn shown_return_file(&self) -> Option<&str> {
        let node = self.preview_source.node_id()?;
        let shown = self.preview_shows();
        (node.strip_prefix("files:").is_some_and(|path| !path.is_empty()) && Some(node) == self.shown_target()
            && self.pages.page().is_none() && self.preview_source.scale().is_none()
            && !self.preview_requests.unconfirmed_scales.contains(&(self.selected.host().clone(), self.selected.workspace_id().clone(), node.to_owned()))
            && !self.hosts.preview_links.values().any(|link| *link.borrow() == PreviewLink::Preamble)
            && !shown.canvas && !shown.fatal && !self.editor.is_editing()).then_some(node)
    }

    // A directory reply carries no kind. Its installed page can outlive the directory in the tree.
    fn is_folder_page(&self, node: &str) -> bool {
        let Some(name) = node.strip_prefix("files:").and_then(|p| std::path::Path::new(p).file_name()).and_then(|n| n.to_str()) else { return false };
        let suffix = b"\n\nFiles-mode root. Navigate the tree to preview individual files.\n";
        self.preview_source.mime() == Some("text/markdown") && self.preview_source.bytes().and_then(|b| b.strip_prefix(b"# "))
            .and_then(|b| b.strip_suffix(suffix.as_slice())) == Some(name.as_bytes())
    }

    pub(in crate::ui) fn check_returned_file(&mut self, now: std::time::Instant, pane: ratatui::layout::Rect) {
        use super::requests::PreviewRead;
        let PreviewRead::Settling(since) = self.preview_requests.read() else { return };
        if self.nav_cursor.is_settling() || now.saturating_duration_since(*since) < crate::ui::app::NAV_FIRE_DEBOUNCE {
            self.pacer.mark_dirty();
            return;
        }
        *self.preview_requests.read() = PreviewRead::Idle;
        if self.hosts.host_connected.get(self.selected.host()) == Some(&false) { return; }
        let Some(node) = self.shown_return_file().filter(|n| !pane.is_empty() && !self.is_folder_page(n)) else { return };
        let node_id = node.to_owned();
        let (fit_w, fit_h) = self.preview_fit_px();
        // Hold the transport phases through enqueue. Retirement takes its write lock before draining non-replayable requests.
        let links = self.hosts.preview_links.clone();
        let phases: Vec<_> = links.iter().map(|(host, link)| (host, link.borrow())).collect();
        if phases.iter().any(|(_, p)| **p == PreviewLink::Preamble)
            || !phases.iter().any(|(host, p)| *host == self.selected.host() && **p == PreviewLink::Ready) { return; }
        let owner = (self.selected.host().clone(), self.selected.workspace_id().clone());
        let generation = self.next_refetch_gen();
        match self.send(OutgoingReq::ReturnPreview { node_id, workspace_id: owner.1.clone(), fit_w, fit_h, generation }) {
            Ok(()) => {
                self.preview_requests.return_out = Some((owner.0, owner.1, generation, links[self.selected.host()].subscribe()));
                *self.preview_requests.read() = PreviewRead::Checking;
            }
            Err(e) => tracing::warn!(%e, "drop preview.get on return — channel closed"),
        }
    }

    /// Same mechanism as `next_preview_gen`, for the concept/annotation slot.
    pub(in crate::ui) fn next_concept_gen(&mut self) -> u64 {
        self.preview_requests.issue_concept_reload()
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
        if self.nav_cursor.pinned().is_some() {
            return;
        }
        // `--capture-preview` runs must show the requested node, full stop.
        // Restored nav state (the previous session's cursor) would otherwise
        // auto-fire its own preview and overwrite the captured one — the
        // root-row suppression at the dispatch site doesn't cover a restored
        // non-root cursor.
        if self.capture.preview_armed() {
            return;
        }
        let Some(row) = self.nav.tree().rows().get(self.nav.tree().selected()) else {
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
                let rel = match self.nav.scan_project_root() {
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
        if self.preview_requests.suppress_follow(&id) {
            return;
        }
        if self.preview_requests.refetch_target() == Some(id.as_str()) {
            // Same file already shown — no re-fetch needed. But items within a
            // module share a file, so the cursor moving between them must still
            // re-anchor the (already rendered) code preview to the new item.
            // Guard on a real change in target line so we don't re-anchor every
            // redraw and fight the user's manual scroll on a stable selection.
            let is_code = self.preview_is_code();
            let (now, length) = self.scroll_ask();
            if self.shaped.reanchor(anchor_line, is_code, now, length) {
                self.window.request_redraw();
            }
            return;
        }
        // New target: drop any in-flight zoom re-raster so its reply can't
        // be mistaken for this fetch.
        self.preview_requests.cancel_reraster();
        let (fit_w, fit_h) = self.preview_fit_px();
        let generation = self.next_preview_gen();
        if let Err(e) = self.send(crate::net::transport::OutgoingReq::PreviewGet {
            node_id: id.clone(),
            workspace_id: self.selected.workspace_id().clone(),
            // Cursor-driven fetch always opens at page 1; the reply's
            // extras re-seed the page for the n/p transport.
            page: None,
            fit_w,
            fit_h,
            generation,
        }) {
            tracing::warn!(error = %e, %id, "drop preview.get request — channel closed");
            return;
        }
        self.preview_requests.record_fired(id);
        self.shaped.arm_anchor(anchor_line);
    }
}

#[cfg(test)]
#[path = "fetch_tests.rs"]
mod tests;
