//! The preview pane's pixel layout for a frame: which source shows, the figure and caption band, the markdown rect,
//! the concept rect and the scroll clamp.

use crate::ui::*;

impl State {
    pub(in crate::ui) fn preview_shows(&self) -> (bool, bool, bool, bool, bool) {
        // ADR 0030 §2, ADR 0049: the refused-hello overlay is a hard block — it takes
        // the preview pane over EVERYTHING (help included) until a clean
        // reconnect clears it. Rebuilt lazily below once md_rect_px is known.
        let show_fatal = self.hello_refused.contains_key(&self.active_host);
        let show_png = self.preview_png.is_some() && !show_fatal;
        let show_svg = false;
        // Edit mode owns the preview pane: the file viewer hides so
        // the editable annotation body has the whole rect.
        let show_edit =
            !show_fatal && self.edit_state.is_some() && self.preview_edit.is_some();
        let show_md = !show_fatal && !show_png && !show_edit;
        (show_fatal, show_png, show_svg, show_edit, show_md)
    }

    pub(in crate::ui) fn layout_figure(
        &mut self,
        preview_rect: ScreenRect,
        show_png: bool,
        show_svg: bool,
    ) -> (Option<crate::ui::preview::image::overlay::CaptionDraw>, ScreenRect, Option<ScreenRect>, Option<ScreenRect>) {
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
            self.caption_label = None;
            None
        };
        // Publish the band height BEFORE deriving image_rect, so the keyboard
        // zoom/pan handler (which runs outside the render pass) derives its pane
        // from the same number this frame drew with.
        self.caption_band_px = caption_draw.as_ref().map_or(0.0, |c| c.backing.h);
        let image_rect = image_rect_for_caption(preview_rect, self.caption_band_px);

        let png_rect = if show_png {
            self.preview_png
                .as_ref()
                .map(|q| letterbox(image_rect, q.size_px))
        } else {
            None
        };
        let svg_rect = if show_svg {
            self.preview_svg
                .as_ref()
                .map(|q| letterbox(preview_rect, q.size_px))
        } else {
            None
        };
        (caption_draw, image_rect, png_rect, svg_rect)
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
        let size_changed = (md_rect.w - self.md_rect_px.w).abs() > 0.5
            || (md_rect.h - self.md_rect_px.h).abs() > 0.5;
        if size_changed {
            self.preview_md
                .resize(self.text.font_system_mut(), md_rect.w, md_rect.h);
        }
        self.md_rect_px = md_rect;
        md_rect
    }

    pub(in crate::ui) fn rebuild_fatal_if_shown(&mut self, show_fatal: bool) {
        // ADR 0030 §2: same lazy build for the protocol-mismatch overlay, once
        // md_rect_px reflects the real preview width so the message wraps right.
        if show_fatal && self.preview_fatal.is_none() {
            self.rebuild_fatal_overlay();
        }
    }

    pub(in crate::ui) fn layout_concept(&mut self, preview_rect: ScreenRect) {
        // Same dance for the concept pane. Shares the same rect now that
        // it's a single preview slot.
        let concept_rect = preview_rect;
        let concept_size_changed = (concept_rect.w - self.concept_rect_px.w).abs() > 0.5
            || (concept_rect.h - self.concept_rect_px.h).abs() > 0.5;
        if concept_size_changed {
            if let Some(pc) = self.preview_concept.as_mut() {
                pc.resize(self.text.font_system_mut(), concept_rect.w, concept_rect.h);
            }
        }
        self.concept_rect_px = concept_rect;
    }

    pub(in crate::ui) fn clamp_preview_scroll(&mut self, show_edit: bool, md_rect: ScreenRect, preview_rect: ScreenRect) -> f32 {
        // Clamp `preview_scroll` so the user can't walk past the end
        // of the document. Pixel-summing per LayoutLine accounts for
        // per-line `line_height_opt` overrides emitted by tall
        // placeholder spans (display math, embedded figures) — using
        // a body-line count alone undercounts the document height by
        // (figure_height - body_line_h) for every embedded media row.
        // Clamp by the one buffer on screen, in the rect it is drawn in, so a
        // hidden buffer never scrolls the pane into blank space.
        let line_h = self.preview_md.line_height().max(1.0);
        // The extras paint with `EXTRA_TOP_PAD_PX` of headroom, so each
        // frame only renders `the drawn rect's height - pad` pixels of content. Subtract
        // the pad from `visible_px` so max_scroll lets the user reach the
        // actual bottom of the document without losing the tail to the
        // padding.
        let (shown, shown_h) = preview_scroll_target(
            show_edit,
            &self.preview_md,
            md_rect.h,
            self.preview_edit.as_ref(),
            preview_rect.h,
        );
        let visible_px = (shown_h - crate::ui::render::text::EXTRA_TOP_PAD_PX).max(line_h);
        // `preview_scroll` is body-line units; convert the pixel slack
        // back via ceil so the final body-line step always lands the
        // bottom of the document on screen (no off-by-fraction clip).
        let max_scroll = preview_max_scroll(line_h, visible_px, shown);
        self.preview_scroll = self.preview_scroll.min(max_scroll);
        let preview_scroll_px = self.preview_scroll as f32 * line_h;
        preview_scroll_px
    }
}
