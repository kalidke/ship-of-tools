//! The render pass's preview draws: the image canvas, the figure bands, the media blocks, the code panels and the
//! strike lines.

use super::*;
use crate::ui::render::brushes::Solid;

const BLOCK_PAD_Y: f32 = 4.0;

impl State {
    pub(in crate::ui) fn paint_preview_png(
        &mut self,
        mut rpass: &mut wgpu::RenderPass<'_>,
        png_rect: Option<ScreenRect>,
        image_rect: ScreenRect,
    ) -> Result<()> {
        // Preview-layer goes under chrome text so borders and labels stay
        // legible above whatever the preview is.
        self.raster.clear_roi();
        let row_key = self.active_result_row_key();
        if let (true, Some(letterbox_rect)) = (self.raster.is_shown(), png_rect) {
            let aim = self.preview_requests.take_ready_roi_aim(row_key.as_ref());
            let fired = self.preview_requests.fired_target();
            let Some(frame) = self.raster.frame(image_rect, letterbox_rect, aim, fired) else {
                return Ok(());
            };
            let canvas_rect = frame.rect;
            // ADR 0022: stash the visible ROI in source-image px so the
            // `C` hotkey / `capture_roi` fe-command and fe-state.json know
            // what's on screen. Image files only — a PDF page's source is
            // the `.pdf`, which `image.crop` can't decode (v2).
            let new_roi: Option<PreviewRoi> =
                self.preview_requests.fired_target().and_then(|nid| {
                    if !Self::is_image_node_id(nid) {
                        return None;
                    }
                    let (src_w, src_h) = frame.src;
                    let (x, y, w, h) = visible_roi_px(
                        canvas_rect.x,
                        canvas_rect.y,
                        canvas_rect.w,
                        canvas_rect.h,
                        image_rect.x,
                        image_rect.y,
                        image_rect.w,
                        image_rect.h,
                        src_w,
                        src_h,
                    )?;
                    Some(PreviewRoi {
                        node_id: nid.to_string(),
                        path: self.backend_abs_path(nid),
                        x,
                        y,
                        w,
                        h,
                        src_w,
                        src_h,
                        zoom: frame.zoom,
                    })
                });
            self.raster.record_roi(new_roi);
            // ADR 0025: echo the effective (post-clamp) rect for a just-
            // applied `--roi` aim — `fe.command.send` is fire-and-forget,
            // so the ack couldn't carry it (2026-07-21 ADR update).
            if let Some(aim) = frame.aim {
                match self.raster.roi() {
                    Some(eff) => self.emit_preview_roi_applied(&aim, eff),
                    // Raster but not a croppable image node (e.g. a PDF
                    // page): no source-px frame to report against.
                    None => tracing::warn!(node_id = %aim.node_id,
                        "preview --roi: no source-px ROI for this preview — no roi_applied echo"),
                }
            }
            // Scissor to the IMAGE rect, not the pane: this is what stops a
            // zoomed/panned canvas from painting over the reserved caption
            // band at the bottom.
            let sx = image_rect.x.max(0.0) as u32;
            let sy = image_rect.y.max(0.0) as u32;
            let sw = image_rect.w.max(0.0) as u32;
            let sh = image_rect.h.max(0.0) as u32;
            rpass.set_scissor_rect(sx, sy, sw, sh);
            if let Some(quad) = self.raster.quad() {
                quad.render(
                    self.gpu.queue(),
                    self.brushes.pipeline(),
                    &mut rpass,
                    canvas_rect,
                    (self.gpu.config().width, self.gpu.config().height),
                )?;
            }
            // Reset scissor so subsequent draws (SVG, media paint,
            // chrome text) aren't clipped to the preview pane.
            rpass.set_scissor_rect(0, 0, self.gpu.config().width, self.gpu.config().height);
        }
        Ok(())
    }

