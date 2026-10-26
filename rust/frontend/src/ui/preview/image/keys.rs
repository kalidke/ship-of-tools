//! Image preview keys: zoom, pan, reset and the scalebar.

use crate::ui::*;
use std::ops::ControlFlow::{self, Break, Continue};
use crate::ui::input::keypress::KeyPress;

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
    if let Some(img_px) = state.raster.quad_size() {
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
            state.overlays.caption_band_px(),
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
            state.raster.reset_zoom();
        } else if action == Some(Action::PreviewPngZoomIn) {
            state.raster.zoom_in(zoom_max);
        } else if action == Some(Action::PreviewPngZoomOut) {
            state.raster.zoom_out();
        } else if action == Some(Action::PreviewPngPanLeft) {
            state.raster.pan_x(pane_w * PAN_FRAC);
        } else if action == Some(Action::PreviewPngPanRight) {
            state.raster.pan_x(-(pane_w * PAN_FRAC));
        } else if action == Some(Action::PreviewPngPanUp) {
            state.raster.pan_y(pane_h * PAN_FRAC);
        } else if action == Some(Action::PreviewPngPanDown) {
            state.raster.pan_y(-(pane_h * PAN_FRAC));
        } else if action == Some(Action::PreviewScalebarToggle)
        {
            // ADR 0034 Ctrl+S. With a scale present this
            // flips the overlay; with NONE it opens the
            // pixel-size prompt (§4 live entry) instead of
            // no-opping, so an uncalibrated raster is one
            // keystroke from a real bar.
            if state.preview_source.scale().is_some() {
                state.overlays.toggle_scalebar();
            } else if !state.begin_scale_entry() {
                state.status.set("scalebar · no image previewed".to_string());
            }
        } else {
            handled = false;
        }
        if handled {
            // View carry is written through from the
            // render pass (the raster view's carry) — a save
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
