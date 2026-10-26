//! The figure overlays: the scalebar toggle and its shaped label, the caption store and the shaped caption, and the
//! caption band the last frame reserved.

use super::overlay::CaptionStore;
use crate::ui::*;

/// What the image pane draws over a figure. The toggle and the captions persist; the labels are shaped each frame
/// outside the render pass and cleared whenever their overlay is not drawn.
pub(in crate::ui) struct FigureOverlays {
    /// Scalebar overlay toggle (ADR 0034, `b`). Default off; sticky across
    /// navigations (renders only when the installed scale is present, so it can
    /// stay armed while browsing a mix of scaled / unscaled rasters).
    scalebar_on: bool,
    /// The shaped scalebar label buffer (e.g. `500 nm`), rebuilt each frame
    /// from the adaptive bar length by `build_scalebar` (shaped OUTSIDE the
    /// render pass). `None` when the bar isn't drawn.
    scalebar_label: Option<MarkdownPreview>,
    /// Agent-supplied figure captions, sticky per (host, listed workspace slug, file).
    /// Commands write the resolved target key and rendering reads the corresponding
    /// active row key. The store is shared across snapshots because its keys carry
    /// the host and workspace identity.
    captions: CaptionStore,
    /// The shaped caption buffer for the image on screen, rebuilt each frame by
    /// `build_caption` (shaped OUTSIDE the render pass, like `scalebar_label`).
    /// `None` when no caption is drawn.
    caption_label: Option<MarkdownPreview>,
    /// Height in px of the caption band the LAST frame reserved (0.0 for none).
    /// Published by the render pass purely so the KEYBOARD zoom/pan handler can
    /// reach it: that handler runs outside the render pass, rebuilds the pane
    /// rect from cells, and would otherwise compute its zoom ceiling against the
    /// unreduced pane — making the reachable max zoom depend on whether a
    /// caption happens to be set (verified 2.9%–6.4% short on a
    /// height-constrained image). Band height is only knowable after the caption
    /// is shaped, so it has to be stashed rather than recomputed.
    caption_band_px: f32,
}

impl FigureOverlays {
    pub(in crate::ui) fn new(scalebar_on: bool) -> Self {
        Self {
            scalebar_on,
            scalebar_label: None,
            captions: CaptionStore::default(),
            caption_label: None,
            caption_band_px: 0.0,
        }
    }

    pub(in crate::ui) fn scalebar_on(&self) -> bool { self.scalebar_on }
    pub(in crate::ui) fn toggle_scalebar(&mut self) { self.scalebar_on = !self.scalebar_on; }
    pub(in crate::ui) fn enable_scalebar(&mut self) { self.scalebar_on = true; }
    /// A scale save failed: the bar never appeared, so the overlay is not left armed.
    pub(in crate::ui) fn disable_scalebar(&mut self) { self.scalebar_on = false; }

    pub(in crate::ui) fn scalebar_label(&self) -> Option<&MarkdownPreview> { self.scalebar_label.as_ref() }
    pub(in crate::ui) fn show_scalebar_label(&mut self, label: MarkdownPreview) { self.scalebar_label = Some(label); }
    pub(in crate::ui) fn clear_scalebar_label(&mut self) { self.scalebar_label = None; }

    pub(in crate::ui) fn captions(&self) -> &CaptionStore { &self.captions }
    pub(in crate::ui) fn set_caption(&mut self, ws_key: WsKey, node_id: String, text: String) {
        self.captions.set(ws_key, node_id, text);
    }
    pub(in crate::ui) fn clear_caption(&mut self, ws_key: &WsKey, node_id: &str) {
        self.captions.clear_one(ws_key, node_id);
    }

    pub(in crate::ui) fn caption_label(&self) -> Option<&MarkdownPreview> { self.caption_label.as_ref() }
    pub(in crate::ui) fn show_caption_label(&mut self, label: MarkdownPreview) { self.caption_label = Some(label); }
    pub(in crate::ui) fn clear_caption_label(&mut self) { self.caption_label = None; }

    pub(in crate::ui) fn caption_band_px(&self) -> f32 { self.caption_band_px }
    /// The band this frame reserved, published before the image rect is derived from it.
    pub(in crate::ui) fn publish_caption_band(&mut self, px: f32) { self.caption_band_px = px; }
}
