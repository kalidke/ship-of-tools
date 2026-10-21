//! Image view geometry: letterbox, zoom bounds, pan and ROI mapping to source pixels.

use crate::ui::*;

/// The visible region of an image preview in source-image pixel coords
/// (ADR 0022). Recomputed each draw; drives `capture_roi` + the `fe-state.json`
/// `preview` block. `path` is the source image's absolute path on the backend
/// (for LLM awareness); the crop itself is produced server-side by `image.crop`.
#[derive(Clone, Debug)]
pub(in crate::ui) struct PreviewRoi {
    pub(in crate::ui) node_id: String,
    pub(in crate::ui) path: String,
    pub(in crate::ui) x: u32,
    pub(in crate::ui) y: u32,
    pub(in crate::ui) w: u32,
    pub(in crate::ui) h: u32,
    pub(in crate::ui) src_w: u32,
    pub(in crate::ui) src_h: u32,
    pub(in crate::ui) zoom: f32,
}

/// A requested `preview --roi` rect in SOURCE-image pixels (ADR 0025,
/// 2026-07-21 update) — the same vocabulary as ADR-0022's `image.crop`, so a
/// crop taken now round-trips to "look here again" later (identical rect ⇒
/// identical region on any display, independent of DPI or pane size).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
pub(in crate::ui) struct RoiRect {
    pub(in crate::ui) x: u32,
    pub(in crate::ui) y: u32,
    pub(in crate::ui) w: u32,
    pub(in crate::ui) h: u32,
}

/// An armed `preview --roi` viewport aim (ADR 0025, 2026-07-21 update).
/// Consumed by the render pass — where the live pane geometry exists — once
/// the aimed image is the *installed* preview quad: `ready` is certified at
/// preview-reply install, because a `preview.get` reply installs whatever
/// arrived last (node-unchecked) and the solve must never run against the
/// previous file's quad. Rides the same badge-floor routing as the carrying
/// `preview`: a cross-workspace aim fires at badge-consume, when the user
/// switches over and the preview renders. One slot, latest-wins.
#[derive(Debug, Clone)]
pub(in crate::ui) struct RoiAim {
    /// The resolved (host, listed workspace slug) of the carrying result.
    pub(in crate::ui) row_key: WsKey,
    pub(in crate::ui) workspace: String,
    pub(in crate::ui) path: String,
    /// `files:<path>` — the node id the aimed preview fires under.
    pub(in crate::ui) node_id: String,
    pub(in crate::ui) rect: RoiRect,
    pub(in crate::ui) ready: bool,
}

/// The rect an image is letterboxed into: the preview pane minus a reserved
/// caption band of `caption_h` at its bottom (0.0 when there's no caption).
///
/// Every consumer of image geometry — `letterbox`, `png_zoom_max`, the pan-slack
/// clamp, canvas centring, `solve_roi_view` and its `visible_roi_px` inverse,
/// and the scissor — must be handed THIS rect, not the pane rect. A consumer
/// left on the pane rect doesn't merely misdraw: because the ROI vocabulary is
/// source-image px solved against the on-screen mapping, a disagreement here
/// skews the `--roi` round-trip (ADR 0022/0025), which is silent and only shows
/// up as an aim that lands slightly wrong.
pub(in crate::ui) fn image_rect_for_caption(preview_rect: ScreenRect, caption_h: f32) -> ScreenRect {
    ScreenRect {
        h: (preview_rect.h - caption_h.max(0.0)).max(1.0),
        ..preview_rect
    }
}

