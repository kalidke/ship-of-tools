//! Preview replies: preview.get and set_scale results and failures, preview.changed, concept.read
//! and concept.write, the browser opens (pluto, docs, video, quarto), and the refused-hello
//! overlay (set on a refusal, cleared by a clean hello).

use crate::ui::*;
use super::requests::{self, RequestReplyContext, PreviewRequests};

impl State {
    pub(crate) fn clear_hello_refused(&mut self, event_host: HostKey) {
        // ADR 0030 §2: a clean hello means the refusal (a protocol skew
        // or an account refusal, if any) is resolved — clear the
        // blocking overlay so the chrome returns to normal.
        // ADR 0042 L2a: only THIS host's mismatch entry is
        // resolved -- a clean hello from host A must not erase
        // host B's still-real refusal. Clearing preview_fatal
        // unconditionally is still correct: it's a projection
        // of active_host's entry, rebuilt lazily at draw time
        // either way (harmless extra rebuild if this wasn't
        // the active host's mismatch to begin with).
        self.hello_refused.remove(&event_host);
        self.preview_fatal = None;
    }

    pub(crate) fn on_hello_refused(&mut self, event_host: HostKey, message: String) {
        // ADR 0030 §2: hard FE/BE version skew; ADR 0049: the daemon
        // refused this window's account. Latch the blocking
        // overlay (rebuilt lazily in the draw once md_rect_px is
        // known, so it wraps to the real preview width) and mirror
        // a short line to the status bar.
        // ADR 0042 L2a: per-host -- a stale/optional remote's
        // refusal must not block the whole UI while every
        // other (healthy) host works fine. Only active_host's
        // entry is ever projected to the blocking overlay
        // (rebuild_fatal_overlay/show_fatal).
        self.hello_refused.insert(event_host.clone(), message);
        self.preview_fatal = None;
        self.status = "the backend refused this window (see preview)".to_string();
    }

    pub(crate) fn on_concept_read(
        &mut self,
        event_host: HostKey,
        target: String,
        workspace_id: Option<String>,
        exists: bool,
        content: String,
        generation: u64,
    ) {
        requests::continue_concept_reply(self, event_host, workspace_id, generation, target, (exists, content));
    }

    fn reload_concept_editor(&mut self, exists: bool, content: &str) {
        self.editor.reload_concept(exists, content);
        self.rebuild_edit_preview();
    }

