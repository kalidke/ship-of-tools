//! The image ROI: raster-node test and the crop capture with its applied report.

use crate::ui::*;

impl State {
    /// True if a `files:` node id names a raster image the backend can decode
    /// + crop (ADR 0022). PDFs are excluded — their preview is a rasterized
    /// page, not the `.pdf` file `image.crop` would try to decode.
    pub(in crate::ui) fn is_image_node_id(node_id: &str) -> bool {
        let lower = node_id.to_ascii_lowercase();
        [
            ".png", ".jpg", ".jpeg", ".bmp", ".gif", ".webp", ".tif", ".tiff",
        ]
        .iter()
        .any(|e| lower.ends_with(e))
    }

    /// ADR 0022: capture the current image-preview ROI. Fires `image.crop`
    /// against the active workspace; the `ImageCropped` reply pastes a
    /// "look at this" line into the LLM pane AND moves focus there, so the
    /// user can type context and hit Enter without a pane hop. No-op (with a
    /// status hint) when the preview isn't a croppable image — and no focus
    /// move on any failure path, so a no-op leaves you on the image.
    pub(in crate::ui) fn capture_roi(&mut self) {
        let Some(roi) = self.preview_roi.clone() else {
            self.status = "capture: no image ROI in preview (zoom an image first)".to_string();
            self.window.request_redraw();
            return;
        };
        let name = roi
            .node_id
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(&roi.node_id)
            .to_string();
        tracing::info!(node_id = %roi.node_id, x = roi.x, y = roi.y, w = roi.w, h = roi.h,
            "image.crop requested (capture_roi)");
        // ADR 0042 L2a codex review, item F: pin the requesting host so
        // the ImageCropped/ImageCropFailed reply (which pastes into the
        // LLM pane — a side effect, not just a paint) can confirm it's
        // still answering FOR this host before acting, even if the user
        // switched hosts between the request and the reply landing.
        let roi_host = self.active_host.clone();
        if self
            .send_to(
                &roi_host,
                crate::net::transport::OutgoingReq::ImageCrop {
                    node_id: roi.node_id.clone(),
                    x: roi.x,
                    y: roi.y,
                    w: roi.w,
                    h: roi.h,
                    workspace_id: self.active_workspace_id.clone(),
                },
            )
            .is_err()
        {
            self.status = "capture: transport closed".to_string();
            self.window.request_redraw();
            return;
        }
        self.pending_roi_capture_host = Some(roi_host);
        self.status = format!("capturing ROI {}×{} of {} → LLM…", roi.w, roi.h, name);
        self.window.request_redraw();
    }

    /// ADR 0025 (2026-07-21 update): after a `preview --roi` aim is applied,
    /// echo the *effective* (post-clamp) viewport rect back to the daemon.
    /// `fe.command.send` is fire-and-forget — the daemon acks `{ok:true}`
    /// before any FE acts — so the effective rect can't ride that ack; it
    /// rides the FE→daemon notification channel that already exists instead:
    /// the `agent.send` relay, which the daemon re-broadcasts to every
    /// connection as an `agent.message` evt (a `sot-fe … --await-roi`
    /// consumer watches that). `clamped` is true when the requested rect is
    /// not fully inside the effective one (beyond a small rounding
    /// tolerance): the aim hit the zoom ceiling or ran off the image, and the
    /// caller may want to re-aim.
    pub(in crate::ui) fn emit_preview_roi_applied(&self, aim: &RoiAim, eff: &PreviewRoi) {
        if self.active_result_row_key().as_ref() != Some(&aim.row_key) {
            return;
        }
        // `visible_roi_px`'s floor/ceil quantization can nibble an edge pixel;
        // don't call that a clamp.
        const TOL: u32 = 2;
        let req = aim.rect;
        let contained = eff.x <= req.x.saturating_add(TOL)
            && eff.y <= req.y.saturating_add(TOL)
            && eff.x.saturating_add(eff.w).saturating_add(TOL) >= req.x.saturating_add(req.w)
            && eff.y.saturating_add(eff.h).saturating_add(TOL) >= req.y.saturating_add(req.h);
        let payload = serde_json::json!({
            "evt": "preview_roi_applied",
            "ws": aim.workspace,
            "path": aim.path,
            "requested": { "x": req.x, "y": req.y, "w": req.w, "h": req.h },
            "effective": {
                "x": eff.x, "y": eff.y, "w": eff.w, "h": eff.h,
                "src_w": eff.src_w, "src_h": eff.src_h,
            },
            "clamped": !contained,
        });
        tracing::info!(ws = %aim.workspace, path = %aim.path, clamped = !contained,
            x = eff.x, y = eff.y, w = eff.w, h = eff.h,
            "preview --roi applied — echoing effective rect");
        if let Err(e) = self.send(crate::net::transport::OutgoingReq::AgentSend {
            from: self_comm_handle(),
            to: String::new(),
            text: payload.to_string(),
        }) {
            tracing::warn!(error = %e, "preview_roi_applied: transport closed — echo dropped");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_image_node_id_matches_rasters_not_pdf() {
        assert!(State::is_image_node_id("files:plots/a.png"));
        assert!(State::is_image_node_id("files:IMG.JPEG"));
        assert!(!State::is_image_node_id("files:doc.pdf"));
        assert!(!State::is_image_node_id("files:src/lib.jl"));
    }
}
