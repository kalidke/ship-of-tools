//! The strip's frame: `draw_session_strip` lays the labels and ships out and eases the scroll.

use super::*;

impl State {
    pub(in crate::ui) fn draw_session_strip(
        &mut self,
        lines: &mut Vec<crate::text::Line>,
        border_rects_by_color: &mut HashMap<(u8, u8, u8), Vec<ScreenRect>>,
    ) -> Result<Vec<ScreenRect>> {
        // Bottom session strip (floating overlay): all sessions laid out
        // horizontally, the active one centered + bold, neighbours dimmed to
        // either side. `strip_scroll_px` eases toward the active session's
        // strip-local center so a switch (Shift+←→ → cycle_workspace)
        // slides the strip macOS-style. Drawn here, before `extras` borrow
        // self, so the ease can mutate self.* without a borrow conflict.
        //
        // Each ship's bow wheel is accumulated here (physical px) and drawn
        // from `self.logo_quad` inside the render pass — the per-ship
        // layout it needs only exists in this block.
        let mut strip_logo_rects: Vec<ScreenRect> = Vec::new();
        if !self.workspace_slugs.is_empty() {
            let (pendings, labels, active) = self.strip_labels();
            // The fleet (owner asks 2026-09-07 + 2026-09-24 + 2026-09-27, ADR
            // 0042 L2a): each HOST GROUP is a ship — a brand wheel at the bow,
            // inline with the session names, and a row below it a waterline
            // running from the bow's rake to the stern with that group's box
            // name set into it, beneath the names it carries. The
            // name comes from `host_label`, the one display projection,
            // truncated like a session name is so a pathological host name
            // can't blow the layout ("local is just another host", so no
            // special-casing the group next to it). `logo_dims` doubles as
            // both "is there an asset to draw a wheel with" and the geometry
            // the layout reserves for it (one source for the logo's on-screen
            // size, not two); with no decoded logo the wheels drop out and the
            // names alone mark the groups, matching the asset's own
            // fail-soft contract (a decode failure never breaks the layout,
            // per `LOGO_DARK_PNG`'s doc).
            let logo_dims: Option<(f32, f32)> = self.logo_quad.as_ref().map(|(_, nw, nh)| {
                let logo_h = (self.cell_h - 2.0).max(1.0);
                let logo_w = logo_h * (*nw as f32 / (*nh).max(1) as f32);
                (logo_w, logo_h)
            });
            let items: Vec<StripItem> = strip_items(&self.workspace_slugs, |h| {
                strip_truncate(host_label(&self.hosts.declared_host, h))
            });
            // Every wheel is full size, and the layout reserves exactly that
            // and nothing more: which ship you steer is spent in the box name's
            // INK, not in its geometry, so switching ships still can't reflow
            // the strip — and the name itself is reserved by nobody, because it
            // runs a row BELOW the session names (`strip_item_widths`).
            let wheel_w = logo_dims.map(|(logo_w, _)| logo_w).unwrap_or(0.0);
            let label_widths: Vec<f32> = labels
                .iter()
                .map(|l| l.chars().count() as f32 * self.cell_w)
                .collect();
            let item_widths = strip_item_widths(&items, &label_widths, wheel_w);
            let item_positions = strip_cursor_positions(&item_widths, |i| {
                strip_gap_before(&items[i], self.cell_w)
            });
            let divider_offsets =
                strip_divider_offsets(&items, &item_widths, &label_widths, self.cell_w);
            let target = session_strip_target(&labels, active, self.cell_w, &divider_offsets);
            let scroll = self.strip_scroll_px.unwrap_or(target);
            // Two rows hanging off the bottom of the chrome grid: session
            // names above, the ships below them. The grid's own bottom edge is
            // where the chrome's last row was drawn (`project_lines` walks the
            // same rows from the same origin), and `cell_grid_for` has already
            // kept the band's rows out of it (`strip_reserved_rows`), so
            // neither row can land on the bottom border line and its version
            // stamp — and the air above the names is `STRIP_TOP_AIR_ROWS`
            // whatever the window height quantises to.
            let grid_bottom = self.chrome_origin_y
                + self.terminal.backend().rows() as f32 * self.cell_h;
            let (baseline_y, ship_y) = strip_row_tops(grid_bottom, self.cell_h);
            let strip_lines = self.strip_name_lines(&labels, active, scroll, baseline_y, &pendings, &divider_offsets);
            let (strip_hull_rects, tag_lines) = self.strip_ships(&items, &item_positions, &item_widths, wheel_w, scroll, logo_dims, baseline_y, ship_y, &mut strip_logo_rects);
            lines.extend(strip_lines);
            lines.extend(tag_lines);
            // Hulls ride the chrome's own colour-keyed solid-quad cache, so the
            // brown costs exactly one entry and one batched draw.
            if !strip_hull_rects.is_empty() {
                if !self.border_quads.contains_key(&HULL_RGB) {
                    let (r, g, b) = HULL_RGB;
                    let quad = Quad::from_rgba8(
                        &self.device,
                        &self.queue,
                        &self.quad_pipeline,
                        &[r, g, b, 255],
                        1,
                        1,
                    )
                    .context("failed to build hull-colour quad")?;
                    self.border_quads.insert(HULL_RGB, quad);
                }
                border_rects_by_color
                    .entry(HULL_RGB)
                    .or_default()
                    .extend(strip_hull_rects);
            }
            self.ease_strip(scroll, target);
        }
        Ok(strip_logo_rects)
    }

