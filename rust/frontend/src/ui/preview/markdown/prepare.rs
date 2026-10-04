//! The markdown pane's per-frame text prepare: the math and table media, then the text layer's `prepare` with the
//! preview, caption, scalebar and table buffers as extra areas.

use crate::ui::*;

impl State {
    pub(in crate::ui) fn prepare_media(&mut self, show_md: bool, md_rect: ScreenRect,
                     preview_scroll_px: f32) -> Vec<(usize, ScreenRect)> {
        // Walk the laid-out markdown buffer for FFFC placeholders, zip
        // with `preview_md.media_blocks` by appearance order, and
        // pre-rasterise any math SVGs we have that haven't been
        // rasterised yet at the current pane width. Painting happens
        // inside the rpass; do the side-effecty rasterise here
        // while we have &mut self.
        let media_paint_targets: Vec<(usize, ScreenRect)> = if show_md {
            self.collect_media_paint_targets(md_rect, preview_scroll_px)
        } else {
            Vec::new()
        };
        // Build / refresh per-table cosmic-text buffers — must run
        // before the extras are assembled below because the extras
        // borrow `&self.table_buffers[i].buffer`. The fn is a no-op
        // when the buffer set already matches `preview_md.media_blocks`
        // (typical steady-state across redraws). It also resets
        // `md_table_scroll_px` to 0 whenever buffers are rebuilt, so
        // navigating to a different doc starts at scroll-left.
        if show_md {
            self.ensure_table_buffers();
        } else {
            self.table_buffers.clear();
        }
        // Clamp the shared horizontal scroll so the user can't walk
        // past the right edge of the widest table on the current doc.
        // Uses the widest natural width across all tables — single
        // scroll var means the clamp has to cover them all (the
        // narrower tables just go past their own right edge into
        // empty space, which TextBounds clips invisibly).
        let widest_table_w = self
            .table_buffers
            .iter()
            .map(|e| e.natural_w_px)
            .fold(0.0_f32, f32::max);
        let table_max_scroll = (widest_table_w - md_rect.w).max(0.0);
        self.md_table_scroll_px = self.md_table_scroll_px.clamp(0.0, table_max_scroll);
        let body_em_px = self.preview_md.body_em().max(1.0);
        for (block_idx, rect) in &media_paint_targets {
            let Some(block) = self.preview_md.media_blocks.get(*block_idx) else {
                continue;
            };
            let crate::ui::preview::markdown::MediaBlock::Math { latex, display } = block else {
                continue;
            };
            let key = (latex.clone(), *display);
            let Some(entry) = self.math_cache.get_mut(&key) else {
                continue;
            };
            if entry.rasterised.is_some() {
                continue;
            }
            // Natural pixel size, derived from the SVG's ex-unit
            // dimensions and the body font. Clamped to the row
            // reservation so a malformed `<svg>` tag (or an
            // exceptionally tall `aligned` block) can't overflow the
            // letterbox and trample neighbouring paragraphs. The fit-
            // scale used to happen inside `quad_from_svg_bytes`; doing
            // it here lets short equations rasterise at their actual
            // size (e.g. ~3ex tall) instead of being stretched to fill
            // the slab.
            let (target_w, target_h) = match (entry.width_ex, entry.height_ex) {
                (Some(w_ex), Some(h_ex)) => {
                    let nat_w = (w_ex * MATHJAX_EX_FACTOR * body_em_px).max(1.0);
                    let nat_h = (h_ex * MATHJAX_EX_FACTOR * body_em_px).max(1.0);
                    let max_w = rect.w.max(1.0);
                    let max_h = rect.h.max(1.0);
                    // Uniform downscale only — never enlarge past natural.
                    let s = (max_w / nat_w).min(max_h / nat_h).min(1.0).max(0.001);
                    (
                        (nat_w * s).ceil().max(1.0) as u32,
                        (nat_h * s).ceil().max(1.0) as u32,
                    )
                }
                _ => {
                    // Pre-fix fallback: no parsed dims, letterbox into
                    // the row reservation. Should be rare — the SVG's
                    // root tag is well-formed in every observed case.
                    ((rect.w as u32).max(1), (rect.h as u32).max(1))
                }
            };
            match quad_from_svg_bytes(
                &self.device,
                &self.queue,
                &self.quad_pipeline,
                &entry.svg_bytes,
                target_w,
                target_h,
            ) {
                Ok(q) => entry.rasterised = Some(q),
                Err(e) => tracing::warn!(error = %e,
                    latex_len = entry.svg_bytes.len(),
                    "math svg rasterise failed"),
            }
        }
        media_paint_targets
    }

