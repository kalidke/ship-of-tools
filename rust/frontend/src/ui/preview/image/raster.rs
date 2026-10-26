//! The raster view: the shown raster's texture and dimensions, its zoom and pan, the visible source-pixel ROI, and
//! the same-directory view carry.

use crate::ui::*;

const ZOOM_STEP: f32 = 1.25;

/// The image pane's view of one raster. Installing a raster resets the view to fit (unless a page re-raster keeps
/// it) and parks a carried view for the render pass to solve; the render pass frames the canvas, records the
/// visible ROI and writes it through to the carry.
pub(in crate::ui) struct RasterView {
    /// The shown raster's texture; `None` when the preview is not a raster.
    quad: Option<Quad>,
    /// Zoom multiplier for the PNG preview pane — 1.0 fits the image to
    /// the pane (current letterbox behaviour); >1.0 samples a 1/zoom-wide
    /// sub-rect of the texture so detail becomes legible.
    zoom: f32,
    /// Pan offset in physical pixels of the zoomed canvas relative to
    /// the pane centre. `(0, 0)` keeps the canvas centred in
    /// `preview_rect`; arrow keys nudge by a fraction of the pane.
    /// Clamped at render time so the canvas covers the pane in whichever
    /// axes it's larger than the pane — the user can't pan past the
    /// image edge and reveal empty pane.
    pan_px: (f32, f32),
    /// Natural dimensions (w, h) of the currently-loaded PNG, used as
    /// part of the cache key so flipping between same-size renders in
    /// the same directory (e.g. successive timesteps of a plot) keeps
    /// the same view (see `view_cache`). `None` until a PNG has
    /// loaded.
    dims: Option<(u32, u32)>,
    /// Dimensions of the shown raster as SERVED, before the GPU-fit downsample
    /// that `decode_and_fit` applies to images exceeding
    /// `max_texture_dimension_2d`. Equals `dims` for everything
    /// under the cap. The ADR-0034 scalebar keys off THIS: the wire's
    /// `nm_per_px` describes served pixels, so measuring against the shrunken
    /// texture width mis-scales the bar by `src/texture` (a 20000 px raster
    /// capped to 16384 reads ~1.22x short). Re-derived on every decode, so it
    /// stays correct across workspace-snapshot repaints.
    src_dims: Option<(u32, u32)>,
    /// The visible region of the current image preview, in *source-image
    /// pixel* coords (ADR 0022). Recomputed each draw from zoom/pan/letterbox
    /// when an image is shown; `None` when the preview isn't a croppable
    /// image. Consumed by `capture_roi` (the `C` hotkey / `capture_roi`
    /// fe-command) and surfaced in `fe-state.json` so the LLM pane knows what
    /// the user is zoomed into.
    roi: Option<PreviewRoi>,
    /// Per-directory + per-dimensions cache of the last visible SOURCE-PX
    /// ROI, so switching among same-sized image renders in one directory
    /// restores the previous view rather than snapping back to fit.
    /// Mixed-size images miss the cache and start at fit-to-pane.
    ///
    /// Source px, not `(zoom, pan_px)`: screen-px values are only valid
    /// against the image rect they were measured in, and that rect moves —
    /// the caption band resizes it per file (and the pane itself can resize
    /// between save and restore). A screen-px carry between a captioned and
    /// an uncaptioned same-size neighbor restored an offset, wrongly-
    /// magnified view. An ROI re-solved against the live rect at consume
    /// time (`pending_restore`) is geometry-independent by construction.
    /// Written through from the render pass each draw — a keystroke-time
    /// save recorded the previous frame's view — and only for image node
    /// ids (via `roi`'s gate), which also stops PDF page turns
    /// (same node id ⇒ same key) from restoring one page's view onto
    /// another when a page turn is specified to reset to fit.
    view_cache: HashMap<(String, (u32, u32)), RoiRect>,
    /// One-shot deferred view restore: a cache hit at preview install can't
    /// solve immediately — there's no live geometry there, and
    /// `caption_band_px` at install time still describes the PREVIOUS
    /// node's band — so the hit is parked as `(node_id, roi)` and consumed
    /// in the render pass next to the `--roi` aim solve, against the
    /// current `image_rect`. An aim consumed for the same install wins over
    /// the carry, and a carry restore never emits the ADR-0025
    /// `preview_roi_applied` echo (that event is the CLI contract for
    /// explicit aims). Node-guarded and overwritten on every raster
    /// install, so a stale entry is inert.
    pending_restore: Option<(String, RoiRect)>,
}

