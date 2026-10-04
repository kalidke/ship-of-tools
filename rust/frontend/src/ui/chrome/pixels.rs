//! The chrome's pixel layer: the projected text lines, the border quads, and the overlay text layer's prepare.

use super::*;

impl State {
    pub(in crate::ui) fn project_chrome(
        &mut self,
    ) -> Result<(Vec<crate::text::Line>, HashMap<(u8, u8, u8), Vec<ScreenRect>>, Vec<ScreenRect>)> {
        let mut lines = self.terminal.backend().project_lines(
            self.chrome_origin_x,
            self.chrome_origin_y,
            self.cell_w,
            self.cell_h,
        );

        let mut border_rects_by_color = self.project_border_quads()?;
        let strip_logo_rects = self.draw_session_strip(&mut lines, &mut border_rects_by_color)?;
        Ok((lines, border_rects_by_color, strip_logo_rects))
    }

    fn project_border_quads(&mut self) -> Result<HashMap<(u8, u8, u8), Vec<ScreenRect>>> {
        // Pane borders: render ratatui's box-drawing glyphs (│ ─ ┌ …) as
        // solid quads sized to the exact cell instead of font glyphs.
        // cosmic-text lays the generic monospace font's box glyphs inside the
        // leading-padded cell, so stacked `│` show sub-cell gaps; arm-from-
        // centre quads tile seamlessly by construction and are font-independent
        // (see chrome::project_border_quads). Same origin/scale as project_lines
        // above so the quads sit on the glyph grid exactly.
        //
        // Thickness ≈ 9% of cell height → a thin ~1–2px light-border weight that
        // scales with DPI (cell_h is BASE_CELL_H * scale).
        let border_thickness = border_thickness_px(self.cell_h);
        let border_quads_raw = self.terminal.backend().project_border_quads(
            self.chrome_origin_x,
            self.chrome_origin_y,
            self.cell_w,
            self.cell_h,
            border_thickness,
        );
        // Group rects by colour (1–2 colours typical): `Quad::render_many` is
        // one colour per Quad, so the pass below does one batched draw per
        // colour.
        let mut border_rects_by_color: HashMap<(u8, u8, u8), Vec<ScreenRect>> = HashMap::new();
        for bq in &border_quads_raw {
            border_rects_by_color
                .entry(bq.color)
                .or_default()
                .push(ScreenRect {
                    x: bq.x,
                    y: bq.y,
                    w: bq.w,
                    h: bq.h,
                });
        }
        // Ensure a cached 1×1 solid Quad exists for each colour BEFORE the
        // render pass — building inside the pass would need &mut
        // self.border_quads while the pass already holds other &self borrows.
        // Doing it here keeps the pass to pure iter_mut + render_many.
        for color in border_rects_by_color.keys() {
            if !self.border_quads.contains_key(color) {
                let (r, g, b) = *color;
                let quad = Quad::from_rgba8(
                    &self.device,
                    &self.queue,
                    &self.quad_pipeline,
                    &[r, g, b, 255],
                    1,
                    1,
                )
                .context("failed to build border-colour quad")?;
                self.border_quads.insert(*color, quad);
            }
        }
        Ok(border_rects_by_color)
    }

    pub(in crate::ui) fn prepare_overlays(&mut self) -> Result<(Option<ScreenRect>, Vec<crate::text::Line>)> {
        let mut help_overlay_rect = None;
        let mut help_overlay_lines = Vec::new();
        let mut help_opacity = 1.0;
        if let Some(peek) = self.help.peek.clone() {
            let now = std::time::Instant::now();
            help_opacity = peek.opacity(now);
            if help_opacity <= 0.0 || peek.context != self.help_context() {
                self.help.peek = None;
            } else {
                let rect = match peek.context.pane {
                    help::Pane::Nav => self.pane_rects.nav,
                    help::Pane::Preview => self.pane_rects.preview,
                    help::Pane::Agent => self.pane_rects.llm,
                    _ => self.pane_rects.repl,
                };
                let content = help::peek_lines(&peek.context, &self.bindings);
                let text_width = rect.width.saturating_sub(4) as usize;
                let longest = content.iter().map(|s| unicode_width::UnicodeWidthStr::width(s.as_str())).max().unwrap_or(0);
                if text_width < longest || rect.height < content.len() as u16 + 2 {
                    self.open_help_drawer(peek.context);
                } else {
                    self.nav_spill_segments.clear();
                    let px = ScreenRect {
                        x: self.chrome_origin_x + rect.x as f32 * self.cell_w,
                        y: self.chrome_origin_y + rect.y as f32 * self.cell_h,
                        w: rect.width as f32 * self.cell_w,
                        h: (content.len() as f32 + 2.0) * self.cell_h,
                    };
                    help_overlay_rect = Some(px);
                    help_overlay_lines = content.into_iter().enumerate().map(|(i, text)| crate::text::Line {
                        text, x: px.x + 2.0 * self.cell_w, y: px.y + (i as f32 + 1.0) * self.cell_h,
                        color: Some((167, 222, 231)), bold: i == 0, italic: false, dim: false,
                    }).collect();
                    let alpha = (help_opacity * 245.0).round() as u8;
                    if self.help_back_quad.as_ref().map(|(a, _)| *a) != Some(alpha) {
                        self.help_back_quad = Some((alpha, Quad::from_rgba8(&self.device, &self.queue,
                            &self.quad_pipeline, &[14, 30, 46, alpha], 1, 1)?));
                    }
                }
            }
        }

        // Nav-spill overlay text: the segments the draw closure just
        // collected, converted cell→px with the SAME origin/cell math the
        // chrome lines use so the overlay realigns pixel-identically over
        // the row it covers. Prepared EVERY frame — an empty list is what
        // clears the overlay renderer's retained geometry (see
        // `prepare_overlay`'s doc), so no `if` around this call.
        let mut overlay_lines: Vec<crate::text::Line> = self
            .nav_spill_segments
            .iter()
            .map(|seg| crate::text::Line {
                text: seg.text.clone(),
                x: self.chrome_origin_x + seg.x as f32 * self.cell_w,
                y: self.chrome_origin_y + seg.row as f32 * self.cell_h,
                color: seg.color,
                bold: seg.bold,
                italic: false,
                dim: seg.dim,
            })
            .collect();
        let fade_start = overlay_lines.len();
        overlay_lines.extend(help_overlay_lines);
        self.text.prepare_overlay(
            &self.device,
            &self.queue,
            self.config.width,
            self.config.height,
            &overlay_lines,
            Some((fade_start, help_opacity)),
        )?;
        Ok((help_overlay_rect, overlay_lines))
    }
}