/// The image area of a preview pane given its size in CELLS — the form the
/// keyboard zoom/pan handler has, since it runs outside the render pass and has
/// no `preview_rect`. Origin-free (`x`/`y` are 0): callers that need placement
/// add it; the keyboard path only needs the extent.
///
/// This exists so the handler's pane derivation is *reachable from a test*.
/// Testing `image_rect_for_caption` alone pinned only the premise of the
/// caption-band bug (that the band moves the zoom ceiling) — a revert of the
/// handler back to the raw pane would still have passed green, because nothing
/// exercised the handler's own arithmetic. With the derivation extracted here,
/// removing the band subtraction fails `keyboard_pane_derivation_subtracts_the_caption_band`.
pub(in crate::ui) fn preview_image_pane_px(
    pane_cells: (u16, u16),
    cell_w: f32,
    cell_h: f32,
    caption_band_px: f32,
) -> ScreenRect {
    image_rect_for_caption(
        ScreenRect {
            x: 0.0,
            y: 0.0,
            w: pane_cells.0 as f32 * cell_w,
            h: pane_cells.1 as f32 * cell_h,
        },
        caption_band_px,
    )
}

/// Cache key for the PNG preview view-state cache. Takes a `files:<rel>`
/// node id and a `(w, h)` and returns the parent-dir portion of the id
/// (everything up to and including the last `/`) paired with the dims.
/// `None` if the node id is missing or doesn't fit the expected shape,
/// in which case the caller falls back to fit-to-pane defaults.
pub(in crate::ui) fn png_cache_key_from_node_id(
    node_id: Option<&str>,
    dims: (u32, u32),
) -> Option<(String, (u32, u32))> {
    let id = node_id?;
    // The `files:` prefix is opaque to us; we just want the directory
    // portion so two PNGs in the same dir share a key. Anything else
    // (no `/`) means the file is at the project root — use the empty
    // prefix string, which is still a valid HashMap key.
    let parent = id.rfind('/').map(|i| &id[..=i]).unwrap_or("");
    Some((parent.to_string(), dims))
}

/// True when two source-px ROI rects differ by at most one pixel on each
/// EDGE (x0/y0/x1/y1) — the outward floor/ceil quantization width of
/// `visible_roi_px`. The write-through view carry treats such rects as the
/// same view; see its comment for the ratchet this prevents.
pub(in crate::ui) fn roi_rects_within_quantization(a: RoiRect, b: RoiRect) -> bool {
    fn close(p: u32, q: u32) -> bool {
        p.abs_diff(q) <= 1
    }
    close(a.x, b.x) && close(a.y, b.y) && close(a.x + a.w, b.x + b.w) && close(a.y + a.h, b.y + b.h)
}

/// Letterbox an image of `(iw, ih)` into `outer`, preserving aspect ratio.
pub(in crate::ui) fn letterbox(outer: ScreenRect, img_px: (u32, u32)) -> ScreenRect {
    let (iw, ih) = (img_px.0.max(1) as f32, img_px.1.max(1) as f32);
    let img_aspect = iw / ih;
    let outer_aspect = outer.w / outer.h.max(1.0);
    let (w, h) = if img_aspect > outer_aspect {
        (outer.w, outer.w / img_aspect)
    } else {
        (outer.h * img_aspect, outer.h)
    };
    ScreenRect {
        x: outer.x + (outer.w - w) * 0.5,
        y: outer.y + (outer.h - h) * 0.5,
        w,
        h,
    }
}

/// On-screen size ceiling for a single source pixel, in screen pixels per
/// axis. PNG-preview zoom is capped so one source pixel never grows past
/// 16×16 screen px — large enough to inspect individual cells of a dense
/// scientific raster, small enough that an already-magnified tiny image
/// can't blow up without bound. This replaced a fixed `32×`-fit cap: the
/// meaningful limit is pixel magnification, not a multiple of fit-to-pane
/// (user ask 2026-05-29). See `png_zoom_max`.
const MAX_PX_PER_SRC_PX: f32 = 16.0;