    pub(in crate::ui) fn paint_figure_bands(
        &mut self,
        mut rpass: &mut wgpu::RenderPass<'_>,
        image_rect: ScreenRect,
        preview_rect: ScreenRect,
        scalebar_draw: &Option<crate::ui::preview::image::overlay::ScalebarDraw>,
        caption_draw: &Option<crate::ui::preview::image::overlay::CaptionDraw>,
        svg_rect: Option<ScreenRect>,
    ) -> Result<()> {
        // ADR 0034: dynamic scalebar overlay, drawn after the image quad so
        // it sits on top, re-scissored to the pane so it can't bleed. Black
        // backing box under a white bar (the label rides in `extras`).
        // Dedicated quad fields, not the shared `border_quads` map: each
        // `render_many` mutably borrows its quad for the whole render-pass
        // lifetime (`'a`), so two colours must come from two disjoint
        // fields — one map borrowed twice would alias. The raster's
        // borrow ended when the image block closed.
        if let Some(sb) = scalebar_draw.as_ref() {
            let sx = image_rect.x.max(0.0) as u32;
            let sy = image_rect.y.max(0.0) as u32;
            let sw = image_rect.w.max(0.0) as u32;
            let sh = image_rect.h.max(0.0) as u32;
            rpass.set_scissor_rect(sx, sy, sw, sh);
            self.brushes.paint(Solid::ScalebarBack, &self.gpu, &mut rpass, std::slice::from_ref(&sb.backing))?;
            self.brushes.paint(Solid::ScalebarBar, &self.gpu, &mut rpass, std::slice::from_ref(&sb.bar))?;
            rpass.set_scissor_rect(0, 0, self.gpu.config().width, self.gpu.config().height);
        }
        // Figure-caption band. Its own quad field for the aliasing reason
        // documented on the scalebar above. Scissored to the FULL pane
        // (`preview_rect`) — the band lives in the strip `image_rect`
        // deliberately gave up, which is outside the image scissor.
        if let Some(cap) = caption_draw.as_ref() {
            let sx = preview_rect.x.max(0.0) as u32;
            let sy = preview_rect.y.max(0.0) as u32;
            let sw = preview_rect.w.max(0.0) as u32;
            let sh = preview_rect.h.max(0.0) as u32;
            rpass.set_scissor_rect(sx, sy, sw, sh);
            self.brushes.paint(Solid::CaptionBack, &self.gpu, &mut rpass, std::slice::from_ref(&cap.backing))?;
            rpass.set_scissor_rect(0, 0, self.gpu.config().width, self.gpu.config().height);
        }
        if let (Some(quad), Some(rect)) = (self.shaped.svg(), svg_rect) {
            quad.render(
                self.gpu.queue(),
                self.brushes.pipeline(),
                &mut rpass,
                rect,
                (self.gpu.config().width, self.gpu.config().height),
            )?;
        }
        Ok(())
    }