    fn strip_labels(&self) -> (Vec<bool>, Vec<String>, usize) {
        // ADR 0042 L2a: `workspace_slugs` is the UNION across every
        // host, so every parallel vector below keys off the full
        // `(host, slug)` pair, not the bare slug — two hosts can share
        // a slug, and the strip must not conflate their state.
        // Per-name badge-floor pending flag (ADR 0025 §1): true when that
        // workspace has a pending nav.preview result waiting, keyed by the
        // same (host, slug) WsKey pair pending_nav uses (ADR 0042 L2a codex
        // review, item E). Read BEFORE the labels because the badge is part
        // of the label (`strip_label`): every width below, the hull's
        // included, is measured from the text that is drawn.
        let pendings: Vec<bool> = self
            .workspace_slugs
            .iter()
            .map(|(h, s)| self.pending_nav.contains_key(&(h.clone(), s.clone())))
            .collect();
        let labels: Vec<String> = self
            .workspace_slugs
            .iter()
            .zip(&pendings)
            .map(|((h, s), &pending)| {
                strip_label(
                    self.workspace_labels
                        .get(&(h.clone(), s.clone()))
                        .map(String::as_str)
                        .unwrap_or(s.as_str()),
                    pending,
                )
            })
            .collect();
        let current_key: Option<WsKey> = self
            .active_workspace_id
            .clone()
            .or_else(|| self.default_workspace_slug.clone())
            .map(|s| (self.active_host.clone(), s));
        let active = current_key
            .as_ref()
            .and_then(|k| self.workspace_slugs.iter().position(|x| x == k))
            .unwrap_or(0)
            .min(labels.len().saturating_sub(1));
        (pendings, labels, active)
    }

    fn strip_name_lines(
        &self,
        labels: &[String],
        active: usize,
        scroll: f32,
        baseline_y: f32,
        pendings: &[bool],
        divider_offsets: &[f32],
    ) -> Vec<crate::text::Line> {
        // Per-name work-state tone, parallel to `labels` (built from
        // `workspace_slugs` in the same order). `now` is fetched per frame
        // so the wilt re-evaluates on the existing 1 Hz idle redraw.
        let strip_now = chrono::Utc::now();
        let tones: Vec<Option<(AgentTone, bool)>> = self
            .workspace_slugs
            .iter()
            .map(|(h, s)| {
                self.workspace_states
                    .get(&(h.clone(), s.clone()))
                    .and_then(|(st, at)| agent_tone_from(st, at, strip_now))
            })
            .collect();
        // Per-name status-change flash factor, parallel to `labels`.
        let flash_now = std::time::Instant::now();
        let flashes: Vec<f32> = self
            .workspace_slugs
            .iter()
            .map(|(h, s)| self.flash_factor_for(h, s, flash_now))
            .collect();
        let strip_lines = session_strip_lines(
            &labels,
            active,
            scroll,
            self.config.width as f32,
            self.cell_w,
            baseline_y,
            &tones,
            self.contrast_dim,
            &flashes,
            &pendings,
            &divider_offsets,
        );
        strip_lines
    }