/// Map the visible region of a zoomed/panned image preview to a
/// source-image-pixel ROI (ADR 0022). The full image is drawn into
/// `canvas` (top-left `canvas_x,canvas_y`, size `canvas_w×canvas_h`); the
/// visible window is `pane` (the scissor). The visible canvas∩pane rectangle,
/// as fractions of the canvas, maps directly to the same fractions of the
/// source — so this is independent of any decode-time downsample (the caller
/// passes native `src_w,src_h`). Returns `(x, y, w, h)` in source px, clamped
/// to the image, or `None` if nothing is visible / inputs are degenerate.
#[allow(clippy::too_many_arguments)]
pub(in crate::ui) fn visible_roi_px(
    canvas_x: f32,
    canvas_y: f32,
    canvas_w: f32,
    canvas_h: f32,
    pane_x: f32,
    pane_y: f32,
    pane_w: f32,
    pane_h: f32,
    src_w: u32,
    src_h: u32,
) -> Option<(u32, u32, u32, u32)> {
    if src_w == 0 || src_h == 0 || canvas_w <= 0.0 || canvas_h <= 0.0 {
        return None;
    }
    let vx0 = canvas_x.max(pane_x);
    let vy0 = canvas_y.max(pane_y);
    let vx1 = (canvas_x + canvas_w).min(pane_x + pane_w);
    let vy1 = (canvas_y + canvas_h).min(pane_y + pane_h);
    if vx1 <= vx0 || vy1 <= vy0 {
        return None;
    }
    let fx0 = ((vx0 - canvas_x) / canvas_w).clamp(0.0, 1.0);
    let fy0 = ((vy0 - canvas_y) / canvas_h).clamp(0.0, 1.0);
    let fx1 = ((vx1 - canvas_x) / canvas_w).clamp(0.0, 1.0);
    let fy1 = ((vy1 - canvas_y) / canvas_h).clamp(0.0, 1.0);
    let x = (fx0 * src_w as f32).floor() as u32;
    let y = (fy0 * src_h as f32).floor() as u32;
    let w = (((fx1 - fx0) * src_w as f32).ceil() as u32).clamp(1, src_w - x);
    let h = (((fy1 - fy0) * src_h as f32).ceil() as u32).clamp(1, src_h - y);
    Some((x, y, w, h))
}

/// Upper zoom bound for a native-`img_px` PNG shown fitted into a
/// `pane_w × pane_h` (physical-pixel) pane. Zoom multiplies the letterbox
/// (fit-to-pane) scale, which preserves aspect — so at zoom 1 a source pixel
/// already spans `fit = min(pane_w/iw, pane_h/ih)` screen px, and the 16-px
/// ceiling is hit at `MAX_PX_PER_SRC_PX / fit`. Clamped to ≥ 1.0 so
/// fit-to-pane is always reachable, even for an image already magnified past
/// the ceiling at fit (tiny image in a large pane). Degenerate `fit`
/// (zero/non-finite) falls back to the bare ceiling.
pub(in crate::ui) fn png_zoom_max(pane_w: f32, pane_h: f32, img_px: (u32, u32)) -> f32 {
    let iw = img_px.0.max(1) as f32;
    let ih = img_px.1.max(1) as f32;
    let fit = (pane_w / iw).min(pane_h / ih);
    if fit.is_finite() && fit > 0.0 {
        (MAX_PX_PER_SRC_PX / fit).max(1.0)
    } else {
        MAX_PX_PER_SRC_PX
    }
}