    pub(in crate::ui) fn paint_media_blocks(&self, mut rpass: &mut wgpu::RenderPass<'_>, preview_rect: ScreenRect,
                          media_paint_targets: Vec<(usize, ScreenRect)>) -> Result<()> {
        // Media paint: math SVGs and figures share this pass since
        // they both ride FFFC placeholders. Paint after the file
        // preview so the bitmap sits on top of any background
        // tinting, and BEFORE text so the FFFC placeholder glyph
        // (and any default-font visual artefact) gets covered.
        //
        // `rect` here is the row reservation (full preview width by
        // the FFFC line's reserved height) for display math and
        // figures; an inline-math rect is already sized + anchored
        // to the FFFC glyph. The bitmap was sized to natural aspect
        // at rasterise time; centre it inside the rect rather than
        // stretching non-uniformly.
        //
        // Scissor the whole pass to the preview pane: a figure (or a
        // tall display-math block) whose reservation straddles the
        // pane's bottom edge would otherwise paint its lower half
        // straight into the terminal drawer below. The code-block
        // panels solve the same bleed by rect-CLAMPING (a solid quad
        // clamps cleanly); a textured image can't — clamping the dest
        // rect squashes the bitmap — so it gets the same wgpu scissor
        // the PNG canvas path uses, then reset to full-frame after.
        {
            let sx = preview_rect.x.max(0.0) as u32;
            let sy = preview_rect.y.max(0.0) as u32;
            let sw = preview_rect.w.max(0.0) as u32;
            let sh = preview_rect.h.max(0.0) as u32;
            rpass.set_scissor_rect(sx, sy, sw, sh);
        }
        for (block_idx, rect) in &media_paint_targets {
            let Some(block) = self.shaped.md().media_blocks.get(*block_idx) else {
                continue;
            };
            // Resolve the kind-specific source quad + the paint
            // size policy. Math SVGs are pre-rasterised at a size
            // that already fits the row reservation (uniform
            // downscale done at rasterise time), so the paint pass
            // just centres them inside the rect. Figures are
            // cached at natural pixel size and might exceed the
            // row reservation in either dimension; uniform-scale
            // them to fit on the paint side.
            let (quad, paint_w, paint_h): (&Quad, f32, f32) = match block {
                crate::ui::preview::markdown::MediaBlock::Math { latex, display } => {
                    let key = (latex.clone(), *display);
                    let Some(entry) = self.md_media.math(&key) else {
                        continue;
                    };
                    let Some(q) = entry.rasterised.as_ref() else {
                        continue;
                    };
                    let (pw, ph) = q.size_px;
                    let pw_f = (pw as f32).min(rect.w);
                    let ph_f = (ph as f32).min(rect.h);
                    (q, pw_f, ph_f)
                }
                crate::ui::preview::markdown::MediaBlock::Figure { url, .. } => {
                    let Some(entry) = self.figures.get(url) else {
                        continue;
                    };
                    let pw = entry.natural_w_px as f32;
                    let ph = entry.natural_h_px as f32;
                    let s = (rect.w / pw.max(1.0))
                        .min(rect.h / ph.max(1.0))
                        .min(1.0)
                        .max(0.001);
                    (&entry.quad, pw * s, ph * s)
                }
                // Tables paint via the extras text path, not the
                // quad pipeline — buffer is built before
                // text.prepare, hosted in `extras`, with TextBounds
                // clipping the overflow to the preview pane and
                // the table scroll shifting the text left for
                // horizontal scroll. Nothing to do in this loop.
                crate::ui::preview::markdown::MediaBlock::Table { .. } => continue,
            };
            // Cull rects that fall completely outside the preview
            // viewport — avoids spending pixels on offscreen media.
            if rect.y + rect.h < preview_rect.y || rect.y > preview_rect.y + preview_rect.h {
                continue;
            }
            let paint_rect = ScreenRect {
                x: rect.x + ((rect.w - paint_w) * 0.5).max(0.0),
                y: rect.y + ((rect.h - paint_h) * 0.5).max(0.0),
                w: paint_w,
                h: paint_h,
            };
            quad.render(
                self.gpu.queue(),
                self.brushes.pipeline(),
                &mut rpass,
                paint_rect,
                (self.gpu.config().width, self.gpu.config().height),
            )?;
        }
        // Reset scissor so subsequent draws (code panels, strike
        // lines, chrome text) aren't clipped to the preview pane.
        rpass.set_scissor_rect(0, 0, self.gpu.config().width, self.gpu.config().height);
        Ok(())
    }

