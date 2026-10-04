//! The render pass's pane and chrome draws: the monitor chart, the REPL figures, the selection, the brand,
//! the chrome text and the nav-spill overlay.

use super::*;

impl State {
    pub(in crate::ui) fn paint_monitor_chart(&self, mut rpass: &mut wgpu::RenderPass<'_>) -> Result<()> {
        // Ctrl+M monitor chart: paint the rasterised SVG quad over the
        // drawer rect (ADR 0020), reusing the same resvg→wgpu-quad path as
        // the math SVG above.
        if self.drawer == DrawerContent::Monitor {
            if let Some(q) = self.monitor_quad.as_ref() {
                q.render(
                    &self.queue,
                    &self.quad_pipeline,
                    &mut rpass,
                    self.monitor_rect_px,
                    (self.config.width, self.config.height),
                )?;
            }
        }
        Ok(())
    }

    pub(in crate::ui) fn paint_repl_figures(&self, mut rpass: &mut wgpu::RenderPass<'_>) -> Result<()> {
        // Inline REPL figures: paint each visible slot's quad over its
        // reserved scrollback rows. Scissored to the scrollback rect so a
        // partially-scrolled figure clips at the drawer edges instead of
        // bleeding over the input line or pane borders.
        if self.drawer == DrawerContent::Repl && !self.repl_image_slots.is_empty() {
            let area = self.repl_scrollback_px;
            let (win_start, win_end) = self.repl_window;
            let sw = (area.w.max(0.0) as u32).min(self.config.width);
            let sh = (area.h.max(0.0) as u32).min(self.config.height);
            if sw > 0 && sh > 0 {
                let sx = (area.x.max(0.0) as u32).min(self.config.width - 1);
                let sy = (area.y.max(0.0) as u32).min(self.config.height - 1);
                let sw = sw.min(self.config.width - sx);
                let sh = sh.min(self.config.height - sy);
                let mut painted = false;
                for slot in &self.repl_image_slots {
                    if slot.line + slot.rows as usize <= win_start || slot.line >= win_end {
                        continue;
                    }
                    let Some(img) = self.repl_images.get(&slot.key) else {
                        continue;
                    };
                    if !painted {
                        rpass.set_scissor_rect(sx, sy, sw, sh);
                        painted = true;
                    }
                    let rect = ScreenRect {
                        x: area.x + self.cell_w,
                        y: area.y + (slot.line as f32 - win_start as f32) * self.cell_h,
                        w: slot.disp_w,
                        h: slot.disp_h,
                    };
                    img.quad.render(
                        &self.queue,
                        &self.quad_pipeline,
                        &mut rpass,
                        rect,
                        (self.config.width, self.config.height),
                    )?;
                }
                if painted {
                    rpass.set_scissor_rect(0, 0, self.config.width, self.config.height);
                }
            }
        }
        Ok(())
    }

    pub(in crate::ui) fn paint_llm_selection(
        &mut self,
        mut rpass: &mut wgpu::RenderPass<'_>,
        llm_selection: Option<((u16, u16), (u16, u16))>,
    ) -> Result<()> {
        // LLM-pane selection highlight — render a pastel-yellow rect
        // for each row of the selection before text.render so glyphs
        // sit on top. Per-row geometry: the first and last rows may
        // be partial (start_col..pane_cols and 0..=end_col); middle
        // rows are full-width.
        if let Some(sel) = llm_selection {
            let (a, b) = sel;
            let (start, end) = if a <= b { (a, b) } else { (b, a) };
            let (sr, sc) = start;
            let (er, ec) = end;
            let pane = self.pane_rects.llm;
            if pane.width > 0 && pane.height > 0 {
                let origin_x = self.chrome_origin_x + pane.x as f32 * self.cell_w;
                let origin_y = self.chrome_origin_y + pane.y as f32 * self.cell_h;
                let max_row = pane.height.saturating_sub(1);
                let max_col = pane.width.saturating_sub(1);
                let sr = sr.min(max_row);
                let er = er.min(max_row);
                let sc = sc.min(max_col);
                let ec = ec.min(max_col);
                // Batched: a per-row `render()` loop rewrites the same
                // vbuf inside one render pass, so only the LAST row's
                // highlight ever reached the GPU — a multiline drag
                // looked like single-line selection (the copy walked
                // the real range all along). Same fix as the markdown
                // code-bg panels.
                let mut row_rects: Vec<ScreenRect> = Vec::new();
                for row in sr..=er {
                    let cs = if row == sr { sc } else { 0 };
                    let ce = if row == er { ec } else { max_col };
                    if ce < cs {
                        continue;
                    }
                    let span = (ce - cs + 1) as f32;
                    row_rects.push(ScreenRect {
                        x: origin_x + cs as f32 * self.cell_w,
                        y: origin_y + row as f32 * self.cell_h,
                        w: span * self.cell_w,
                        h: self.cell_h,
                    });
                }
                if !row_rects.is_empty() {
                    self.selection_bg_quad.render_many(
                        &self.device,
                        &self.queue,
                        &self.quad_pipeline,
                        &mut rpass,
                        &row_rects,
                        (self.config.width, self.config.height),
                    )?;
                }
            }
        }
        Ok(())
    }