/// Invert `visible_roi_px` (ADR 0025 `preview --roi`): the zoom + pan that make
/// the pane's visible window show (at least) the requested source-px rect,
/// clamped to what interactive zoom could reach. At zoom `z` the pane spans
/// `src_w * pane_w / (letterbox_w * z)` source px horizontally (canvas
/// fractions == source fractions, see `visible_roi_px`), so the rect-fitting
/// zoom per axis inverts that; the smaller axis wins so the WHOLE rect stays
/// visible (the other axis shows extra context). Zoom is clamped to
/// `[1, zoom_max]` BEFORE the pan solve — pan is in canvas px, so clamping
/// after would leave it scaled for a canvas that doesn't exist. The pan
/// centres the rect: the canvas centre sits at pane centre + pan, so a rect
/// centre at source fraction `fc` lands mid-pane when `pan = (0.5 - fc) *
/// canvas`. The render pass's existing pan-slack clamp still applies (and the
/// post-clamp `visible_roi_px` is the effective rect echoed to the caller).
/// A request at/past an image edge aims at the edge. `None` on degenerate
/// geometry.
pub(in crate::ui) fn solve_roi_view(
    pane_w: f32,
    pane_h: f32,
    letterbox_w: f32,
    letterbox_h: f32,
    zoom_max: f32,
    src_w: u32,
    src_h: u32,
    roi: RoiRect,
) -> Option<(f32, (f32, f32))> {
    if src_w == 0
        || src_h == 0
        || roi.w == 0
        || roi.h == 0
        || pane_w <= 0.0
        || pane_h <= 0.0
        || letterbox_w <= 0.0
        || letterbox_h <= 0.0
    {
        return None;
    }
    let sw = src_w as f32;
    let sh = src_h as f32;
    let rx = (roi.x as f32).min(sw - 1.0);
    let ry = (roi.y as f32).min(sh - 1.0);
    let rw = (roi.w as f32).min(sw - rx);
    let rh = (roi.h as f32).min(sh - ry);
    let zx = sw * pane_w / (letterbox_w * rw);
    let zy = sh * pane_h / (letterbox_h * rh);
    let zoom = zx.min(zy).clamp(1.0, zoom_max.max(1.0));
    if !zoom.is_finite() {
        return None;
    }
    let fcx = (rx + rw * 0.5) / sw;
    let fcy = (ry + rh * 0.5) / sh;
    let canvas_w = letterbox_w * zoom;
    let canvas_h = letterbox_h * zoom;
    Some((zoom, ((0.5 - fcx) * canvas_w, (0.5 - fcy) * canvas_h)))
}

impl State {
    /// After a zoom change on a paginated preview, re-request the page at
    /// the new on-screen pixel size so rasterized text re-renders crisp
    /// instead of magnifying the fit-sized bitmap (ADR 0021). Only fires
    /// zooming *in* past the current bitmap's detail, with 1.2× hysteresis
    /// so a held/repeated zoom doesn't flood the backend; zooming out just
    /// linear-downsamples the existing higher-res bitmap. The GPU keeps
    /// showing the stretched texture until the sharper reply swaps in.
    pub(in crate::ui) fn maybe_reraster_page(&mut self) {
        let Some((page, _count)) = self.preview_page else {
            return;
        };
        let target = self.preview_png_zoom.max(1.0);
        let have = self.preview_page_raster_zoom;
        let pending = self
            .preview_page_raster_pending
            .map(|(_, z)| z)
            .unwrap_or(0.0);
        if target <= have.max(pending) * 1.2 {
            return;
        }
        let Some(node_id) = self.preview_node_id_fired.clone() else {
            return;
        };
        let (Some(fw), Some(fh)) = self.preview_fit_px() else {
            return;
        };
        // The plugin caps the long side at 4096; scaling here just keeps the
        // request honest about what's needed at this zoom.
        let scaled_w = ((fw as f32 * target).round() as u32).clamp(1, 8192);
        let scaled_h = ((fh as f32 * target).round() as u32).clamp(1, 8192);
        let generation = self.next_preview_gen();
        if self
            .send(crate::net::transport::OutgoingReq::PreviewGet {
                node_id,
                workspace_id: self.active_workspace_id.clone(),
                page: Some(page),
                fit_w: Some(scaled_w),
                fit_h: Some(scaled_h),
                generation,
            })
            .is_ok()
        {
            self.preview_page_raster_pending = Some((page, target));
        }
    }

    /// Scale the PNG pan offset when the zoom changes so the point at the
    /// centre of the field of view stays put. `pan_px` is measured in
    /// zoomed-canvas pixels (canvas = letterbox × zoom), so the image
    /// fraction off-centre is `pan / (letterbox × zoom)`. Holding that
    /// fraction fixed across a zoom change means pan scales with the zoom
    /// ratio — without this, zooming while panned drifts the view off the
    /// point you were looking at. The render path re-clamps pan to the new
    /// slack, so over-scrolled values self-correct.
    pub(in crate::ui) fn scale_png_pan_for_zoom(&mut self, cur: f32, next: f32) {
        if cur <= 0.0 {
            return;
        }
        let ratio = next / cur;
        self.preview_png_pan_px.0 *= ratio;
        self.preview_png_pan_px.1 *= ratio;
    }
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