/// The canvas one frame draws: where the zoomed image sits, at which zoom, and a `--roi` aim the frame applied.
pub(in crate::ui) struct CanvasFrame {
    pub(in crate::ui) rect: ScreenRect,
    pub(in crate::ui) zoom: f32,
    /// The raster's source pixels (`dims`, else the texture's).
    pub(in crate::ui) src: (u32, u32),
    pub(in crate::ui) aim: Option<RoiAim>,
}

impl RasterView {
    /// The startup splash, or nothing.
    pub(in crate::ui) fn new(quad: Option<Quad>) -> Self {
        Self {
            quad,
            zoom: 1.0,
            pan_px: (0.0, 0.0),
            dims: None,
            src_dims: None,
            roi: None,
            view_cache: HashMap::new(),
            pending_restore: None,
        }
    }

    pub(in crate::ui) fn quad(&self) -> Option<&Quad> { self.quad.as_ref() }
    pub(in crate::ui) fn is_shown(&self) -> bool { self.quad.is_some() }
    pub(in crate::ui) fn quad_size(&self) -> Option<(u32, u32)> { self.quad.as_ref().map(|q| q.size_px) }
    pub(in crate::ui) fn zoom(&self) -> f32 { self.zoom }
    pub(in crate::ui) fn roi(&self) -> Option<&PreviewRoi> { self.roi.as_ref() }

    /// The raster's dimensions as served (before any GPU-fit downsample), else as loaded.
    pub(in crate::ui) fn served_dims(&self) -> Option<(u32, u32)> { self.src_dims.or(self.dims) }

    /// A decoded raster replaces the shown one, with its texture and served dimensions.
    pub(in crate::ui) fn install(&mut self, quad: Quad, src_dims: (u32, u32)) {
        self.dims = Some(quad.size_px);
        self.quad = Some(quad);
        self.src_dims = Some(src_dims);
    }

    /// A new raster's view starts at fit. For an image node (`carry_for`) a cached view of a same-size neighbor in
    /// its directory is parked for the render pass to solve against the live geometry.
    pub(in crate::ui) fn reset_view(&mut self, carry_for: Option<&str>) {
        self.pending_restore = carry_for.and_then(|nid| {
            let key = png_cache_key_from_node_id(Some(nid), self.dims?)?;
            let roi = *self.view_cache.get(&key)?;
            Some((nid.to_string(), roi))
        });
        self.zoom = 1.0;
        self.pan_px = (0.0, 0.0);
    }

    /// The preview is no longer a raster.
    pub(in crate::ui) fn hide(&mut self) { self.quad = None; }

    pub(in crate::ui) fn reset_zoom(&mut self) {
        self.zoom = 1.0;
        self.pan_px = (0.0, 0.0);
    }

    pub(in crate::ui) fn pan_x(&mut self, d: f32) { self.pan_px.0 += d; }
    pub(in crate::ui) fn pan_y(&mut self, d: f32) { self.pan_px.1 += d; }

    pub(in crate::ui) fn zoom_in(&mut self, zoom_max: f32) {
        // Zoom sequence: 1.0 → 1.25 → 2 → 3 → 4 → …
        // up to the per-image ceiling (`zoom_max`).
        // Once we're past 1.5×, step in integer
        // multiples of fit so increments stay
        // predictable and avoid the moiré beating of
        // fractional zoom against the nearest-
        // neighbour sampler grid — which keeps dense
        // scientific rasters reading crisply per-pixel
        // (user ask 2026-05-22). The final value is
        // clamped to the ceiling, so the last step may
        // land on a fractional zoom that puts a source
        // pixel at exactly 16 screen px.
        let cur = self.zoom;
        let raw_next = if cur < 1.5 {
            let raw = cur * ZOOM_STEP;
            if raw >= 1.5 {
                2.0
            } else {
                raw
            }
        } else {
            cur + 1.0
        };
        let next = raw_next.clamp(1.0, zoom_max);
        self.scale_pan_for_zoom(cur, next);
        self.zoom = next;
    }