    fn install_concept_read(&mut self, target: String, exists: bool, content: String) {
        let synced_against = if exists {
            sot_protocol::annotation::synced_against(&content)
        } else {
            None
        };
        if exists {
            let body = split_frontmatter(&content).1;
            self.preview_concept = Some(MarkdownPreview::new(
                self.text.font_system_mut(),
                &body,
                self.concept_rect_px.w.max(1.0),
                self.concept_rect_px.h.max(1.0),
                self.scale,
                &MathMetricsMap::new(),
                &FigureMetricsMap::new(),
                &self.highlight_service,
                &self.markdown_token_cache,
            ));
        } else {
            self.preview_concept = None;
        }
        self.concept = Some(ConceptInfo {
            target,
            exists,
            content,
            synced_against,
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn on_preview(
        &mut self,
        event_host: HostKey,
        node_id: Option<String>,
        workspace_id: Option<String>,
        mime: String,
        bytes: Vec<u8>,
        extras: Option<serde_json::Value>,
        generation: u64,
    ) {
        let payload = PreviewReply { node_id, workspace_id: workspace_id.clone(), mime, bytes, extras, generation };
        requests::continue_preview_reply(self, event_host, workspace_id, generation, payload);
    }

    fn install_preview_reply(&mut self, payload: PreviewReply) {
        let PreviewReply { node_id, workspace_id, mime, bytes, extras, generation } = payload;
        // Cache the source so a runtime font-size change
        // can re-render at the new scale without a
        // round-trip to the backend, with the node THIS reply
        // actually answered, not the last one requested
        // (field report round 2: `previewed_files_path()` reads
        // it so `o`/`W`/`O` route against what's actually
        // painted even when a newer request is already in
        // flight ahead of its reply). ADR 0034: its physical
        // scale for the scalebar overlay, cleared on every reply
        // like pagination — a reply with no `physical_scale`
        // retires the bar for the new target. For markdown, the
        // node id + workspace that served the buffer: figure URL
        // resolution needs the markdown file's directory, and
        // figure fetches must go to the same workspace
        // (otherwise active_workspace_id drift sends the
        // request to a project that doesn't have the file).
        let scale = extras.as_ref().and_then(parse_physical_scale);
        self.preview_source.install(mime.clone(), bytes.clone(), node_id.clone(), scale, workspace_id);
        // Pagination state (ADR 0021): present only when the
        // serving plugin reported page extras; anything else
        // (including a later unpaginated reply for a new
        // cursor target) clears it, retiring the n/p keys.
        self.preview_page = extras.as_ref().and_then(|e| {
            let page = e.get("page")?.as_u64()? as u32;
            let count = e.get("page_count")?.as_u64()? as u32;
            Some((page, count))
        });
        // Resolve a live-entry save: this same handler serves the
        // `set_scale` reply (one install path, by design), so it's
        // where "saving…" has to be retired. Gated on the pending
        // marker so ordinary previews never trigger it.
        // Only THIS save's target may resolve it — otherwise
        // navigating away mid-save lets the new file's preview
        // consume the marker and label an unrelated image "saved".
        let scale_saved = self.preview_requests.settle_scale_save(node_id.as_deref());
        if let Some(raw) = scale_saved {
            self.status = if self.preview_source.scale().is_some() {
                format!("pixel size {raw} nm · saved")
            } else {
                // Reply landed but carried no scale — report that
                // rather than claiming a save that didn't stick.
                format!("pixel size {raw} nm · saved, but no scale came back")
            };
        }
        // Is this the higher-res reply to a zoom re-raster of the
        // page already on screen? Only if a re-raster is pending
        // for the SAME page — a reply for a different page is a
        // real navigation (page turn / cursor move) and resets the
        // view to fit.
        match self.preview_requests.settle_page_reply(self.preview_page.map(|(page, _)| page)) {
            Some(z) => {
                self.preview_page_raster_zoom = z;
                self.preview_reraster_keep_view = true;
            }
            None => {
                self.preview_page_raster_zoom = 1.0;
                self.preview_reraster_keep_view = false;
            }
        }
        if matches!(mime.as_str(), "text/markdown" | "text/x-markdown") {
            // A fresh preview.get REPLY landing here (as
            // opposed to a cached-bytes reflow — see the note
            // in render_preview_source) is the one genuine
            // "this document just reloaded" event: new
            // evidence that a figure which failed before
            // (fired before its target existed) may exist
            // now. Clear the failure set here, once, so the
            // walk render_preview_source is about to run
            // gets a clean shot at every `![](url)` via
            // dispatch_pending_figures. figure_cache hits and
            // in-flight figure_pending entries are untouched.
            self.figure_failed.clear();
        }
        self.render_preview_source(&mime, &bytes);
        self.result_preview_installed(generation, node_id.as_deref());
        // ADR 0025 `preview --roi`: certify a pending aim once its
        // image is the INSTALLED quad. A preview reply installs
        // whatever arrived last (node-unchecked above), so the
        // render-pass solve gates on this — never on the previous
        // file's quad. The solve itself stays in the render pass,
        // where the live pane geometry exists.
        let row_key = self.active_result_row_key();
        let raster_installed = is_raster_preview_mime(&mime) && self.preview_png.is_some();
        if let Some(dropped) =
            self.preview_requests.certify_roi_aim(row_key.as_ref(), node_id.as_deref(), raster_installed)
        {
            // The aimed file didn't produce a raster (non-raster
            // preview or decode failure): a viewport aim is
            // meaningless — retire it rather than letting it fire
            // on a later unrelated raster.
            let aim = Some(dropped);
            tracing::warn!(node_id = ?aim.map(|a| a.node_id),
                "preview --roi: target has no raster preview — aim dropped");
        }
        // Modules-mode line anchoring: render_preview_source just
        // reset the scroll to the top; if the selected row gave us
        // a definition line, scroll the item (its docstring if
        // present, else the definition) to the top instead of
        // showing the containing file from line 1. Consume-once,
        // code shapers only (tokens / non-markdown text), and only
        // for the reply matching the request we anchored.
        if let Some(def_line) = self.preview_anchor_line.take() {
            let is_code = mime.starts_with("application/vnd.sot.tokens+json")
                || (mime.starts_with("text/")
                    && mime != "text/markdown"
                    && mime != "text/x-markdown");
            let matches_req =
                node_id.as_deref() == self.preview_requests.fired_target();
            if def_line > 0 && is_code && matches_req {
                self.preview_scroll = self
                    .preview_md
                    .anchor_scroll_for_def_line(def_line as usize);
                self.preview_anchored_to = Some(def_line);
            } else {
                self.preview_anchored_to = None;
            }
        } else {
            self.preview_anchored_to = None;
        }
    }

    pub(crate) fn on_concept_write_done(
        &mut self,
        target: String,
        result: crate::net::transport::ConceptWriteResult,
    ) {
        // Only reconcile when the reply targets the active
        // edit — late replies for an abandoned edit are
        // ignored. Stale-write banner UI lands in a later
        // commit; for v1 we log loudly and trust the backend's
        // refusal (no auto-clobber, no silent overwrite).
        let matches_active = self.editor.edits_target(&target);
        match result {
            crate::net::transport::ConceptWriteResult::Ok { path, written } => {
                tracing::info!(%target, %path, written, "concept.write ok");
                if matches_active {
                    // Snap `original` so dirty-check matches
                    // the new on-disk state — the user can
                    // keep editing without an instant dirty
                    // flag after a save.
                    self.editor.mark_saved();
                }
            }
            crate::net::transport::ConceptWriteResult::Stale => {
                tracing::warn!(%target, "concept.write refused: stale");
                if matches_active {
                    self.editor.mark_stale();
                    self.rebuild_edit_preview();
                }
            }
            crate::net::transport::ConceptWriteResult::Error { code, message } => {
                tracing::error!(%target, %code, %message, "concept.write failed");
            }
        }
    }

    pub(crate) fn on_preview_changed(&mut self, event_host: HostKey, payload: serde_json::Value) {
        // The daemon's file watcher reported a filesystem change
        // (create / modify / remove). On a create or remove the
        // affected directory's listing changed, so live-refresh
        // it in the Files nav tree — otherwise the pane shows a
        // stale listing until a manual re-nav (the reported bug).
        //
        // Acceptance is two-path — workspace-tag match on the
        // carried node_id, else path translation under the KNOWN
        // active root — see `resolve_preview_changed` for the
        // rationale (and the 2026-08-17 live forensics that
        // replaced the path-only scheme). Duplicate copies from
        // overlapping watchers re-fire the same idempotent
        // refresh; cheap.
        //
        // ADR 0042 L2a codex review, item E: `active_ws` /
        // `active_project_root()` below describe active_host's
        // OWN view -- there is no per-host parked preview
        // state to update for a non-active host, so a change
        // reported by any other host has nothing valid to
        // resolve against here. Without this gate a
        // coincidental node_id/path match against the
        // ACTIVE host's tag/root (e.g. two projects both
        // having "src/main.jl") could repaint the visible
        // pane with a non-active host's file content.
        if event_host != *self.selected.host() {
            tracing::debug!(%event_host, active_host = %self.selected.host(),
                "preview.changed from a non-active host — dropped");
            return;
        }
        let event_ws = payload.get("workspace_id").and_then(|v| v.as_str());
        let event_node = payload.get("node_id").and_then(|v| v.as_str());
        let event_path = payload.get("path").and_then(|v| v.as_str());
        let kind = payload.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let active_ws = self
            .selected
            .workspace_id()
            .as_deref()
            .or(self.row_presentation.default_slug());
        let resolved = resolve_preview_changed(
            event_ws,
            event_node,
            event_path,
            active_ws,
            self.active_project_root(),
        );
        // Receipt log — the arm used to skip silently, which
        // made the live-refresh path undiagnosable from the FE
        // log (2026-08-17 forensics). The level splits on the
        // OUTCOME, not on arrival: a resolved event is rare and
        // actionable, an unresolved one is the bulk of a busy
        // host's traffic. The old comment here claimed
        // "debounced daemon-side, so info-level is low-volume";
        // measured on a laptop FE 2026-09-05 that is false —
        // 109 events in a 30 s idle window, 101 of them
        // unresolved, ~2.9 KB/s of formatted disk writes for
        // events that are then discarded. Keeping both outcomes
        // at info made the diagnostic log proportional to the
        // flood it exists to diagnose. Both paths still log
        // every field.
        let Some(node_id) = resolved else {
            // Not ours to render (foreign workspace, or the
            // active root is unknown and the tag didn't match).
            tracing::debug!(
                kind,
                event_ws = ?event_ws,
                path = ?event_path,
                active_ws = ?active_ws,
                "preview.changed dropped — not the active view"
            );
            return;
        };
        tracing::info!(
            kind,
            event_ws = ?event_ws,
            path = ?event_path,
            active_ws = ?active_ws,
            resolved = %node_id,
            "preview.changed received"
        );
        // A file edited after the cursor first reached it must be hashed again (and a rewritten annotation
        // read again), or its STALE mark waits for a window restart.
        self.forget_changed_file(&node_id);
        if kind == "created" || kind == "removed" {
            let parent = parent_files_node_id(&node_id);
            self.refresh_tree_dir_if_expanded(&parent);
        }
        // A change to the file the preview pane is currently
        // showing means its bytes changed underneath us — re-fire
        // `preview.get` so the pane reflects the new content.
        // BOTH kinds matter: an in-place rewrite arrives as
        // "modified", but atomic savers (write temp + rename
        // into place) deliver the SAME logical update as
        // "created" — the old modified-only gate left renamed-in
        // figures stale (the reported same-filename bug).
        // The fired target is the source of truth for
        // "what the pane shows right now" (same anchor the
        // reconnect re-fetch uses); hold the current page so a
        // paginated preview doesn't snap back to page 1.
        if (kind == "modified" || kind == "created")
            && self.preview_requests.fired_target() == Some(node_id.as_str())
        {
            let (fit_w, fit_h) = self.preview_fit_px();
            let generation = self.next_preview_gen();
            let _ = self.send_to(
                &event_host,
                crate::net::transport::OutgoingReq::PreviewGet {
                    node_id: node_id.clone(),
                    workspace_id: self.selected.workspace_id().clone(),
                    page: self.preview_page.map(|(p, _)| p),
                    fit_w,
                    fit_h,
                    generation,
                },
            );
        }
    }

    pub(crate) fn on_pluto_opened(&mut self, event_host: HostKey, result: Result<String, String>) {
        match result {
            Ok(url) => {
                if let Some(url) = self.ensure_proxy_for_url(&event_host, &url, crate::ui::page_proxy::PageSource::Served) {
                    let origin = crate::browser_open::origin_of(&url);
                    if let Err(e) = crate::browser_open::open_page(&url) {
                        tracing::warn!(error = %e, page = %origin,
                                "pluto: browser open failed");
                        self.status = format!("pluto.open browser-launch failed · {e}");
                    } else {
                        self.status = format!("pluto · opened {origin}");
                    }
                }
                self.window.request_redraw();
            }
            Err(msg) => {
                tracing::warn!(error = %msg, "pluto.open failed");
                self.status = format!("pluto.open failed · {msg}");
                self.window.request_redraw();
            }
        }
    }

    /// A `pluto.open` answered that the server is still starting: its start goes on, so this is no failure, and the
    /// next open of a notebook joins it.
    pub(crate) fn on_pluto_starting(&mut self, event_host: HostKey, message: String) {
        tracing::info!(%message, %event_host, "pluto.open: the server is still starting");
        let host = host_label(&self.hosts.declared_host, &event_host);
        self.status = pluto_starting_status(host, &message);
        self.window.request_redraw();
    }

    pub(crate) fn on_docs_opened(&mut self, event_host: HostKey, result: Result<String, String>) {
        match result {
            Ok(url) => {
                if let Some(url) = self.ensure_proxy_for_url(&event_host, &url, crate::ui::page_proxy::PageSource::Served) {
                    let origin = crate::browser_open::origin_of(&url);
                    if let Err(e) = crate::browser_open::open_page(&url) {
                        tracing::warn!(error = %e, page = %origin,
                                "docs: browser open failed");
                        self.status = format!("docs.open browser-launch failed · {e}");
                    } else {
                        self.status = format!("docs · opened {origin}");
                    }
                }
                self.window.request_redraw();
            }
            Err(msg) => {
                tracing::warn!(error = %msg, "docs.open failed");
                self.status = format!("docs.open failed · {msg}");
                self.window.request_redraw();
            }
        }
    }

    pub(crate) fn on_video_opened(&mut self, event_host: HostKey, result: Result<String, String>) {
        match result {
            Ok(url) => {
                if let Some(url) = self.ensure_proxy_for_url(&event_host, &url, crate::ui::page_proxy::PageSource::Served) {
                    let origin = crate::browser_open::origin_of(&url);
                    if let Err(e) = crate::browser_open::open_page(&url) {
                        tracing::warn!(error = %e, page = %origin,
                                "video: browser open failed");
                        self.status = format!("video.open browser-launch failed · {e}");
                    } else {
                        self.status = "video · opened in browser".to_string();
                    }
                }
                self.window.request_redraw();
            }
            Err(msg) => {
                tracing::warn!(error = %msg, "video.open failed");
                self.status = format!("video.open failed · {msg}");
                self.window.request_redraw();
            }
        }
    }

    pub(crate) fn on_quarto_opened(&mut self, result: Result<Vec<u8>, String>) {
        match result {
            Ok(html) => {
                // Backend rendered a self-contained HTML with no
                // backing file of its own (`--embed-resources`
                // inlines every asset) — there's no fs path to
                // route through `docs.open`, so this is the
                // ONE legitimate caller of the temp-byte open
                // left; a sourced `text/html` preview's `o`
                // goes through `docs.open` instead (see
                // `open_path_external`).
                if let Err(e) = open_html_in_browser(&html) {
                    tracing::warn!(error = %e, "quarto: open_html_in_browser failed");
                    self.status = format!("quarto.open browser-launch failed · {e}");
                } else {
                    self.status = "quarto · opened in browser".to_string();
                }
                self.window.request_redraw();
            }
            Err(msg) => {
                tracing::warn!(error = %msg, "quarto.open failed");
                self.status = format!("quarto.open failed · {msg}");
                self.window.request_redraw();
            }
        }
    }

    pub(crate) fn on_preview_get_failed(
        &mut self,
        event_host: HostKey,
        node_id: Option<String>,
        workspace_id: Option<String>,
        generation: u64,
        message: String,
    ) {
        requests::continue_preview_failure(self, event_host, workspace_id, generation, (node_id, message));
    }

    fn install_preview_failure(&mut self, node_id: Option<String>, message: String) {
        // Most commonly `code: "kernel_unavailable"` — a
        // bounded-output-only file type (HDF5/video/PDF) with
        // the Julia kernel unavailable. Same status-line
        // convention as `ScaleSetFailed`/`ImageCropFailed` just
        // above; the preview pane itself is left as whatever it
        // already showed (no blank/stale flash) rather than
        // inventing a new error widget for it.
        let name = node_id
            .as_deref()
            .and_then(|id| id.rsplit(['/', '\\']).next())
            .unwrap_or("preview")
            .to_string();
        self.status = format!("preview failed · {name}: {message}");
        self.window.request_redraw();
    }
}

pub(in crate::ui) struct PreviewReply {
    node_id: Option<String>,
    workspace_id: Option<String>,
    mime: String,
    bytes: Vec<u8>,
    extras: Option<serde_json::Value>,
    generation: u64,
}

impl RequestReplyContext for State {
    type Preview = PreviewReply;
    type Concept = (bool, String);
    type Failure = (Option<String>, String);
    fn requests(&self) -> &PreviewRequests { &self.preview_requests }
    fn active_host(&self) -> &HostKey { self.selected.host() }
    fn active_workspace(&self) -> &Option<String> { self.selected.workspace_id() }
    fn install_preview(&mut self, payload: Self::Preview) { self.install_preview_reply(payload); }
    fn stale_editor_target(&self) -> Option<&str> {
        self.editor.stale_target()
    }
    fn reload_editor(&mut self, (exists, content): &Self::Concept) {
        self.reload_concept_editor(*exists, content);
    }
    fn install_concept(&mut self, target: String, (exists, content): Self::Concept) {
        self.install_concept_read(target, exists, content);
    }
    fn preview_failed(&mut self, (node_id, message): Self::Failure) {
        self.install_preview_failure(node_id, message);
    }
}

/// The status line for a Pluto still starting on `host`: what to do first, so a narrow bar keeps it, then the daemon's
/// account.
fn pluto_starting_status(host: &str, message: &str) -> String {
    format!("pluto · still starting on {host}; open again once it is ready · {message}")
}

#[cfg(test)]
#[path = "replies_pluto_starting_tests.rs"]
mod pluto_starting_tests;
