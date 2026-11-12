//! Image replies: markdown figure bytes and their failures, the ROI crop pasted to the agent pane,
//! crop failures, a rejected pixel-size save.

use super::figure_store::{Reply, Settled};
use super::figures::{FigureCacheEntry, FIGURE_SVG_SIZE};
use super::svg_preview::settle_frame;
use crate::ui::*;

impl State {
    pub(crate) fn on_figure_loaded(&mut self, url: String, mime: String, bytes: Vec<u8>) {
        // Decode the bytes into a Quad sized to the
        // bitmap's natural pixel dimensions, then drop it
        // into figure_cache keyed by the original markdown
        // URL. A reflow request forces a one-shot walk
        // before the next paint so the placeholder's
        // reserved height tracks the figure's actual aspect
        // — without it the FFFC stays at the
        // FIGURE_BLOCK_H_DEFAULT fallback even after the
        // bytes land.
        // An SVG is drawn by the worker, never on this thread: its URL stays pending until its outcome
        // (`on_svg_settled`); anything else comes back here to decode.
        let generation = self.preview_source.generation();
        let (url, bytes) = match self.figures.route_reply(&mut self.svg_draws, generation, url, &mime, bytes) {
            Reply::Raster { url, bytes } => (url, bytes),
            Reply::Submitted => return,
            Reply::Failed => {
                self.md_media.request_reflow();
                self.window.request_redraw();
                return;
            }
        };
        match decode_figure_bytes(
            self.gpu.device(),
            self.gpu.queue(),
            self.brushes.pipeline(),
            &mime,
            &bytes,
        ) {
            Ok(entry) => {
                tracing::info!(
                    %url,
                    %mime,
                    w = entry.natural_w_px,
                    h = entry.natural_h_px,
                    "figure decoded"
                );
                self.figures.install(url, entry);
                self.md_media.request_reflow();
                self.window.request_redraw();
            }
            Err(e) => {
                tracing::warn!(%url, %mime, error = %e, "figure decode failed");
                // Terminal: collapse the reservation to the
                // compact fallback on the next reflow rather
                // than leaving an empty box that will never
                // be painted over.
                self.figures.fail(url);
                self.md_media.request_reflow();
                self.window.request_redraw();
            }
        }
    }

    /// The top of a frame's step for the worker's outcomes (`svg_preview::settle_frame`): the worker is told the pane's
    /// generation, a figure's outcome goes to `on_svg_settled` and the file preview's to `apply_svg_step`.
    pub(in crate::ui) fn settle_svg_draws(&mut self) {
        let generation = self.preview_source.generation();
        let frame = settle_frame(&mut self.svg_draws, &mut self.figures, &mut self.svg_preview, generation);
        for (url, settled) in frame.figures {
            self.on_svg_settled(url, settled);
        }
        if let Some(step) = frame.preview {
            self.apply_svg_step(step);
        }
    }

    /// A figure's SVG settled: a picture is uploaded and installed,
    /// a refusal has failed the figure, a stale job left it unhandled so the next walk fetches it again. One reflow
    /// follows each.
    pub(in crate::ui) fn on_svg_settled(&mut self, url: String, settled: Settled) {
        match settled {
            Settled::Ignored => return,
            Settled::Drawn { w, h, rgba } => {
                let quad = Quad::from_rgba8(self.gpu.device(), self.gpu.queue(), self.brushes.pipeline(), &rgba, w, h);
                match quad {
                    Ok(quad) => {
                        let (natural_w_px, natural_h_px) = FIGURE_SVG_SIZE;
                        self.figures.install(url, FigureCacheEntry { quad, natural_w_px, natural_h_px });
                    }
                    Err(e) => {
                        tracing::warn!(%url, error = %e, "figure svg upload failed");
                        self.figures.fail(url);
                    }
                }
            }
            Settled::Failed(why) => tracing::warn!(%url, why, "figure svg not drawn"),
            Settled::Unhandled => {}
        }
        self.md_media.request_reflow();
        self.window.request_redraw();
    }