    pub(in crate::ui) fn zoom_out(&mut self) {
        // Mirror of zoom-in: integer-step down
        // from ≥ 2, then drop back through 1.25
        // → 1.0. Hitting 2.0 → 1.25 is the
        // discrete jump out of integer mode so
        // the user lands cleanly on the
        // multiplicative step below 1.5×.
        let cur = self.zoom;
        let next = if cur > 1.5 {
            let raw = cur - 1.0;
            if raw < 2.0 {
                1.25
            } else {
                raw
            }
        } else {
            (cur / ZOOM_STEP).max(1.0)
        };
        self.scale_pan_for_zoom(cur, next);
        self.zoom = next;
        if self.zoom <= 1.0 {
            self.pan_px = (0.0, 0.0);
        }
    }

    /// Scale the pan offset when the zoom changes so the point at the
    /// centre of the field of view stays put. `pan_px` is measured in
    /// zoomed-canvas pixels (canvas = letterbox × zoom), so the image
    /// fraction off-centre is `pan / (letterbox × zoom)`. Holding that
    /// fraction fixed across a zoom change means pan scales with the zoom
    /// ratio — without this, zooming while panned drifts the view off the
    /// point you were looking at. The render path re-clamps pan to the new
    /// slack, so over-scrolled values self-correct.
    fn scale_pan_for_zoom(&mut self, cur: f32, next: f32) {
        if cur <= 0.0 {
            return;
        }
        let ratio = next / cur;
        self.pan_px.0 *= ratio;
        self.pan_px.1 *= ratio;
    }

    /// ADR 0022: recomputed by the frame when an image is shown; cleared each
    /// frame so a switch to markdown / no-preview drops the stale ROI.
    pub(in crate::ui) fn clear_roi(&mut self) { self.roi = None; }