    #[allow(clippy::too_many_lines, reason = "prepares the preview text, laying out each block kind in turn; predates the 100-line limit")]
    pub(in crate::ui) fn prepare_preview_text(
        &mut self,
        lines: Vec<crate::ui::render::text::Line>,
        media_paint_targets: Vec<(usize, ScreenRect)>,
        preview_rect: ScreenRect,
        image_rect: ScreenRect,
        png_rect: Option<ScreenRect>,
        md_rect: ScreenRect,
        preview_scroll_px: f32,
        show_md: bool,
        show_edit: bool,
        show_fatal: bool,
        caption_draw: &Option<crate::ui::preview::image::overlay::CaptionDraw>,
    ) -> Result<(Option<crate::ui::preview::image::overlay::ScalebarDraw>, Vec<(usize, ScreenRect)>)> {
        // ADR 0034: compute the scalebar geometry + shape its label BEFORE the
        // `extras` borrows and `text.prepare` — the label is pushed as an
        // ExtraArea below, and the bar rects are drawn inside the pass. `&mut
        // self` here (font_system + scalebar_label); done before the shared
        // buffer borrows the `extras` Vec takes.
        // `image_rect`, not `preview_rect`: the bar belongs to the figure, so
        // when a caption reserves the bottom band the bar rides above it with
        // no inset arithmetic of its own.
        let scalebar_draw = self.build_scalebar(png_rect, image_rect);

        // The preview text is laid out at its own line pitch, so the cell-grid
        // bottom rarely lands on a line boundary; clip at the last whole line.
        let whole_line_clip = |p: &MarkdownPreview, r: ScreenRect, scroll_px: f32| {
            r.y + crate::ui::render::text::EXTRA_TOP_PAD_PX
                + p.whole_line_bottom(scroll_px, r.h - crate::ui::render::text::EXTRA_TOP_PAD_PX)
        };
        let md_clip_bottom = whole_line_clip(&self.preview_md, md_rect, preview_scroll_px);
        let mut extras: Vec<crate::ui::render::text::ExtraArea> = Vec::new();
        if let (Some(sb), Some(lbl)) = (scalebar_draw.as_ref(), self.scalebar_label.as_ref()) {
            extras.push(crate::ui::render::text::ExtraArea {
                buffer: &lbl.buffer,
                x: sb.label_x,
                y: sb.label_y,
                right: image_rect.x + image_rect.w,
                bottom: image_rect.y + image_rect.h,
                clip_left: Some(image_rect.x),
                clip_top: Some(image_rect.y),
                // Near-white on the dark backing box drawn under it.
                color: (245, 245, 245),
                scroll_y_px: 0.0,
            });
        }
        if let (Some(cap), Some(lbl)) = (caption_draw.as_ref(), self.caption_label.as_ref()) {
            extras.push(crate::ui::render::text::ExtraArea {
                buffer: &lbl.buffer,
                x: cap.text_x,
                y: cap.text_y,
                right: cap.backing.x + cap.backing.w,
                // `clip_bottom` (not the backing's edge) so a caption longer
                // than CAPTION_MAX_LINES is cut at the band's text area instead
                // of bleeding into the padding.
                bottom: cap.clip_bottom,
                clip_left: Some(cap.backing.x),
                clip_top: Some(cap.backing.y),
                color: (232, 232, 232),
                scroll_y_px: 0.0,
            });
        }
        if show_md {
            extras.push(crate::ui::render::text::ExtraArea {
                buffer: &self.preview_md.buffer,
                x: md_rect.x,
                y: md_rect.y,
                right: md_rect.x + md_rect.w,
                bottom: md_clip_bottom,
                clip_left: None,
                clip_top: None,
                color: (220, 220, 220),
                scroll_y_px: preview_scroll_px,
            });
        }
        if show_edit {
            if let Some(pe) = self.preview_edit.as_ref() {
                extras.push(crate::ui::render::text::ExtraArea {
                    buffer: &pe.buffer,
                    x: preview_rect.x,
                    y: preview_rect.y,
                    right: preview_rect.x + preview_rect.w,
                    bottom: whole_line_clip(pe, preview_rect, preview_scroll_px),
                    clip_left: None,
                    clip_top: None,
                    // Warm gold tint so the user sees at a glance that
                    // this is editable, not the read-only annotation.
                    color: (235, 215, 160),
                    scroll_y_px: preview_scroll_px,
                });
            }
        }
        // ADR 0030 §2: protocol-mismatch overlay, reusing the help overlay's
        // paint path but with a warm red tint so it reads as an error, not a
        // cheat sheet. Trumps everything (highest priority in the cascade).
        if show_fatal {
            if let Some(pf) = self.preview_fatal.as_ref() {
                extras.push(crate::ui::render::text::ExtraArea {
                    buffer: &pf.buffer,
                    x: preview_rect.x,
                    y: preview_rect.y,
                    right: preview_rect.x + preview_rect.w,
                    bottom: whole_line_clip(pf, preview_rect, 0.0),
                    clip_left: None,
                    clip_top: None,
                    color: (240, 160, 150),
                    scroll_y_px: 0.0,
                });
            }
        }
        // Per-table extras — one ExtraArea per MediaBlock::Table, hosted
        // at the FFFC's screen rect with a left-shift of
        // `md_table_scroll_px` so the user can drag the table
        // horizontally. TextBounds at preview-pane edges clip the
        // overflow glyph-by-glyph — no wgpu scissor needed.
        //
        // We iterate `media_paint_targets` (in FFFC source order) and
        // increment a `table_idx` counter on each Table encounter so it
        // walks `table_buffers` parallel to the source-order TableBufferEntry
        // build inside `ensure_table_buffers`.
        if show_md && !self.table_buffers.is_empty() {
            let mut table_idx: usize = 0;
            for (block_idx, rect) in &media_paint_targets {
                let Some(block) = self.preview_md.media_blocks.get(*block_idx) else {
                    continue;
                };
                if !matches!(block, crate::ui::preview::markdown::MediaBlock::Table { .. }) {
                    continue;
                }
                let Some(entry) = self.table_buffers.get(table_idx) else {
                    table_idx += 1;
                    continue;
                };
                table_idx += 1;
                // Cull tables fully scrolled off the vertical viewport
                // — TextBounds would catch them anyway but the cheap
                // skip saves a glyphon TextArea entry.
                if rect.y + rect.h < preview_rect.y || rect.y > preview_rect.y + preview_rect.h {
                    continue;
                }
                // `x` rides the shared horizontal scroll; `y` plants
                // the table's first row exactly at the FFFC's screen
                // y. The ExtraArea pipeline adds EXTRA_TOP_PAD_PX to
                // the y, so we subtract it back here.
                let table_x = rect.x - self.md_table_scroll_px;
                let table_y = rect.y - crate::ui::render::text::EXTRA_TOP_PAD_PX;
                extras.push(crate::ui::render::text::ExtraArea {
                    buffer: &entry.buffer,
                    x: table_x,
                    y: table_y,
                    // Bounds clip to the preview pane in BOTH axes so
                    // the table's natural-width overflow gets glyph-
                    // clipped at the pane right edge, and vertical
                    // scroll past the pane edges is invisible. The bottom
                    // stops at the last whole row.
                    right: preview_rect.x + preview_rect.w,
                    bottom: whole_row_bottom(
                        rect.y,
                        entry.buffer.metrics().line_height,
                        preview_rect.y + preview_rect.h,
                    ),
                    // Pin the bounds.left to the pane edge — the
                    // glyph origin (`x`) is shifted into negative
                    // territory by `md_table_scroll_px` and would
                    // otherwise let bounds.left follow it off-pane.
                    clip_left: Some(preview_rect.x),
                    clip_top: Some(preview_rect.y),
                    color: (220, 220, 220),
                    scroll_y_px: 0.0,
                });
            }
        }

        self.text.prepare(
            &self.device,
            &self.queue,
            self.config.width,
            self.config.height,
            &lines,
            &extras,
        )?;
        Ok((scalebar_draw, media_paint_targets))
    }
}
