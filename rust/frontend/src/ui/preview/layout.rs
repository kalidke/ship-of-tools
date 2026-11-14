//! The preview pane's pixel layout for a frame: which source shows, the figure and caption band, the markdown rect,
//! the concept rect and the scroll clamp.

use crate::ui::*;

/// What the pane shows. ADR 0030 §2, ADR 0049: the refused-hello overlay is a
/// hard block: it takes the pane over everything until a clean reconnect clears it. Edit mode owns the pane after it, so
/// the editable annotation body has the whole rect. `canvas` arrives decided (`InkLayer::shown_base`, which yields it
/// only outside both); it takes the pane from the image and the text, and the installed preview stays installed under it.
pub(in crate::ui) fn shows(refused: bool, raster: bool, edit: bool, canvas: bool) -> PreviewShows {
    let show_edit = !refused && edit;
    let show_png = raster && !refused && !canvas;
    let show_md = !refused && !show_png && !show_edit && !canvas;
    PreviewShows { fatal: refused, png: show_png, edit: show_edit, md: show_md, canvas }
}

/// The pane's decision for a frame, one flag per source; every reader names the one it asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ui) struct PreviewShows {
    pub(in crate::ui) fatal: bool,
    pub(in crate::ui) png: bool,
    pub(in crate::ui) edit: bool,
    pub(in crate::ui) md: bool,
    pub(in crate::ui) canvas: bool,
}

impl State {
    pub(in crate::ui) fn preview_shows(&self) -> PreviewShows {
        let show_fatal = self.hello_refused.refused(self.selected.host());
        let edit = self.editor.is_editing() && self.editor.shaped().is_some();
        let canvas = self.shown_ink_base() == Some(sot_protocol::ops::InkBase::Canvas);
        shows(show_fatal, self.raster.is_shown(), edit, canvas)
    }

    pub(in crate::ui) fn layout_figure(
        &mut self,
        preview_rect: ScreenRect,
        show_png: bool,
    ) -> (Option<crate::ui::preview::image::overlay::CaptionDraw>, ScreenRect, Option<ScreenRect>) {
        // Figure caption (agent-supplied, ADR 0025): a band RESERVED at the
        // bottom of the preview pane. Computed here, before `png_rect`, because
        // every piece of image geometry downstream — letterbox fit, the zoom
        // ceiling, pan slack, the ROI solve and its inverse, the scissor — keys
        // off the pane rect, and reserving space only works if they all agree on
        // the SAME reduced rect. `image_rect` is that rect; `preview_rect`
        // continues to mean the whole pane for text/media/overlay draws.
        let caption_draw = if show_png {
            self.build_caption(preview_rect)
        } else {
            // No raster on screen (help, edit modal, markdown, fatal overlay):
            // nothing to caption, and the text pane keeps the full rect.
            self.overlays.clear_caption_label();
            None
        };
        // Publish the band height BEFORE deriving image_rect, so the keyboard
        // zoom/pan handler (which runs outside the render pass) derives its pane
        // from the same number this frame drew with.
        self.overlays.publish_caption_band(caption_draw.as_ref().map_or(0.0, |c| c.backing.h));
        let image_rect = image_rect_for_caption(preview_rect, self.overlays.caption_band_px());

        let png_rect = if show_png {
            self.raster
                .quad_size()
                .map(|size_px| letterbox(image_rect, size_px))
        } else {
            None
        };
        (caption_draw, image_rect, png_rect)
    }

    pub(in crate::ui) fn layout_markdown(&mut self, preview_rect: ScreenRect, cell_w: f32, cell_h: f32) -> ScreenRect {
        // Inset the markdown content from the pane edge so text isn't flush
        // against the border — GitHub (`.markdown-body` padding) and VSCode
        // (~26px body padding) both gutter their rendered markdown. We translate
        // that to cell units: ~1 char each side + a half-line top/bottom. Tune
        // PREVIEW_PAD_X/Y to taste. Images keep the full `preview_rect` (the PNG
        // path above letterboxes into it) — only flowed text gets the gutter.
        const PREVIEW_PAD_X: f32 = 1.0; // cells, each side
        const PREVIEW_PAD_Y: f32 = 0.5; // cells, top & bottom
        let pad_x = PREVIEW_PAD_X * cell_w;
        let pad_y = PREVIEW_PAD_Y * cell_h;
        // Re-shape the markdown buffer if the pane rect changed shape.
        let md_rect = ScreenRect {
            x: preview_rect.x + pad_x,
            y: preview_rect.y + pad_y,
            w: (preview_rect.w - 2.0 * pad_x).max(1.0),
            h: (preview_rect.h - 2.0 * pad_y).max(1.0),
        };
        self.canvas.shape(|fs| self.shaped.layout(md_rect, fs));
        md_rect
    }

    pub(in crate::ui) fn rebuild_fatal_if_shown(&mut self, show_fatal: bool) {
        // ADR 0030 §2: same lazy build for the protocol-mismatch overlay, once
        // md_rect_px reflects the real preview width so the message wraps right.
        if show_fatal && self.shaped.fatal().is_none() {
            self.rebuild_fatal_overlay();
        }
    }

    pub(in crate::ui) fn layout_concept(&mut self, preview_rect: ScreenRect) {
        // Same dance for the concept pane. Shares the same rect now that
        // it's a single preview slot.
        self.canvas.shape(|fs| self.concept.layout(preview_rect, fs));
    }

    /// What a person's scroll asks with: the event's instant and how long it takes to arrive. The one place the
    /// length is read, so every call site asks alike.
    pub(in crate::ui) fn scroll_ask(&self) -> (std::time::Instant, std::time::Duration) {
        (std::time::Instant::now(), crate::ui::motion::SCROLL_LENGTH)
    }

    pub(in crate::ui) fn clamp_preview_scroll(&mut self, show_edit: bool, md_rect: ScreenRect, preview_rect: ScreenRect) -> f32 {
        // Clamp the preview scroll so the user can't walk past the end
        // of the document. Pixel-summing per LayoutLine accounts for
        // per-line `line_height_opt` overrides emitted by tall
        // placeholder spans (display math, embedded figures) — using
        // a body-line count alone undercounts the document height by
        // (figure_height - body_line_h) for every embedded media row.
        // Clamp by the one buffer on screen, in the rect it is drawn in, so a
        // hidden buffer never scrolls the pane into blank space.
        let line_h = self.shaped.md().line_height().max(1.0);
        // The extras paint with `EXTRA_TOP_PAD_PX` of headroom, so each
        // frame only renders `the drawn rect's height - pad` pixels of content. Subtract
        // the pad from `visible_px` so max_scroll lets the user reach the
        // actual bottom of the document without losing the tail to the
        // padding.
        let (shown, shown_h) = preview_scroll_target(
            show_edit,
            self.shaped.md(),
            md_rect.h,
            self.editor.shaped(),
            preview_rect.h,
        );
        let visible_px = (shown_h - crate::ui::render::text::EXTRA_TOP_PAD_PX).max(line_h);
        // The preview scroll is body-line units; convert the pixel slack
        // back via ceil so the final body-line step always lands the
        // bottom of the document on screen (no off-by-fraction clip).
        let max_scroll = preview_max_scroll(line_h, visible_px, shown);
        // A person's scroll eases over frames: spend this frame's share, and keep frames coming until it rests.
        if self.shaped.advance(std::time::Instant::now(), max_scroll, visible_px / line_h, show_edit) {
            self.pacer.mark_dirty();
        }
        self.shaped.scroll_px()
    }
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod layout_tests;
