//! The nav pane's body inside the chrome draw: the header, the tree rows and their colours, the scroll and the spill segments.

use super::*;
use super::view::{ChromeView, NavRow};

impl ChromeView<'_> {
    pub(super) fn nav_body(
        &mut self,
        nav_rect: ratatui::layout::Rect,
        preview_rect: ratatui::layout::Rect,
        tree_lines: Vec<NavRow>,
        nav_spill_segs_out: &mut Vec<NavSpillSeg>,
    ) -> (usize, Vec<String>, ratatui::layout::Rect, Paragraph<'static>) {
        let ChromeView { nav_prompt_line, lease_notice, nav_line, owed_line, status, nav_cursor_body_pos, tree_empty, nav_has_cursor, mut nav_scroll, .. } = *self;
        let owed_drawn;
        let leaving_drawn;
        // Help lives on the focused border; keep the nav header compact.
        // The status text wraps at the pane's width (a toast is a
        // sentence): "status: " heads the first line only. Everything
        // below counts from body_lines.len(), and the cursor's body
        // position (a header of one status line and a spacer, computed
        // before the draw) shifts by the extra lines here.
        let nav_w = nav_rect.width as usize;
        let (nav_list_h, nav_pinned, line_whole) = nav_pinned_rows(
            nav_prompt_line.as_ref().map(|(t, c)| (t.as_str(), c.as_str())),
            lease_notice,
            nav_line.as_deref(),
            nav_w,
            nav_rect.height as usize,
        );
        let nav_list_rect = ratatui::layout::Rect { height: nav_list_h as u16, ..nav_rect };
        // Only a line drawn whole is acked: a grant's count here, the
        // leaving line once presented (`Leaving::presented`).
        owed_drawn = line_whole && owed_line.is_some() && nav_line == owed_line;
        leaving_drawn = line_whole;
        let mut body_lines = status_spans(&status, nav_w);
        let nav_cursor_body_pos = nav_cursor_body_pos + body_lines.len() - 1;
        body_lines.push(RtLine::from(""));
        if tree_empty {
            body_lines.push(RtLine::from(vec![Span::styled(
                "  (no tree yet)",
                Style::default().add_modifier(Modifier::DIM),
            )]));
        }
        let (tree_rows_body_start, tree_rows_body_end) = self.nav_tree_rows(&mut body_lines, tree_lines);
        // Scroll the nav body so the selected tree row stays in
        // the comfort zone — the middle 1/3 of the pane. Going
        // down: once cursor crosses the bottom-third boundary,
        // the scroll advances so the cursor stays planted at
        // that boundary, no big jumps. Going up: same on the
        // top boundary. At the actual top/bottom of the body
        // the cursor falls through to the real first/last row,
        // since clamping `nav_scroll` to [0, max_scroll]
        // releases it. Header lines scroll off the top as a
        // simple trade; sub-paneled header/footer is a later
        // refinement.
        let nav_inner_h = nav_list_h;
        let body_len = body_lines.len();
        if !nav_has_cursor || body_len <= nav_inner_h {
            nav_scroll = 0;
        } else {
            let scrolloff = (nav_inner_h / 3).max(1);
            let min_view = scrolloff;
            // last comfort row in the viewport (inclusive)
            let max_view = nav_inner_h.saturating_sub(scrolloff).saturating_sub(1);
            let view_pos = nav_cursor_body_pos.saturating_sub(nav_scroll as usize);
            if view_pos < min_view {
                nav_scroll = (nav_cursor_body_pos.saturating_sub(min_view)) as u16;
            } else if view_pos > max_view {
                nav_scroll = (nav_cursor_body_pos.saturating_sub(max_view)) as u16;
            }
            let max_scroll = body_len.saturating_sub(nav_inner_h) as u16;
            if nav_scroll > max_scroll {
                nav_scroll = max_scroll;
            }
        }
        self.collect_nav_spill(nav_rect, preview_rect, nav_scroll, tree_rows_body_start, tree_rows_body_end, &body_lines, nav_list_h, nav_spill_segs_out);
        let nav_body = Paragraph::new(body_lines).scroll((nav_scroll, 0));
        self.owed_drawn = owed_drawn;
        self.leaving_drawn = leaving_drawn;
        self.nav_scroll = nav_scroll;
        (nav_list_h, nav_pinned, nav_list_rect, nav_body)
    }

    fn nav_tree_rows(&self, body_lines: &mut Vec<RtLine<'static>>, tree_lines: Vec<NavRow>) -> (usize, usize) {
        let ChromeView { concept_status, last_key, .. } = *self;
        // Exact tree-row span of body_lines, captured AT ASSEMBLY
        // (codex round 4: the header is not a constant — Files/
        // Modules carry 4 chrome lines, Sessions 5, the picker 3 —
        // so any fixed offset either spills chrome or misses bottom
        // rows). The picker's own header row lives inside
        // tree_lines and stays spill-eligible on purpose: floating
        // the full picker path is exactly what the spill is for.
        let tree_rows_body_start = body_lines.len();
        for (
            text,
            is_selected,
            is_stale,
            is_pinned,
            agent,
            flash,
            is_pending,
            is_attention,
        ) in
            &tree_lines
        {
            let mut style = self.nav_row_style(is_attention, is_stale, agent, is_pinned, flash, is_selected);
            // Badge floor (ADR 0025 §1): a workspace with a pending
            // nav.preview result gets a non-disruptive "result waiting"
            // badge — a leading `● ` sigil in bright white + bold,
            // and the row fg pulled to the same accent (clearing DIM) so
            // it reads distinctly from the work-state tones (green
            // working / purple waiting / red blocked / etc.) and the
            // cyan pin, without adding another hue to the palette. The
            // view is never switched; only the colour/sigil changes.
            if *is_pending {
                const PENDING_ACCENT: Color = Color::Rgb(255, 255, 255);
                style = style.fg(PENDING_ACCENT).remove_modifier(Modifier::DIM);
                if *is_selected {
                    style = style.add_modifier(Modifier::BOLD);
                }
                body_lines.push(RtLine::from(vec![
                    Span::styled(
                        "● ",
                        Style::default()
                            .fg(PENDING_ACCENT)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(text.clone(), style),
                ]));
            } else {
                body_lines.push(RtLine::from(vec![Span::styled(text.clone(), style)]));
            }
        }
        let tree_rows_body_end = body_lines.len();
        body_lines.push(RtLine::from(""));
        body_lines.push(RtLine::from(vec![Span::styled(
            concept_status.clone(),
            Style::default().fg(Color::LightMagenta),
        )]));
        body_lines.push(RtLine::from(""));
        body_lines.push(RtLine::from(vec![
            Span::styled("key: ", Style::default().fg(Color::DarkGray)),
            Span::raw(last_key.clone().unwrap_or_else(|| "(none)".to_string())),
        ]));
        (tree_rows_body_start, tree_rows_body_end)
    }

    fn nav_row_style(
        &self,
        is_attention: &bool,
        is_stale: &bool,
        agent: &Option<(AgentTone, bool)>,
        is_pinned: &bool,
        flash: &f32,
        is_selected: &bool,
    ) -> Style {
        let ChromeView { contrast_dim, mode, .. } = *self;
        let mut style = Style::default();
        // Cross-cutting colour layer. `is_stale` (annotation
        // drift OR, since ADR 0030 §8 decision 31c, a foreign-
        // build capsule row) is loudest and checked FIRST — a
        // row that is drifted or unusable must read that way
        // regardless of any work-state tone it also carries.
        // Below that, state-nav agent tone (ADR 0023) owns the
        // colour of a Sessions row that has one: working/idle/
        // blocked/done each get a hue, a stale "working" wilts
        // (DIM), and selection still reads through the `>`
        // caret + bold so the cursor stays visible over the
        // state colour. Without an agent tone either, the
        // original layer applies: the pinned accent (bright
        // cyan, distinct from the yellow stale/selected hues),
        // then selection (light yellow), then dim.
        if *is_attention {
            // Attention row: yellow + BOLD. Scoped to rows that
            // announce a key the user must press BEFORE the
            // default commits, so it never competes with the
            // cross-cutting stale hue below (which stays plain
            // yellow — drift is noticed, not shouted).
            style = style.fg(Color::Yellow).add_modifier(Modifier::BOLD);
        } else if *is_stale {
            style = style.fg(Color::Yellow);
        } else if let Some((tone, aged)) = agent {
            // Resolve the tone to RGB through the shared contrast
            // helper so the nav row and the bottom strip render
            // the same pixels. `Color::Rgb` (not the named tone
            // colour) is required because the "bright"/"dim"
            // levers and the status-change flash scale/lerp the
            // channels — ratatui can't lerp a named colour. Bold
            // still composes with the colour, and the
            // stale-"working" wilt still DIMs.
            let (rgb, bold, dim) =
                contrast_tone_rgb(*tone, *aged, *is_selected, contrast_dim, *flash);
            if let Some((r, g, b)) = rgb {
                style = style.fg(Color::Rgb(r, g, b));
            }
            if dim {
                style = style.add_modifier(Modifier::DIM);
            }
            if bold {
                style = style.add_modifier(Modifier::BOLD);
            }
        } else if *is_pinned {
            style = style.fg(Color::Cyan).add_modifier(Modifier::BOLD);
        } else if *flash > 0.0 {
            // Tone-less Sessions row that just changed state:
            // resolve the base fg + flash toward white so the
            // blink reads even without a state colour.
            let base = if *is_selected {
                (245, 245, 67)
            } else {
                (204, 204, 204)
            };
            let (r, g, b) = lerp_to_white(base, *flash);
            style = style.fg(Color::Rgb(r, g, b));
            if *is_selected {
                style = style.add_modifier(Modifier::BOLD);
            }
        } else if *is_selected {
            style = style.fg(Color::LightYellow);
        } else if contrast_dim && mode == Mode::Sessions {
            // "dim" lever: fade non-selected Sessions rows that
            // carry no tone so the selection pops by contrast.
            // Scoped to Sessions so Files/Modules nav is untouched.
            let (r, g, b) = scale_rgb((204, 204, 204), CONTRAST_DIM_FACTOR);
            style = style.fg(Color::Rgb(r, g, b));
        } else {
            style = style.add_modifier(Modifier::DIM);
        }
        style
    }

    fn collect_nav_spill(
        &self,
        nav_rect: ratatui::layout::Rect,
        preview_rect: ratatui::layout::Rect,
        nav_scroll: u16,
        tree_rows_body_start: usize,
        tree_rows_body_end: usize,
        body_lines: &[RtLine<'static>],
        nav_list_h: usize,
        nav_spill_segs_out: &mut Vec<NavSpillSeg>,
    ) {
        let ChromeView { nav_spill_active, .. } = *self;
        // Nav-spill segment collection: for each visible TREE row
        // whose text is wider than the nav column, record the full
        // row (truncated to the overlay's reach cap) so the render-
        // pass tail can float it over the preview's left edge.
        // TREE rows only — the assembly-captured span above: header
        // and trailing chrome lines (status/help/concept/key) never
        // spill (codex review). Widths are terminal
        // CELLS via unicode-width, so CJK/emoji names measure and
        // truncate exactly (codex review).
        if nav_spill_active && nav_rect.width > 0 && preview_rect.width > 0 {
            use unicode_width::UnicodeWidthStr;
            // Reach: from the nav left edge to 2 cells short of the
            // preview's right edge, in cells.
            let max_cells = (preview_rect.x + preview_rect.width)
                .saturating_sub(2)
                .saturating_sub(nav_rect.x) as usize;
            let first = nav_scroll as usize;
            let tree_span = tree_rows_body_start..tree_rows_body_end;
            let visible = body_lines.iter().skip(first).take(nav_list_h);
            for (vis_idx, line) in visible.enumerate() {
                if !tree_span.contains(&(first + vis_idx)) {
                    continue;
                }
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                let cell_w = UnicodeWidthStr::width(text.as_str());
                let Some(take) = nav_spill_take(cell_w, nav_rect.width as usize, max_cells)
                else {
                    continue;
                };
                // Style of the widest span — rows are one span, or
                // sigil + text where the text span dominates (and
                // the pending sigil shares the text's accent
                // anyway), so a single-run overlay is colour-
                // faithful in practice.
                let style = line
                    .spans
                    .iter()
                    .max_by_key(|s| UnicodeWidthStr::width(s.content.as_ref()))
                    .map(|s| s.style)
                    .unwrap_or_default();
                let (shown, shown_cells) = if take < cell_w {
                    truncate_to_cells(&text, take)
                } else {
                    let w = UnicodeWidthStr::width(text.as_str());
                    (text, w)
                };
                let width_cells = shown_cells as u16;
                nav_spill_segs_out.push(NavSpillSeg {
                    x: nav_rect.x,
                    row: nav_rect.y + vis_idx as u16,
                    text: shown,
                    width_cells,
                    color: crate::ui::render::cells::ratatui_color_to_rgb(style.fg),
                    bold: style.add_modifier.contains(Modifier::BOLD),
                    dim: style.add_modifier.contains(Modifier::DIM),
                });
            }
        }
    }
}