    pub(crate) fn on_figure_get_failed(&mut self, url: String) {
        // `figure.get` answered with an `{error, code}`
        // envelope or failed to parse — the bytes never
        // arrived at all (field report: this used to
        // warn-and-drop with no event, leaving `url` stuck
        // in `figure_pending` forever since
        // `dispatch_pending_figures` never refires anything
        // already pending). Same terminal collapse as a
        // decode failure above.
        tracing::warn!(%url, "figure.get failed — collapsing to compact fallback");
        self.figures.fail(url);
        self.md_media.request_reflow();
        self.window.request_redraw();
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn on_image_cropped(
        &mut self,
        event_host: HostKey,
        node_id: String,
        path: String,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        src_w: u32,
        src_h: u32,
    ) {
        // ADR 0042 L2a codex review, item F: only the host
        // capture_roi actually pinned may drive this reply's
        // side effect (the LLM-pane paste + focus move) — a
        // stray/late reply from a non-owning host (or one
        // that arrives after a second capture_roi already
        // consumed the pin) is dropped.
        if !self.preview_requests.settle_crop(&event_host) {
            tracing::debug!(%event_host, %node_id,
                "image.cropped from a non-owning/stale host — dropped");
            return;
        }
        tracing::info!(%node_id, %path, w, h, "image.cropped received → pasting to LLM pane");
        // ADR 0022: paste a ready-to-send "look at this" line into
        // the LLM pane (BL pty). No trailing Enter — the user can
        // add context and submit, so we never fire a half-formed
        // prompt or clobber partial input in a shared pane. The
        // message names the *full source path* (provenance) and the
        // crop path; Claude Code auto-attaches the crop image from
        // its path, so the in-pane agent sees the actual pixels.
        let name = node_id
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(&node_id)
            .to_string();
        let src_path = self.backend_abs_path(&node_id);
        let msg = format!(
            "Look at this cropped region of {src_path} ({src_w}×{src_h} source) — \
             ROI x={x} y={y}, {w}×{h} px. Cropped PNG: {path}"
        );
        let bytes = bracketed_paste_bytes(&msg);
        // Focus follows the paste: the capture key's whole
        // point is to ask the agent about what you're looking
        // at, and the pasted line is deliberately unsent (no
        // trailing Enter) so you can add context first. Landing
        // focus here removes the Ctrl+Arrow hop that every
        // capture used to require. Moved on the REPLY, not on
        // the keypress, so a crop that fails leaves focus in
        // the Preview pane where the image still is — see the
        // ImageCropFailed arm, which deliberately does not move
        // focus.
        // A hidden LLM pane can't show the paste it was just
        // handed — drop wide-preview so the pane (and the
        // focus move) are actually visible.
        let reveal = self.layout_help.reveal_agent();
        // Focus moves leave an open quit question intact; its next key
        // still answers the dialog before reaching the agent pane.
        // ADR 0042 slice L1b fix 3: routed through the ONE
        // session-pane input dispatcher, exactly like
        // `forward_clipboard_paste_to_llm` — a live capsule
        // gets `send_input` on its own connection, a pending
        // resolution buffers, and only a confirmed tmux row
        // reaches the daemon's `pty.write`. Before this fix
        // the ROI paste always went straight to `pty.write`,
        // landing in whatever tmux pty the daemon still had
        // open even while a capsule was live and selected.
        layout_help::finish_agent_reveal(reveal, self, Some(&bytes));
        self.status.set(format!("ROI {w}×{h} of {name} → LLM pane · Enter to send"));
        self.window.request_redraw();
    }

    pub(crate) fn on_image_crop_failed(
        &mut self,
        event_host: HostKey,
        node_id: String,
        message: String,
    ) {
        // Same owner check as ImageCropped above -- a failure
        // from a non-owning/stale host must not clobber the
        // status line for whatever the user is doing now.
        if !self.preview_requests.settle_crop(&event_host) {
            tracing::debug!(%event_host, %node_id,
                "image.crop failure from a non-owning/stale host — dropped");
            return;
        }
        let name = node_id
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(&node_id)
            .to_string();
        self.status.set(format!("capture failed · {name}: {message}"));
        self.window.request_redraw();
    }

    pub(crate) fn on_scale_set_failed(&mut self, node_id: String, message: String) {
        // ADR 0034 live entry rejected (not_a_raster / bad_scale /
        // path_escape / io_error / …). Surface it so the prompt's
        // "saving…" resolves; the calibration simply isn't applied,
        // and the user can re-open the prompt with Ctrl+S and retry.
        let name = node_id
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(&node_id)
            .to_string();
        // The bar never appeared, so don't leave the overlay armed
        // claiming a scale we don't have.
        self.overlays.disable_scalebar();
        // This save resolved (as a failure), so retire the pending
        // marker — otherwise the next unrelated preview would
        // consume it and report a "saved" that never happened.
        self.preview_requests.fail_scale_save();
        self.status.set(format!("pixel size failed · {name}: {message}"));
        self.window.request_redraw();
    }
}