    fn strip_ships(
        &self,
        items: &[StripItem],
        item_positions: &[f32],
        item_widths: &[f32],
        wheel_w: f32,
        scroll: f32,
        logo_dims: Option<(f32, f32)>,
        baseline_y: f32,
        ship_y: f32,
        strip_logo_rects: &mut Vec<ScreenRect>,
    ) -> (Vec<ScreenRect>, Vec<crate::text::Line>) {
        // ONE list of marks: every rect a ship draws — its wheel, its box
        // name, the waterline segments, the bow rake, the stern — culled
        // once, there (`ship_marks`, `strip_visible`), so what the loop
        // below paints is exactly what survived the cull and no draw site
        // can re-derive a second answer.
        let win_w = self.config.width as f32;
        let (_, logo_h) = logo_dims.unwrap_or((0.0, 0.0));
        let (hull_y, hull_h) = hull_band(ship_y, self.cell_h);
        let marks: Vec<StripMark> = ship_marks(
            &items,
            &item_positions,
            &item_widths,
            &self.active_host,
            |h| self.hosts.host_connected.get(h).copied().unwrap_or(false),
            wheel_w,
            self.cell_w,
            hull_h,
            scroll,
            win_w,
        );
        // Draw the marks. Every vertical coordinate comes from
        // `ship_vertical` — the one place the band's locked geometry lives.
        let vert = ship_vertical(baseline_y, ship_y, self.cell_h, logo_h);
        let mut strip_hull_rects: Vec<ScreenRect> = Vec::new();
        let mut tag_lines: Vec<crate::text::Line> = Vec::new();
        for m in &marks {
            match &m.kind {
                StripMarkKind::Wheel => {
                    // Centred vertically in the SESSION-NAME row: the wheel
                    // is inline with the names (owner, 2026-09-27), which is
                    // what leaves the row below it free to be a line.
                    strip_logo_rects.push(ScreenRect {
                        x: m.left,
                        y: vert.wheel_y,
                        w: m.w,
                        h: logo_h,
                    });
                }
                StripMarkKind::BoxName { name, steered } => {
                    // Water blue, in its own tier — and the steered box in
                    // the lighter water (`box_name_rgb`). Never the cream a
                    // session name takes: identical ink made a host read as
                    // one more session. Never lifted, never bold, no tone,
                    // flash or badge sigil — a box name is chrome, and a
                    // host is not an agent.
                    tag_lines.push(crate::text::Line {
                        text: name.clone(),
                        x: m.left,
                        y: vert.name_y,
                        color: Some(box_name_rgb(*steered, self.contrast_dim)),
                        bold: false,
                        italic: false,
                        dim: false,
                    })
                }
                StripMarkKind::Hull => {
                    strip_hull_rects.extend(hull_bar_rect(m.left, m.left + m.w, hull_y, hull_h))
                }
                StripMarkKind::BowRake => strip_hull_rects.extend(hull_bow_rects(
                    m.left,
                    m.w,
                    hull_y,
                    hull_h,
                    vert.rake_rise,
                )),
                StripMarkKind::Stern => {
                    strip_hull_rects.extend(hull_stern_rect(m.left + m.w, hull_y, hull_h, ship_y))
                }
            }
        }
        (strip_hull_rects, tag_lines)
    }

    fn ease_strip(&mut self, scroll: f32, target: f32) {
        // Ease toward `target` for the next frame; keep the frame loop
        // alive (dirty) until settled. Frame-rate-independent ease-out.
        let now = std::time::Instant::now();
        let dt = self
            .strip_anim_last
            .map(|t| (now - t).as_secs_f32().min(0.1))
            .unwrap_or(0.0);
        let k = if dt > 0.0 {
            1.0 - (-dt / STRIP_TAU).exp()
        } else {
            0.0
        };
        let next = scroll + (target - scroll) * k;
        if (target - next).abs() < 0.5 {
            self.strip_scroll_px = Some(target);
            self.strip_anim_last = None;
        } else {
            self.strip_scroll_px = Some(next);
            self.strip_anim_last = Some(now);
            self.dirty = true;
        }
        // Spin the brand wheels down on the same frame clock: advance the
        // angle by the current velocity, decay the velocity (frame-rate
        // independent), and keep the loop alive until it settles. The angle
        // is left wherever it stops — a wheel rests fine at any rotation.
        if self.wheel_vel.abs() > WHEEL_MIN_VEL {
            let wdt = self
                .wheel_anim_last
                .map(|t| (now - t).as_secs_f32().min(0.1))
                .unwrap_or(0.0);
            self.wheel_angle += self.wheel_vel * wdt;
            self.wheel_vel *= (-wdt / WHEEL_TAU).exp();
            if self.wheel_vel.abs() <= WHEEL_MIN_VEL {
                self.wheel_vel = 0.0;
                self.wheel_anim_last = None;
            } else {
                self.wheel_anim_last = Some(now);
                self.dirty = true;
            }
        }
    }
}