    pub(in crate::ui) fn paint_code_panels(&mut self, mut rpass: &mut wgpu::RenderPass<'_>, show_md: bool, preview_rect: ScreenRect,
                         md_rect: ScreenRect, preview_scroll_px: f32) -> Result<()> {
        // Markdown code-bg panel — paint the slate quad behind
        // every contiguous code-glyph run the walk tagged with
        // CODE_GLYPH_META. Rects come back in buffer-local coords;
        // we add the markdown pane origin + EXTRA_TOP_PAD_PX (the
        // same headroom the text gets) and subtract the scroll so
        // the panel rides with the text on wheel. Clipped to the
        // preview rect so a code line that's just scrolled past
        // doesn't bleed into the wireframe.
        // Single batched render for both block panels (full pane
        // width) and inline pills (text width + padding). Combined
        // because each `render_many` rewrites its quad's vertex buffer,
        // so two paints with one brush in one pass would leave only the
        // second; one merged Vec sidesteps it.
        if show_md {
            // One rect per fenced block, spanning from the first
            // line's top to the last line's bottom — covers blank
            // lines inside the fence that `code_block_line_rects`
            // skipped, so the panel reads as one continuous strip
            // rather than per-line stripes with gaps.
            let block_rects = self.shaped.md().code_block_rects();
            let inline_rects = self.shaped.md().code_glyph_rects();
            if !block_rects.is_empty() || !inline_rects.is_empty() {
                const PAD_X: f32 = 3.0;
                const PAD_Y: f32 = 1.0;
                let pane_top = preview_rect.y;
                let pane_bot = preview_rect.y + preview_rect.h;
                let pane_left = md_rect.x;
                let pane_right = md_rect.x + md_rect.w;
                let mut batched: Vec<ScreenRect> =
                    Vec::with_capacity(block_rects.len() + inline_rects.len());
                // Block panels first — full pane width per block, no
                // x-padding; the line already covers the gutter.
                for (by, bh) in block_rects {
                    let sy = md_rect.y + crate::ui::render::text::EXTRA_TOP_PAD_PX + by
                        - preview_scroll_px
                        - BLOCK_PAD_Y;
                    let sh = bh + 2.0 * BLOCK_PAD_Y;
                    // Clamp the panel to the preview pane's visible band,
                    // not just cull: a block taller than the pane (a long
                    // HDF5 tree, say) must stop at the drawer top
                    // (`pane_bot`) instead of bleeding down into the
                    // drawer, and at `pane_top` when scrolled up.
                    let top = sy.max(pane_top);
                    let bot = (sy + sh).min(pane_bot);
                    if bot <= top {
                        continue;
                    }
                    batched.push(ScreenRect {
                        x: md_rect.x,
                        y: top,
                        w: md_rect.w,
                        h: bot - top,
                    });
                }
                // Inline pills — text-width + small padding. Skipped
                // for any glyph also tagged CODE_BLOCK_FLAG (the
                // walker filters those out).
                for (bx, by, bw, bh) in inline_rects {
                    let sy = md_rect.y + crate::ui::render::text::EXTRA_TOP_PAD_PX + by
                        - preview_scroll_px
                        - PAD_Y;
                    let sh = bh + 2.0 * PAD_Y;
                    if sy + sh < pane_top || sy > pane_bot {
                        continue;
                    }
                    let raw_x = md_rect.x + bx - PAD_X;
                    let raw_w = bw + 2.0 * PAD_X;
                    let sx = raw_x.max(pane_left);
                    let sw = (raw_x + raw_w).min(pane_right) - sx;
                    if sw <= 0.0 {
                        continue;
                    }
                    batched.push(ScreenRect {
                        x: sx,
                        y: sy,
                        w: sw,
                        h: sh,
                    });
                }
                if !batched.is_empty() {
                    self.brushes.paint(Solid::CodeBg, &self.gpu, &mut rpass, &batched)?;
                }
                self.paint_code_borders(rpass, md_rect, preview_scroll_px, pane_top, pane_bot)?;
            }
        }
        Ok(())
    }