    pub(in crate::ui) fn paint_brand(&mut self, mut rpass: &mut wgpu::RenderPass<'_>, strip_logo_rects: Vec<ScreenRect>) -> Result<()> {
        // Brand chrome: the dark logo at each ship's bow in the strip, plus
        // the wordmark at the top-right of the nav pane. Drawn just before
        // the text layer so any glyphs (e.g. the nav title) stay legible on
        // top. Cosmetic — each is skipped when its quad failed to decode
        // (field is None).
        //
        // Bow wheels via render_many, NOT a render() loop: render()
        // rewrites the quad's vbuf at offset 0, so a per-rect loop in one
        // pass leaves only the LAST rect on the GPU (see Quad::render_many's
        // own docstring) — that bug showed exactly one logo at the right end.
        if !strip_logo_rects.is_empty() {
            // Copy out before the &mut borrow of logo_quad below.
            let wheel_angle = self.wheel_angle;
            if let Some((quad, _, _)) = self.logo_quad.as_mut() {
                quad.render_many_rotated(
                    &self.device,
                    &self.queue,
                    &self.quad_pipeline,
                    &mut rpass,
                    &strip_logo_rects,
                    wheel_angle,
                    (self.config.width, self.config.height),
                )?;
            }
        }
        // Wordmark — the nav pane's FIRST row, left-aligned (owner ruling
        // 2026-09-06). The tree body starts on the row below (`nav_rect` in
        // the draw closure is carved by the same `wordmark_quad.is_some()`),
        // so nothing is ever drawn under it. One row tall; width follows the
        // PNG's aspect, clamped to the pane's width minus a cell each side.
        if let Some((quad, nw, nh)) = self.wordmark_quad.as_ref() {
            let nav = self.pane_rects.nav;
            let nav_x = self.chrome_origin_x + nav.x as f32 * self.cell_w;
            let nav_y = self.chrome_origin_y + nav.y as f32 * self.cell_h;
            let nav_w = nav.width as f32 * self.cell_w;
            let mut wm_h = self.cell_h;
            let mut wm_w = wm_h * (*nw as f32 / (*nh).max(1) as f32);
            let max_w = (nav_w - 2.0 * self.cell_w).max(1.0);
            if wm_w > max_w {
                wm_w = max_w;
                wm_h = wm_w * (*nh as f32 / (*nw).max(1) as f32);
            }
            quad.render(
                &self.queue,
                &self.quad_pipeline,
                &mut rpass,
                ScreenRect {
                    x: nav_x + self.cell_w,
                    y: nav_y + (self.cell_h - wm_h) * 0.5,
                    w: wm_w,
                    h: wm_h,
                },
                (self.config.width, self.config.height),
            )?;
        }
        Ok(())
    }

    pub(in crate::ui) fn paint_chrome_text(
        &mut self,
        mut rpass: &mut wgpu::RenderPass<'_>,
        border_rects_by_color: HashMap<(u8, u8, u8), Vec<ScreenRect>>,
    ) -> Result<()> {
        // Pane-border quads — one batched render per colour, drawn just
        // before text.render so glyphs stay legible on top. The rects come
        // from chrome::project_border_quads (arm-from-centre, gap-free
        // tiling) computed above with the same origin/scale as the chrome
        // text. `iter_mut` yields disjoint &mut Quad, so the whole map is a
        // single mutable borrow for the pass (no per-entry borrow conflict
        // like two render_many calls on one field would hit), while
        // &self.device/queue/quad_pipeline stay separate fields — the same
        // disjoint-field pattern as the code_bg / strike passes.
        if !border_rects_by_color.is_empty() {
            for (color, quad) in self.border_quads.iter_mut() {
                if let Some(rects) = border_rects_by_color.get(color) {
                    quad.render_many(
                        &self.device,
                        &self.queue,
                        &self.quad_pipeline,
                        &mut rpass,
                        rects,
                        (self.config.width, self.config.height),
                    )?;
                }
            }
        }

        self.text.render(&mut rpass)?;
        Ok(())
    }

    pub(in crate::ui) fn paint_overlays(
        &mut self,
        mut rpass: &mut wgpu::RenderPass<'_>,
        help_overlay_rect: Option<ScreenRect>,
        overlay_lines: Vec<crate::ui::render::text::Line>,
    ) -> Result<()> {
        // Nav-spill overlay — the ONLY draws above the main text pass.
        // Backing strips first (near-opaque surface navy, one cell row
        // tall, from the nav left edge across the border cell to the
        // spilled text's end + 1 cell pad), then the overlay glyphs on
        // top via the overlay text layer. Draw order inside the pass
        // is the z-order: these cover preview images AND main-pass
        // glyphs, which is exactly what "floating over the preview"
        // means. Full-surface scissor — the segments were reach-capped
        // at collection time.
        if !self.nav_spill_segments.is_empty() {
            rpass.set_scissor_rect(0, 0, self.config.width, self.config.height);
            let strip_rects: Vec<ScreenRect> = self
                .nav_spill_segments
                .iter()
                .map(|seg| ScreenRect {
                    x: self.chrome_origin_x + seg.x as f32 * self.cell_w,
                    y: self.chrome_origin_y + seg.row as f32 * self.cell_h,
                    w: (seg.width_cells + 1) as f32 * self.cell_w,
                    h: self.cell_h,
                })
                .collect();
            self.overlay_back_quad.render_many(
                &self.device,
                &self.queue,
                &self.quad_pipeline,
                &mut rpass,
                &strip_rects,
                (self.config.width, self.config.height),
            )?;
        }
        if let (Some(rect), Some((_, quad))) = (help_overlay_rect, self.help_back_quad.as_mut()) {
            rpass.set_scissor_rect(0, 0, self.config.width, self.config.height);
            quad.render(&self.queue, &self.quad_pipeline, &mut rpass,
                rect, (self.config.width, self.config.height))?;
        }
        if !overlay_lines.is_empty() {
            rpass.set_scissor_rect(0, 0, self.config.width, self.config.height);
            self.text.render_overlay(&mut rpass)?;
        }
        Ok(())
    }
}