    /// Frame the canvas for this draw (`None` with no raster). PNG uses a canvas
    /// model — at zoom 1 the canvas equals letterbox (image inside
    /// pane, aspect preserved); at zoom > 1 the canvas grows by
    /// `zoom` and is rendered at full size with a scissor clip to
    /// the pane, so the zoomed-in view fills the whole pane.
    ///
    /// A ready `--roi` aim overrides zoom and pan; without one, a parked view carry for the fired target does.
    /// Zoom and pan are then clamped to the live geometry.
    pub(in crate::ui) fn frame(
        &mut self, image_rect: ScreenRect, letterbox_rect: ScreenRect, aim: Option<RoiAim>, fired: Option<&str>,
    ) -> Option<CanvasFrame> {
        let size_px = self.quad.as_ref()?.size_px;
        // Re-clamp against the live pane size before sizing the
        // canvas: a pane resize or a zoom restored from the
        // view-state cache can sit above the per-pixel ceiling for
        // the current geometry, and the canvas must honour it. Same
        // `(image_rect, size_px)` inputs as the `letterbox` —
        // they must stay in lockstep, caption band included, or at the
        // ceiling canvas_w no longer equals 16 × source-px exactly.
        let zoom_max = png_zoom_max(image_rect.w, image_rect.h, size_px);
        let (src_w, src_h) = self.dims.unwrap_or(size_px);
        // ADR 0025 `preview --roi`: a pending viewport aim whose image is
        // the installed quad (ready — certified at preview install) AND
        // still the fired preview target. The solve overrides zoom/pan
        // here, where the live pane geometry exists; the ordinary clamps
        // just below then produce the effective rect echoed back after
        // the ROI recomputes.
        let mut roi_aim: Option<RoiAim> = None;
        if let Some(aim) = aim {
            match solve_roi_view(
                image_rect.w,
                image_rect.h,
                letterbox_rect.w,
                letterbox_rect.h,
                zoom_max,
                src_w,
                src_h,
                aim.rect,
            ) {
                Some((z, pan)) => {
                    self.zoom = z;
                    self.pan_px = pan;
                    roi_aim = Some(aim);
                }
                None => tracing::warn!(node_id = %aim.node_id,
                    "preview --roi: degenerate geometry — aim dropped"),
            }
        }
        if roi_aim.is_some() {
            // An explicit `--roi` aim beats the view carry: both
            // target this install, and the aim is a user/CLI ask.
            self.pending_restore = None;
        } else if self.pending_restore.as_ref().is_some_and(|(nid, _)| Some(nid.as_str()) == fired) {
            // Same-dir same-size view carry (`view_cache`),
            // deferred from preview install to here — the first
            // frame with this node's OWN caption band in
            // `image_rect`. Solved exactly like an aim; the
            // ordinary clamps below still apply. Deliberately no
            // `preview_roi_applied` echo: that event is the
            // ADR-0025 contract for explicit aims only.
            let (nid, rect) = self.pending_restore.take().expect("checked Some above");
            match solve_roi_view(
                image_rect.w,
                image_rect.h,
                letterbox_rect.w,
                letterbox_rect.h,
                zoom_max,
                src_w,
                src_h,
                rect,
            ) {
                Some((z, pan)) => {
                    self.zoom = z;
                    self.pan_px = pan;
                }
                // Degraded, not broken — the view stays at fit. Logged
                // (debug, not the aim path's warn: no user asked for
                // this rect) so a mysteriously-not-carried view is
                // diagnosable.
                None => tracing::debug!(node_id = %nid,
                    "view carry: degenerate geometry — restore dropped"),
            }
        }
        let zoom = self.zoom.clamp(1.0, zoom_max);
        self.zoom = zoom;
        let canvas_w = letterbox_rect.w * zoom;
        let canvas_h = letterbox_rect.h * zoom;
        let pane_cx = image_rect.x + image_rect.w * 0.5;
        let pane_cy = image_rect.y + image_rect.h * 0.5;
        // Clamp pan so canvas always covers the pane in any axis
        // where canvas > pane. When canvas < pane (e.g. at zoom
        // 1 with a non-pane-aspect image), pan in that axis is
        // forced to 0 so the letterbox stays centred.
        let slack_x = (canvas_w - image_rect.w).max(0.0);
        let slack_y = (canvas_h - image_rect.h).max(0.0);
        let pan_x = self.pan_px.0.clamp(-slack_x * 0.5, slack_x * 0.5);
        let pan_y = self.pan_px.1.clamp(-slack_y * 0.5, slack_y * 0.5);
        self.pan_px = (pan_x, pan_y);
        let rect = ScreenRect {
            x: pane_cx - canvas_w * 0.5 + pan_x,
            y: pane_cy - canvas_h * 0.5 + pan_y,
            w: canvas_w,
            h: canvas_h,
        };
        Some(CanvasFrame { rect, zoom, src: (src_w, src_h), aim: roi_aim })
    }

    /// The frame's visible ROI, written through to the view carry so
    /// same-dir same-size neighbors restore this view. Done here, not at
    /// keystroke time, because only the render pass has the post-clamp
    /// geometry. The hysteresis matters: a restore's own readback lands
    /// within quantization (±1 px/edge) of the rect it restored,
    /// and overwriting with it would ratchet — each flip
    /// re-fitting a rect one pixel bigger, the view creeping out.
    /// Keeping the incumbent inside that window makes the carry a
    /// true fixed point; real zoom/pan input moves edges by far
    /// more than a pixel, so nothing a user does is swallowed.
    /// The ROI is image-node-gated, so PDF pages never save.
    pub(in crate::ui) fn record_roi(&mut self, roi: Option<PreviewRoi>) {
        self.roi = roi;
        if let Some(roi) = self.roi.as_ref() {
            if let Some(key) = png_cache_key_from_node_id(
                Some(roi.node_id.as_str()),
                (roi.src_w, roi.src_h),
            ) {
                let rect = RoiRect {
                    x: roi.x,
                    y: roi.y,
                    w: roi.w,
                    h: roi.h,
                };
                match self.view_cache.get(&key) {
                    Some(prev) if roi_rects_within_quantization(*prev, rect) => {}
                    _ => {
                        self.view_cache.insert(key, rect);
                    }
                }
            }
        }
    }
}
