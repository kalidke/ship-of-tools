//! The chrome's pixel layer: the projected text lines, the border quads, and the overlay text layer's prepare.

use super::*;

impl State {
    pub(in crate::ui) fn project_chrome(
        &mut self,
    ) -> Result<(Vec<crate::ui::render::text::Line>, HashMap<(u8, u8, u8), Vec<ScreenRect>>, Vec<ScreenRect>)> {
        let mut lines = self.canvas.chrome().project_lines(
            self.canvas.origin_x(),
            self.canvas.origin_y(),
            self.canvas.cell_w(),
            self.canvas.cell_h(),
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
        let border_thickness = border_thickness_px(self.canvas.cell_h());
        let border_quads_raw = self.canvas.chrome().project_border_quads(
            self.canvas.origin_x(),
            self.canvas.origin_y(),
            self.canvas.cell_w(),
            self.canvas.cell_h(),
            border_thickness,
        );
        // Group rects by colour (1–2 colours typical): `Quad::render_many` is
        // one colour per Quad, so the pass does one batched draw per
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
            self.brushes.ensure_border(*color, &self.gpu, "failed to build border-colour quad")?;
        }
        Ok(border_rects_by_color)
    }

    pub(in crate::ui) fn prepare_overlays(
        &mut self,
    ) -> Result<(Option<ScreenRect>, Option<ScreenRect>, Vec<crate::ui::render::text::Line>)> {
        // The close dialog covers every pane; while it shows, the help peek and the nav spill, whose text would draw
        // over its backing, do not.
        let dialog = self.close_dialog();
        let mut help_overlay_rect = None;
        let mut help_overlay_lines = Vec::new();
        let mut help_opacity = 1.0;
        // Keep clock sampling after the peek observation and before current-context evaluation.
        let mut rect = ratatui::layout::Rect::default();
        let inputs = if dialog.is_none() && self.layout_help.peek().is_some() {
            let now = std::time::Instant::now();
            let context = (self.layout_help.peek().expect("peek observed above").opacity(now) > 0.0)
                .then(|| self.help_context());
            let peek = self.layout_help.peek().expect("peek observed above");
            rect = match peek.context().pane {
                help::Pane::Nav => self.pane_rects.nav,
                help::Pane::Preview => self.pane_rects.preview,
                help::Pane::Agent => self.pane_rects.llm,
                _ => self.pane_rects.repl,
            };
            Some(layout_help::PeekInputs { context, focus: self.focus_prompts.focus(),
                width: rect.width, height: rect.height, now })
        } else { None };
        let prepared = self.layout_help.prepare_peek(dialog.is_some(), inputs, &self.bindings);
        if prepared.clear_spill { self.nav_spill.clear_segments(); }
        prepared.layout.apply(self, None);
        if let Some(overlay) = prepared.overlay {
            help_opacity = overlay.opacity;
            let content = overlay.lines;
            let px = ScreenRect {
                x: self.canvas.origin_x() + rect.x as f32 * self.canvas.cell_w(),
                y: self.canvas.origin_y() + rect.y as f32 * self.canvas.cell_h(),
                w: rect.width as f32 * self.canvas.cell_w(),
                h: (content.len() as f32 + 2.0) * self.canvas.cell_h(),
            };
            help_overlay_rect = Some(px);
            help_overlay_lines = content.into_iter().enumerate().map(|(i, text)| crate::ui::render::text::Line {
                text, x: px.x + 2.0 * self.canvas.cell_w(), y: px.y + (i as f32 + 1.0) * self.canvas.cell_h(),
                color: Some((167, 222, 231)), bold: i == 0, italic: false, dim: false,
            }).collect();
            let alpha = (help_opacity * 245.0).round() as u8;
            self.brushes.ensure_help_back(alpha, &self.gpu)?;
        }

        // Nav-spill overlay text: the segments the draw closure just
        // collected, converted cell→px with the SAME origin/cell math the
        // chrome lines use so the overlay realigns pixel-identically over
        // the row it covers. Prepared EVERY frame — an empty list is what
        // clears the overlay renderer's retained geometry (see
        // `prepare_overlay`'s doc), so no `if` around this call.
        let mut overlay_lines: Vec<crate::ui::render::text::Line> = self
            .nav_spill
            .segments()
            .iter()
            .map(|seg| crate::ui::render::text::Line {
                text: seg.text.clone(),
                x: self.canvas.origin_x() + seg.x as f32 * self.canvas.cell_w(),
                y: self.canvas.origin_y() + seg.row as f32 * self.canvas.cell_h(),
                color: seg.color,
                bold: seg.bold,
                italic: false,
                dim: seg.dim,
            })
            .collect();
        let (ox, oy, cw, ch) = (self.canvas.origin_x(), self.canvas.origin_y(), self.canvas.cell_w(), self.canvas.cell_h());
        let dialog_rect = dialog.map(|d| {
            overlay_lines.extend(d.rows.into_iter().map(|(x, y, text, style)| crate::ui::render::text::Line {
                text, x: ox + x as f32 * cw, y: oy + y as f32 * ch,
                color: crate::ui::render::cells::ratatui_color_to_rgb(style.fg),
                bold: style.add_modifier.contains(Modifier::BOLD), italic: false, dim: false,
            }));
            let p = d.panel;
            ScreenRect { x: ox + p.x as f32 * cw, y: oy + p.y as f32 * ch, w: p.width as f32 * cw, h: p.height as f32 * ch }
        });
        let fade_start = overlay_lines.len();
        overlay_lines.extend(help_overlay_lines);
        self.canvas.prepare_overlay(&self.gpu, &overlay_lines, Some((fade_start, help_opacity)))?;
        Ok((help_overlay_rect, dialog_rect, overlay_lines))
    }
}
