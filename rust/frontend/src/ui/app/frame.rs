//! One frame of the window: `State::redraw`, the sequence of the whole frame, the per-frame upkeep that
//! opens it and the lease-line acks that close it.

use super::*;

impl State {
    pub(in crate::ui) fn redraw(&mut self) -> Result<()> {
        self.frame_upkeep();
        let (preview_cells, repl_cells, repl_scrollback_cells, repl_window, llm_selection, owed, owed_line,
            owed_drawn, leaving_drawn, presentation_candidate) = self.draw_chrome()?;
        let (lines, border_rects_by_color, strip_logo_rects) = self.project_chrome()?;
        // Compute pixel rects from ratatui's cell rects, then letterbox each
        // image inside its rect.
        let cell_w = self.cell_w;
        let cell_h = self.cell_h;
        let ox = self.chrome_origin_x;
        let oy = self.chrome_origin_y;
        let cells_to_px = move |cells: ratatui::layout::Rect| ScreenRect {
            x: ox + cells.x as f32 * cell_w,
            y: oy + cells.y as f32 * cell_h,
            w: cells.width as f32 * cell_w,
            h: cells.height as f32 * cell_h,
        };
        // One pane rect for the file viewer. Priority cascade is just
        // PNG > markdown (or any text mime, rendered as markdown). The
        // concept annotation gets its own home once concept-mode-nav
        // lands — for now showing it here was overriding the actual
        // file content the user navigated to. SVG (math) also drops out
        // of the cascade by default; it comes back when inline math
        // placement is wired through the markdown buffer.
        let preview_rect = cells_to_px(preview_cells);
        let (show_fatal, show_png, show_svg, show_edit, show_md) = self.preview_shows();
        let (caption_draw, image_rect, png_rect, svg_rect) = self.layout_figure(preview_rect, show_png, show_svg);
        let md_rect = self.layout_markdown(preview_rect, cell_w, cell_h);
        self.layout_drawer_px(cells_to_px, repl_scrollback_cells, repl_window, repl_cells);
        self.rebuild_fatal_if_shown(show_fatal);
        self.layout_concept(preview_rect);
        let preview_scroll_px = self.clamp_preview_scroll(show_edit, md_rect, preview_rect);
        let media_paint_targets = self.prepare_media(show_md, md_rect, preview_scroll_px);
        let (scalebar_draw, media_paint_targets) = self.prepare_preview_text(lines, media_paint_targets,
            preview_rect, image_rect, png_rect, md_rect, preview_scroll_px, show_md, show_edit, show_fatal, &caption_draw)?;
        let (help_overlay_rect, overlay_lines) = self.prepare_overlays()?;
        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.surface.configure(&self.device, &self.config);
                self.surface.get_current_texture()?
            }
            Err(e) => return Err(e.into()),
        };

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("sot-frame"),
            });

        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear+preview+text"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.background),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            self.paint_preview_png(&mut rpass, png_rect, image_rect)?;
            self.paint_figure_bands(&mut rpass, image_rect, preview_rect, &scalebar_draw, &caption_draw, svg_rect)?;
            self.paint_monitor_chart(&mut rpass)?;
            self.paint_repl_figures(&mut rpass)?;
            self.paint_media_blocks(&mut rpass, preview_rect, media_paint_targets)?;
            self.paint_llm_selection(&mut rpass, llm_selection)?;
            self.paint_code_panels(&mut rpass, show_md, preview_rect, md_rect, preview_scroll_px)?;
            self.paint_strike_lines(&mut rpass, show_md, preview_rect, md_rect, preview_scroll_px)?;
            self.paint_brand(&mut rpass, strip_logo_rects)?;
            self.paint_chrome_text(&mut rpass, border_rects_by_color)?;
            self.paint_overlays(&mut rpass, help_overlay_rect, overlay_lines)?;
        }
        let (readback, capture_target, capture_now, selfie_target) = self.stage_frame_capture(&mut encoder, &frame);
        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        if let Some(candidate) = presentation_candidate {
            self.log_presentation(candidate);
        }
        self.acknowledge_presented_result();
        self.ack_presented_lines(owed_drawn, owed, owed_line, leaving_drawn);
        self.text.trim();
        self.finish_frame_capture(readback, capture_target, capture_now, selfie_target);
        self.frame_counter += 1;
        self.last_frame_at = Some(std::time::Instant::now());
        Ok(())
    }

    fn frame_upkeep(&mut self) {
        if self.help_peek_start_pending {
            self.help_peek_start_pending = false;
            self.help.peek = Some(help::Peek { context: self.help_context(), started: std::time::Instant::now() });
        }
        if self.help_start_pending {
            self.help_start_pending = false;
            self.open_help_drawer(self.help_context());
        }

        self.drain_events();
        // Prune finished status-change flashes; while any is still fading,
        // mark dirty so the frame loop keeps animating it (the fast-repaint
        // cadence is armed in `about_to_wait`).
        if self.prune_expired_flashes(std::time::Instant::now()) {
            self.dirty = true;
        }
        self.reflow_markdown_preview();
        self.fire_settled_cursor();
        self.run_harness_one_shots();
        self.walk_start_path();
    }

    fn reflow_markdown_preview(&mut self) {
        // Coalesced reflow: one MathRendered (or a burst) sets
        // needs_md_reflow; we rebuild preview_md here so the walk pulls
        // the freshly-cached SVG dims when sizing per-block placeholders.
        // Markdown-only by construction — non-markdown previews don't go
        // through the math walk.
        if self.needs_md_reflow {
            self.needs_md_reflow = false;
            if let Some((mime, bytes)) = self.preview_src.clone() {
                if mime == "text/markdown" || mime == "text/x-markdown" {
                    self.render_preview_source(&mime, &bytes);
                }
            }
        }
    }

    fn fire_settled_cursor(&mut self) {
        // Debounce nav-driven backend round-trips on cursor-settle. User-
        // reported: hold-to-scroll generated hundreds of `preview.get` /
        // `concept.read` / `file.parse` requests per second, saturating
        // the SSH tunnel and pushing wgpu through enough rapid-fire
        // preview blob rasterisation that the AMD driver overlay fired.
        // Cascade: tunnel saturation → transport reconnect → hello-time
        // `tree.root` re-fire → cursor reset to row 0. The fires below
        // are the *only* path that ships per-row backend traffic;
        // suppressing them until the cursor sits still for
        // `NAV_FIRE_DEBOUNCE` makes hold-to-scroll free.
        let cursor_now = (self.mode, self.tree.selected);
        if self.last_cursor_pos != Some(cursor_now) {
            self.last_cursor_pos = Some(cursor_now);
            self.cursor_moved_at = Some(std::time::Instant::now());
        }
        let debouncing = self
            .cursor_moved_at
            .map(|t| t.elapsed() < NAV_FIRE_DEBOUNCE)
            .unwrap_or(false);
        if debouncing {
            // Mark dirty so `about_to_wait` reschedules a redraw at the
            // frame boundary; on each subsequent redraw the elapsed
            // check passes once the user settles, then the fires go
            // through. ~10 cheap no-op redraws per settle, which is
            // dwarfed by the per-row backend traffic we're skipping.
            self.dirty = true;
        } else {
            self.cursor_moved_at = None;
            self.maybe_fire_concept_read();
            self.maybe_fire_preview();
        }
    }

    fn run_harness_one_shots(&mut self) {
        // Drive `--auto-expand` exactly once, after the initial selection
        // has been applied (i.e., the first TreeRoot landed).
        // We clear the flag whether or not the expansion request actually
        // queued — a no-op row (leaf or already expanded) doesn't deserve
        // a retry loop.
        if self.pending_auto_expand
            && self.pending_initial_selection.is_none()
            && !self.tree.rows.is_empty()
        {
            self.try_expand_selected();
            self.pending_auto_expand = false;
        }
        // `--auto-pin`: drive C2 toggle once the cursor selection has
        // landed. Same gating as `--auto-expand`. Pinning a row whose
        // id doesn't start with `files:` is a `toggle_pin` no-op; the
        // flag still clears so we don't churn.
        if self.pending_auto_pin
            && self.pending_initial_selection.is_none()
            && !self.tree.rows.is_empty()
        {
            self.toggle_pin();
            self.pending_auto_pin = false;
        }
        // `--demo-repl-eval`: one-shot self-submit once the workspace is
        // live (same gating as the other harness one-shots). Goes through
        // submit_repl_input so a repl_log entry exists for the frames to
        // land in, then shows the REPL drawer so the capture includes it.
        if self.pending_demo_repl_eval.is_some()
            && self.pending_initial_selection.is_none()
            && !self.tree.rows.is_empty()
        {
            if let Some(code) = self.pending_demo_repl_eval.take() {
                self.repl_input = code;
                self.submit_repl_input();
                if self.drawer != DrawerContent::Repl {
                    self.drawer = DrawerContent::Repl;
                }
            }
        }
        // `--demo-function-methods` chain: once the target function row
        // appears in the tree (after the col-2 splice has landed),
        // position cursor on it and fire the methods request. Single-fire
        // by clearing the pending tuple.
        if let Some((module, name)) = self.pending_demo_function_methods.clone() {
            let target_id = format!("modules:{module}:{name}");
            if let Some(idx) = self.tree.rows.iter().position(|r| r.node.id == target_id) {
                self.tree.selected = idx;
                self.try_expand_selected();
                self.pending_demo_function_methods = None;
            }
        }
    }

    fn walk_start_path(&mut self) {
        // `--start-path` walk (files mode): land the cursor on the target
        // file, expanding one collapsed ancestor directory per tree update
        // on the way down. Once the cursor is on the file row, the normal
        // cursor-tracking passes above (concept.read / preview / file.parse)
        // fire exactly as they would for a user host-2 — which is the
        // point: `--capture-preview` only fires preview.get, but the
        // concept panel and drift badge key off the cursored row.
        if let Some(path) = self.pending_start_path.clone() {
            let target_id = format!("files:{path}");
            if let Some(idx) = self.tree.rows.iter().position(|r| r.node.id == target_id) {
                self.tree.selected = idx;
                self.pending_start_path = None;
                self.start_path_fired = None;
            } else {
                // Deepest ancestor directory that exists in the tree but is
                // still collapsed. (Ancestors appear top-down, so the last
                // match is the frontier of the walk.)
                let mut prefix = String::new();
                let mut frontier: Option<usize> = None;
                for seg in path.split('/') {
                    if !prefix.is_empty() {
                        prefix.push('/');
                    }
                    prefix.push_str(seg);
                    if prefix == path {
                        break; // the file itself is handled above
                    }
                    let anc_id = format!("files:{prefix}");
                    if let Some(idx) = self
                        .tree
                        .rows
                        .iter()
                        .position(|r| r.node.id == anc_id && !r.expanded)
                    {
                        frontier = Some(idx);
                    }
                }
                if let Some(idx) = frontier {
                    let anc_id = self.tree.rows[idx].node.id.clone();
                    // Fire once per frontier; `expanded` flips only when the
                    // children splice lands, so gate re-fires on the memo.
                    if self.start_path_fired.as_deref() != Some(anc_id.as_str()) {
                        self.tree.selected = idx;
                        if self.try_expand_selected() {
                            self.start_path_fired = Some(anc_id);
                        } else {
                            // Not expandable (leaf / no children): the path
                            // can't be reached — stop walking rather than
                            // retry every redraw.
                            tracing::warn!(%path, %anc_id, "--start-path dead end — ancestor not expandable");
                            self.pending_start_path = None;
                            self.start_path_fired = None;
                        }
                    }
                }
                // No ancestor row yet (root still loading): stay pending;
                // the next tree update re-enters this block.
            }
        }
    }

    fn ack_presented_lines(
        &mut self,
        owed_drawn: bool,
        owed: Vec<(HostKey, u32)>,
        owed_line: Option<String>,
        leaving_drawn: bool,
    ) {
        if owed_drawn {
            for (k, n) in &owed {
                self.leases.notice_seen(k, *n);
            }
            if self.leaving.is_none() {
                self.not_ended_shown = owed_line.map(|l| (l, std::time::Instant::now() + NOTIFY_STICKY));
            }
        }
        // The leaving line holds from the frame that presents it, and only
        // then are its counts acked.
        if leaving_drawn {
            if let Some(l) = self.leaving.as_mut() {
                for (h, n) in l.presented(std::time::Instant::now()) {
                    self.leases.notice_seen(&h, n);
                }
            }
        }
    }
}