    fn paint_code_borders(
        &mut self,
        mut rpass: &mut wgpu::RenderPass<'_>,
        md_rect: ScreenRect,
        preview_scroll_px: f32,
        pane_top: f32,
        pane_bot: f32,
    ) -> Result<()> {
                // Per-block 1-px border around the slate panel —
                // top / bottom / left / right edges. A different
                // brush from the code background, so a second
                // paint in this pass is fine (the vertex buffer
                // is per brush, not per pass).
                // Inline pills don't get bordered; the visual
                // affordance is only useful at panel scale.
                // Clamped panel rect + whether the real top / bottom edge
                // falls inside the pane. When a panel is clipped at the
                // drawer top (or pane top on scroll), we suppress the edge
                // at the clip line so there's no false border drawn across
                // the drawer boundary.
                let block_panels: Vec<(ScreenRect, bool, bool)> = self
                    .shaped
                    .md()
                    .code_block_rects()
                    .into_iter()
                    .filter_map(|(by, bh)| {
                        let sy = md_rect.y + crate::ui::render::text::EXTRA_TOP_PAD_PX + by
                            - preview_scroll_px
                            - BLOCK_PAD_Y;
                        let sh = bh + 2.0 * BLOCK_PAD_Y;
                        let top = sy.max(pane_top);
                        let bot = (sy + sh).min(pane_bot);
                        if bot <= top {
                            return None;
                        }
                        let top_visible = sy >= pane_top;
                        let bot_visible = sy + sh <= pane_bot;
                        Some((
                            ScreenRect {
                                x: md_rect.x,
                                y: top,
                                w: md_rect.w,
                                h: bot - top,
                            },
                            top_visible,
                            bot_visible,
                        ))
                    })
                    .collect();
                if !block_panels.is_empty() {
                    const BORDER: f32 = 1.0;
                    let mut edges: Vec<ScreenRect> = Vec::with_capacity(block_panels.len() * 4);
                    for (r, top_visible, bot_visible) in &block_panels {
                        // Top edge — only if the real top is in-pane.
                        if *top_visible {
                            edges.push(ScreenRect {
                                x: r.x,
                                y: r.y,
                                w: r.w,
                                h: BORDER,
                            });
                        }
                        // Bottom edge — only if the real bottom is in-pane
                        // (else it'd draw a false line at the drawer top).
                        if *bot_visible {
                            edges.push(ScreenRect {
                                x: r.x,
                                y: r.y + r.h - BORDER,
                                w: r.w,
                                h: BORDER,
                            });
                        }
                        // Left / right edges span the clamped visible
                        // height (corner overlap with top/bottom is the
                        // same colour, so harmless).
                        edges.push(ScreenRect {
                            x: r.x,
                            y: r.y,
                            w: BORDER,
                            h: r.h,
                        });
                        edges.push(ScreenRect {
                            x: r.x + r.w - BORDER,
                            y: r.y,
                            w: BORDER,
                            h: r.h,
                        });
                    }
                    self.brushes.paint(Solid::CodeBorder, &self.gpu, &mut rpass, &edges)?;
                }
        Ok(())
    }

    pub(in crate::ui) fn paint_strike_lines(
        &mut self,
        mut rpass: &mut wgpu::RenderPass<'_>,
        show_md: bool,
        preview_rect: ScreenRect,
        md_rect: ScreenRect,
        preview_scroll_px: f32,
    ) -> Result<()> {
        // Markdown strikethrough — thin horizontal quad at the
        // line's x-height midline for every STRIKE_GLYPH_FLAG run.
        // Replaces the combining-mark fallback that rasterised
        // inconsistently across font picks.
        if show_md {
            let rects = self.shaped.md().strike_glyph_rects();
            if !rects.is_empty() {
                const STRIKE_THICKNESS: f32 = 1.5;
                let pane_top = preview_rect.y;
                let pane_bot = preview_rect.y + preview_rect.h;
                let pane_left = md_rect.x;
                let pane_right = md_rect.x + md_rect.w;
                let mut batched: Vec<ScreenRect> = Vec::with_capacity(rects.len());
                for (bx, by, bw, bh) in rects {
                    // Mid-x-height is ≈ 55% down from line_top for a
                    // single-size run; close enough for the heading
                    // / paragraph mix the preview shows.
                    let line_y = md_rect.y + crate::ui::render::text::EXTRA_TOP_PAD_PX + by
                        - preview_scroll_px
                        + bh * 0.55
                        - STRIKE_THICKNESS * 0.5;
                    if line_y + STRIKE_THICKNESS < pane_top || line_y > pane_bot {
                        continue;
                    }
                    let raw_x = md_rect.x + bx;
                    let sx = raw_x.max(pane_left);
                    let sw = (raw_x + bw).min(pane_right) - sx;
                    if sw <= 0.0 {
                        continue;
                    }
                    batched.push(ScreenRect {
                        x: sx,
                        y: line_y,
                        w: sw,
                        h: STRIKE_THICKNESS,
                    });
                }
                self.brushes.paint(Solid::StrikeLine, &self.gpu, &mut rpass, &batched)?;
            }
        }
        Ok(())
    }
}
