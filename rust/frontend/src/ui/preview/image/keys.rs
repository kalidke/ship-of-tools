//! Image preview keys: zoom, pan, reset and the scalebar.

use crate::ui::*;
use std::ops::ControlFlow::{self, Break, Continue};
use crate::ui::input::keypress::KeyPress;

const ZOOM_STEP: f32 = 1.25;

pub(in crate::ui) fn png_key(state: &mut State, key: KeyPress<'_>, label: String) -> ControlFlow<()> {
    let KeyPress { event, action, .. } = key;
    // PNG-pane zoom/pan routed through `KeyBindings`
    // so `.sot/keybindings.toml` can rebind each
    // action. Defaults: zoom in/out is Shift+Arrow
    // up/down (plus `+`/`=`/`-`); reset is `r` or
    // `0`; pan is the bare arrows. Order matters —
    // ZoomIn checked before PanUp so Shift+ArrowUp
    // doesn't double-fire (the Chord matcher ignores
    // surplus shift for the `=`/`+` compatibility
    // case, so the same key can match both action
    // lists; first-hit wins). Pan step is 10% of
    // the pane size per press so the perceived
    // increment is constant regardless of zoom; the
    // render-time clamp keeps the canvas covering
    // the pane.
    if let Some(img_px) = state.preview_png.as_ref().map(|q| q.size_px) {
        const PAN_FRAC: f32 = 0.1;
        // Subtract the reserved figure-caption band before
        // ANY of this: the image lives in `image_rect`, not
        // the whole pane, and png_zoom_max keys off pane
        // height (fit = min(w/iw, h/ih)). Computing the
        // ceiling against the unreduced pane made the
        // reachable max zoom depend on whether a caption
        // happened to be set — silently 2.9%–6.4% short on a
        // height-constrained image, since the render path
        // re-clamps with the correct (larger) ceiling, so it
        // degraded to under-zoom rather than a misdraw.
        // Extracted so it's testable — see
        // `preview_image_pane_px`. Reads the band height the
        // last frame published; the one-frame lag is
        // inherent (the band isn't known until the caption
        // is shaped) and harmless, since the render pass
        // re-clamps zoom against its own current ceiling.
        let pane_rect = preview_image_pane_px(
            (
                state.pane_rects.preview.width,
                state.pane_rects.preview.height,
            ),
            state.cell_w,
            state.cell_h,
            state.caption_band_px,
        );
        let (pane_w, pane_h) = (pane_rect.w, pane_rect.h);
        // Zoom ceiling is per-image: how big a single
        // source pixel may get on screen (16×16 px), not
        // a fixed multiple of fit-to-pane. A dense raster
        // whose native pixels are sub-screen-pixel at fit
        // gets generous headroom; a tiny already-magnified
        // image is held near fit.
        let zoom_max = png_zoom_max(pane_w, pane_h, img_px);
        let mut handled = true;
        if !event.repeat
            && action == Some(Action::PreviewPngReset)
        {
            state.preview_png_zoom = 1.0;
            state.preview_png_pan_px = (0.0, 0.0);
        } else if action == Some(Action::PreviewPngZoomIn) {
            png_zoom_in(state, zoom_max);
        } else if action == Some(Action::PreviewPngZoomOut) {
            png_zoom_out(state);
        } else if action == Some(Action::PreviewPngPanLeft) {
            state.preview_png_pan_px.0 += pane_w * PAN_FRAC;
        } else if action == Some(Action::PreviewPngPanRight) {
            state.preview_png_pan_px.0 -= pane_w * PAN_FRAC;
        } else if action == Some(Action::PreviewPngPanUp) {
            state.preview_png_pan_px.1 += pane_h * PAN_FRAC;
        } else if action == Some(Action::PreviewPngPanDown) {
            state.preview_png_pan_px.1 -= pane_h * PAN_FRAC;
        } else if action == Some(Action::PreviewScalebarToggle)
        {
            // ADR 0034 Ctrl+S. With a scale present this
            // flips the overlay; with NONE it opens the
            // pixel-size prompt (§4 live entry) instead of
            // no-opping, so an uncalibrated raster is one
            // keystroke from a real bar.
            if state.preview_scale.is_some() {
                state.scalebar_on = !state.scalebar_on;
            } else if !state.begin_scale_entry() {
                state.status = "scalebar · no image previewed".to_string();
            }
        } else {
            handled = false;
        }
        if handled {
            // View carry is written through from the
            // render pass (`preview_png_cache`) — a save
            // here would record LAST frame's ROI.
            // Paginated page (PDF): re-rasterize at the
            // new zoom so text stays crisp past 1×.
            state.maybe_reraster_page();
            state.last_key = Some(label);
            state.window.request_redraw();
            return Break(());
        }
    }
    Continue(())
}

fn png_zoom_in(state: &mut State, zoom_max: f32) {
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
    let cur = state.preview_png_zoom;
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
    state.scale_png_pan_for_zoom(cur, next);
    state.preview_png_zoom = next;
}

fn png_zoom_out(state: &mut State) {
    // Mirror of zoom-in: integer-step down
    // from ≥ 2, then drop back through 1.25
    // → 1.0. Hitting 2.0 → 1.25 is the
    // discrete jump out of integer mode so
    // the user lands cleanly on the
    // multiplicative step below 1.5×.
    let cur = state.preview_png_zoom;
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
    state.scale_png_pan_for_zoom(cur, next);
    state.preview_png_zoom = next;
    if state.preview_png_zoom <= 1.0 {
        state.preview_png_pan_px = (0.0, 0.0);
    }
}
