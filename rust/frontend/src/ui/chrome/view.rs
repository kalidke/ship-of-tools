//! The chrome draw closure's view: `ChromeView` lends the closure one frame's snapshots, and its methods
//! paint the panes' titles, the REPL drawer body, the wireframe, the clock, the stamp and the terminals.

use super::*;

// Snapshot per-row chrome strings up-front so the ratatui closure
// doesn't need to borrow `self.tree` (the closure captures `frame`
// mutably elsewhere and the borrow checker dislikes mixing).
//
// When the workspace picker is active we render *its* directory
// listing in the NavTree pane instead of `self.tree.rows`, so
// Sessions mode flow visibly transitions to the picker without
// having to introduce a second pane region. A title row at the
// top shows `current_path` so the user always knows where they
// are; below it, each subdirectory is one row, plus a `[..]`
// ascend row at the very top of the list for one-key parent
// navigation.
// Each tuple: (line text, selected, stale-annotation, pinned, agent
// tone, flash). The 5th element is the state-nav agent work-state (ADR
// 0023), present only on Sessions rows that carry one — `None`
// everywhere else, so every other mode renders unchanged. The 6th is
// the status-change flash factor (0.0 = no flash), also Sessions-only.
// (text, is_selected, is_stale, is_pinned, agent_tone, flash,
//  is_pending, is_attention)
// `is_attention` (2026-09-15, field report) marks a row that
// names an action the user must take before the default is
// committed: yellow + bold, ahead of every other colour layer.
// `is_pending` (ADR 0025 §1 badge floor) flags a Sessions row whose
// workspace has a pending nav.preview result waiting — rendered as a
// non-disruptive indicator distinct from the work-state colours.
pub(super) type NavRow = (
    String,
    bool,
    bool,
    bool,
    Option<(AgentTone, bool)>,
    f32,
    bool,
    bool,
);

/// What one frame's chrome draw reads and writes. The ratatui draw closure
/// cannot borrow `self` (`terminal.draw` holds it), so `draw_chrome` lends it
/// this view of its own locals; fields below `// written by the draw` are the
/// closure's results, copied back into `draw_chrome`'s locals after the draw.
pub(super) struct ChromeView<'a> {
    pub(super) layout_preset: &'a crate::ui::persist::settings::LayoutPreset,
    pub(super) drawer_open: bool,
    pub(super) maximize_slot: Option<crate::ui::persist::settings::Slot>,
    pub(super) nav_logo_row: bool,
    pub(super) focus: PaneFocus,
    pub(super) mode: Mode,
    pub(super) nav_prompt_line: &'a Option<(String, String)>,
    pub(super) lease_notice: Option<&'static str>,
    pub(super) nav_line: &'a Option<String>,
    pub(super) owed_line: &'a Option<String>,
    pub(super) status: &'a String,
    pub(super) nav_cursor_body_pos: usize,
    pub(super) tree_empty: bool,
    pub(super) contrast_dim: bool,
    pub(super) concept_status: &'a String,
    pub(super) last_key: &'a Option<String>,
    pub(super) nav_has_cursor: bool,
    pub(super) nav_spill_active: bool,
    pub(super) preview_name: &'a Option<String>,
    pub(super) pinned_preview_node_id: &'a Option<String>,
    pub(super) drawer: DrawerContent,
    pub(super) monitor_host_connected: bool,
    pub(super) monitor_host_label: &'a HostKey,
    pub(super) repl_input: &'a String,
    pub(super) repl_lines: &'a Vec<RtLine<'static>>,
    pub(super) repl_pkg_mode: bool,
    pub(super) help_state: &'a help::Help,
    pub(super) help_bindings: &'a KeyBindings,
    pub(super) help_context: &'a help::Context,
    pub(super) clock_now: chrono::NaiveDateTime,
    pub(super) battery: &'a Option<String>,
    pub(super) version_stamp: &'a String,
    pub(super) version_skew: bool,
    pub(super) pty_screen: &'a vt100::Screen,
    pub(super) pane_overlay: &'a Vec<String>,
    pub(super) term_screen: Option<&'a vt100::Screen>,
    // written by the draw
    pub(super) owed_drawn: bool,
    pub(super) leaving_drawn: bool,
    pub(super) nav_scroll: u16,
    pub(super) preview_cells: ratatui::layout::Rect,
    pub(super) repl_cells: ratatui::layout::Rect,
    pub(super) new_pane_rects: PaneRects,
    pub(super) new_repl_scroll: u16,
    pub(super) repl_scrollback_cells: ratatui::layout::Rect,
    pub(super) repl_window: (usize, usize),
    pub(super) pty_size_observed: (u16, u16),
    pub(super) term_size_observed: (u16, u16),
}

impl ChromeView<'_> {
    pub(super) fn paint(
        &mut self,
        frame: &mut ratatui::Frame<'_>,
        tree_lines: Vec<NavRow>,
        nav_spill_segs_out: &mut Vec<NavSpillSeg>,
    ) {
        let ChromeView { focus, mode, .. } = *self;
        let area = frame.area();
        let (geom, nav_frame_rect, nav_rect, preview_rect, llm_rect, repl_rect) = self.pane_rects(area);

        // Style palette: borders are uniform gray; focus is
        // signalled only through title colour (cyan when the
        // pane has focus, gray otherwise). No per-pane border
        // colour means the wireframe stays internally
        // consistent.
        let border_style = Style::default().fg(Color::DarkGray);
        let focus_title_style = Style::default().fg(Color::LightCyan);
        let idle_title_style = Style::default().fg(Color::DarkGray);

        let nav_focus = focus == PaneFocus::NavTree;
        let nav_title = format!(
            " nav · mode: {} {} ",
            mode.label(),
            if nav_focus { "· [FOCUS]" } else { "" }
        );
        let (nav_list_h, nav_pinned, nav_list_rect, nav_body) = self.nav_body(nav_rect, preview_rect, tree_lines, nav_spill_segs_out);

        // Other pane titles + body widgets. No Block / borders
        // — we paint the frame ourselves below so the math is
        // exact and there are no double walls.
        let preview_focus = focus == PaneFocus::Preview;
        let preview_pinned = self.pinned_preview_node_id.is_some();
        let preview_title = self.preview_title(preview_focus, preview_pinned, preview_rect);
        self.export_pane_rects(nav_frame_rect, preview_rect, llm_rect, repl_rect);

        let llm_focus = focus == PaneFocus::Llm;
        let llm_title = if llm_focus {
            " llm · [FOCUS] ".to_string()
        } else {
            " llm ".to_string()
        };

        let repl_focus = focus == PaneFocus::Repl;
        let repl_title = self.drawer_title(repl_focus);
        let (repl_split, scroll_para, input_para) = self.repl_drawer_body(repl_rect, repl_focus);

        render_nav_widgets(frame, nav_body, nav_list_rect, nav_pinned, nav_rect, nav_list_h, self.nav_prompt_line.is_some());
        self.render_drawer_widgets(frame, repl_rect, scroll_para, input_para, repl_split);

        // Paint the wireframe directly into the buffer.
        // vlines/hlines drive the inner borders + corner
        // junctions; outer perimeter is always drawn. Each
        // border cell is written exactly once.
        let buf = frame.buffer_mut();
        draw_wireframe(
            buf,
            area,
            &geom.vlines,
            &geom.hlines,
            geom.drawer_x_end,
            geom.llm_left_vline,
            border_style,
        );
        // Titles overlay the top wireframe edge of each
        // column (and the drawer's top edge when open). Each
        // title is clamped to its column's interior width so
        // it can't smear over a divider or the neighbour's
        // title.
        let title_style_for = |focused: bool| {
            if focused {
                focus_title_style
            } else {
                idle_title_style
            }
        };
        let title_w = |rect: ratatui::layout::Rect| rect.width.saturating_sub(2);
        if nav_frame_rect.width > 0 {
            write_title(
                buf,
                nav_frame_rect.x + 1,
                nav_frame_rect.y.saturating_sub(1),
                &nav_title,
                title_w(nav_frame_rect),
                title_style_for(nav_focus),
            );
        }
        if preview_rect.width > 0 {
            write_title(
                buf,
                preview_rect.x + 1,
                preview_rect.y.saturating_sub(1),
                &preview_title,
                title_w(preview_rect),
                title_style_for(preview_focus),
            );
        }
        if llm_rect.width > 0 {
            write_title(
                buf,
                llm_rect.x + 1,
                llm_rect.y.saturating_sub(1),
                &llm_title,
                title_w(llm_rect),
                title_style_for(llm_focus),
            );
        }
        if repl_rect.width > 0 {
            write_title(
                buf,
                repl_rect.x + 1,
                repl_rect.y.saturating_sub(1),
                &repl_title,
                title_w(repl_rect),
                title_style_for(repl_focus),
            );
        }

        self.paint_focus_hint(buf, nav_frame_rect, preview_rect, llm_rect, repl_rect, border_style, focus_title_style);

        self.paint_clock(buf, area, idle_title_style);

        self.paint_version_stamp(buf, area, idle_title_style);

        self.paint_pane_terminals(buf, llm_rect, repl_rect);
    }

    fn pane_rects(
        &self,
        area: ratatui::layout::Rect,
    ) -> (crate::ui::chrome::layout::LayoutGeom, ratatui::layout::Rect, ratatui::layout::Rect, ratatui::layout::Rect, ratatui::layout::Rect, ratatui::layout::Rect) {
        let ChromeView { layout_preset, drawer_open, maximize_slot, nav_logo_row, .. } = *self;
        // Inner divisions positioned by the user-configurable
        // settings (defaults 50/50, see settings.toml). Range
        // clamped to [10, 90] at parse time so the math here
        // can't degenerate.
        // Pane geometry — pure integer math, no Block borders.
        // Borders are drawn by us into the buffer below so every
        // shared edge is exactly one cell wide and junctions are
        // proper line-drawing characters. The "content" rects
        // are the interior of each quadrant (no border cells).
        //
        //   col 0 = outer left   col mid_col = inner vertical   col last = outer right
        //   row 0 = outer top    row mid_row = inner horizontal row last = outer bottom
        // Preset-driven geometry.
        // Each named slot gets a rect; vlines/hlines drive the
        // wireframe + title positioning. Maximisation collapses
        // every other slot + every inner border so the focused
        // pane absorbs the area; zero-sized siblings' paint
        // paths no-op (the pty.open/resize guard at
        // `cols >= 2 && rows >= 2` similarly keeps the BL
        // backend safe). Toggle: Ctrl+z. A leave's line restores the
        // panes (`maximize_slot`).
        let geom = crate::ui::chrome::layout::compute(area, &layout_preset, drawer_open, maximize_slot);
        // Names preserved so the rest of the closure reads
        // unchanged: nav = old TL (left column), preview = old
        // TR (middle column), llm = old BL (rightmost column
        // in the 3-col layout), repl = old BR (bottom drawer).
        // `nav_frame_rect` is the pane as laid out (title and focus border
        // hang off it); `nav_rect` is what the tree body may use -- one
        // row shorter when the wordmark owns the first row.
        let nav_frame_rect = geom.rect_for(crate::ui::persist::settings::Slot::Nav);
        let nav_rect = if nav_logo_row && nav_frame_rect.height > 1 {
            ratatui::layout::Rect {
                y: nav_frame_rect.y + 1,
                height: nav_frame_rect.height - 1,
                ..nav_frame_rect
            }
        } else {
            nav_frame_rect
        };
        let preview_rect = geom.rect_for(crate::ui::persist::settings::Slot::Preview);
        let llm_rect = geom.rect_for(crate::ui::persist::settings::Slot::Llm);
        let repl_rect = geom.rect_for(crate::ui::persist::settings::Slot::Repl);
        (geom, nav_frame_rect, nav_rect, preview_rect, llm_rect, repl_rect)
    }

    fn preview_title(
        &self,
        preview_focus: bool,
        preview_pinned: bool,
        preview_rect: ratatui::layout::Rect,
    ) -> String {
        let ChromeView { preview_name, .. } = *self;
        // T1: surface the full path of the file the preview is showing
        // (clipped in the narrow nav column) here in the wide title.
        // Markers go after the name so middle-truncating the name to
        // fit never drops [FOCUS]/[pinned *].
        let preview_title = {
            let mut markers = String::new();
            if preview_focus {
                markers.push_str(" · [FOCUS]");
            }
            if preview_pinned {
                markers.push_str(" · [pinned *]");
            }
            match preview_name.clone() {
                Some(name) => {
                    // Budget the name against the pane width so even an
                    // over-long title keeps its basename + the markers.
                    let avail = preview_rect.width.saturating_sub(2) as usize;
                    let fixed = " preview · ".chars().count() + markers.chars().count() + 1; // trailing space
                    let name_budget = avail.saturating_sub(fixed).max(1);
                    let shown = middle_truncate(&name, name_budget);
                    format!(" preview · {shown}{markers} ")
                }
                None => format!(" preview{markers} "),
            }
        };
        preview_title
    }

    fn export_pane_rects(
        &mut self,
        nav_frame_rect: ratatui::layout::Rect,
        preview_rect: ratatui::layout::Rect,
        llm_rect: ratatui::layout::Rect,
        repl_rect: ratatui::layout::Rect,
    ) {
        let preview_cells;
        let repl_cells;
        let new_pane_rects;
        // The preview slot still needs its content cell rect
        // exported for the wgpu preview-layer surface.
        preview_cells = preview_rect;
        // Drawer cell rect, exported for the Ctrl+M monitor chart quad.
        repl_cells = repl_rect;
        // Cache the four pane content rects for between-frame
        // hit-testing (mouse wheel → which pane scrolls).
        new_pane_rects = PaneRects {
            nav: nav_frame_rect,
            preview: preview_rect,
            llm: llm_rect,
            repl: repl_rect,
        };
        self.preview_cells = preview_cells;
        self.repl_cells = repl_cells;
        self.new_pane_rects = new_pane_rects;
    }

    fn drawer_title(&self, repl_focus: bool) -> String {
        let ChromeView { drawer, monitor_host_connected, monitor_host_label, .. } = *self;
        // G6: the drawer title reflects which content it's showing —
        // the Julia REPL (Ctrl+J) or the local terminal (Ctrl+T).
        let repl_title = match (drawer, repl_focus) {
            (DrawerContent::Terminal, true) => " terminal · [FOCUS] ".to_string(),
            (DrawerContent::Terminal, false) => " terminal ".to_string(),
            // 4.3, option (a): names whose record this is — the
            // resolved host (the hub when it's connected, else the
            // `default_host` fallback), flagged when that host
            // isn't actually among today's connections.
            (DrawerContent::Monitor, _) => {
                if monitor_host_connected {
                    format!(" monitor · {monitor_host_label} ")
                } else {
                    format!(" monitor · {monitor_host_label} [not connected] ")
                }
            }
            (DrawerContent::Help, _) => " help ".to_string(),
            (_, true) => " repl · julia · [FOCUS] ".to_string(),
            (_, false) => " repl · julia ".to_string(),
        };
        repl_title
    }

    fn repl_drawer_body(
        &mut self,
        repl_rect: ratatui::layout::Rect,
        repl_focus: bool,
    ) -> (std::rc::Rc<[ratatui::layout::Rect]>, Paragraph<'static>, Paragraph<'static>) {
        let ChromeView { repl_input, repl_lines, repl_pkg_mode, mut new_repl_scroll, .. } = *self;
        let repl_scrollback_cells;
        let repl_window;
        // Input pane height tracks the number of newline-separated
        // lines in `repl_input` so a multi-line buffer (built up
        // via Shift+Enter) is fully visible while editing. Capped
        // at `repl_rect.height - 1` so at least one row of
        // scrollback is always on screen — a runaway buffer
        // narrows scrollback but is still recoverable via Enter
        // or Backspace.
        let input_line_count = (repl_input.matches('\n').count() + 1) as u16;
        let max_input_rows = repl_rect.height.saturating_sub(1).max(1);
        let input_rows = input_line_count.min(max_input_rows).max(1);
        let repl_split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(input_rows)])
            .split(repl_rect);
        let scroll_h = repl_split[0].height as usize;
        // Scrollback window: `repl_scroll` is the number of rows
        // *back from the tail*. 0 = live; positive = older. Clamp
        // so the user can't scroll past the top of the log, and
        // write the clamped value back to State so the wheel
        // handler doesn't accumulate dead range.
        let total = repl_lines.len();
        let max_scroll = total.saturating_sub(scroll_h) as u16;
        let clamped = new_repl_scroll.min(max_scroll);
        new_repl_scroll = clamped;
        let end = total.saturating_sub(clamped as usize);
        let start = end.saturating_sub(scroll_h);
        // Export for the inline-image paint pass: which absolute
        // lines are on screen, and the sub-rect they render into.
        repl_scrollback_cells = repl_split[0];
        repl_window = (start, end);
        let scroll_para = Paragraph::new(repl_lines[start..end].to_vec());
        // Mode-aware prompt: `julia> ` in cyan vs `pkg> ` in
        // blue (matches the standard Julia REPL palette). Dim
        // both when the REPL pane isn't focused — same
        // attention-direction trick as before.
        // Match the stdlib `REPL.jl` / VSCode Julia-ext palette:
        // `julia>` green, `pkg>` blue. Light* variants pop on the
        // near-black surface fg.
        let prompt_text = if repl_pkg_mode { "pkg> " } else { "julia> " };
        let prompt_focus_color = if repl_pkg_mode {
            Color::LightBlue
        } else {
            Color::LightGreen
        };
        let prompt_color = if repl_focus {
            prompt_focus_color
        } else {
            Color::DarkGray
        };
        // Multi-line input: first segment carries the live prompt,
        // continuation segments get a same-width filler so the
        // text column stays aligned under the prompt. Cursor
        // block lives at the end of the last segment regardless
        // of how many lines deep we are.
        let cont_pad: String = " ".repeat(prompt_text.len());
        let segments: Vec<&str> = repl_input.split('\n').collect();
        let last_idx = segments.len().saturating_sub(1);
        let input_rt_lines: Vec<RtLine> = segments
            .iter()
            .enumerate()
            .map(|(i, seg)| {
                let mut spans: Vec<Span> = Vec::with_capacity(3);
                if i == 0 {
                    spans
                        .push(Span::styled(prompt_text, Style::default().fg(prompt_color)));
                } else {
                    spans.push(Span::raw(cont_pad.clone()));
                }
                spans.push(Span::raw(seg.to_string()));
                if repl_focus && i == last_idx {
                    spans.push(Span::styled(
                        "\u{2588}",
                        Style::default().fg(prompt_focus_color),
                    ));
                }
                RtLine::from(spans)
            })
            .collect();
        let input_para = Paragraph::new(input_rt_lines);
        self.new_repl_scroll = new_repl_scroll;
        self.repl_scrollback_cells = repl_scrollback_cells;
        self.repl_window = repl_window;
        (repl_split, scroll_para, input_para)
    }

    fn render_drawer_widgets(
        &self,
        frame: &mut ratatui::Frame<'_>,
        repl_rect: ratatui::layout::Rect,
        scroll_para: Paragraph<'static>,
        input_para: Paragraph<'static>,
        repl_split: std::rc::Rc<[ratatui::layout::Rect]>,
    ) {
        let ChromeView { drawer, help_state, help_bindings, .. } = *self;
        if drawer == DrawerContent::Help {
            help::render(frame, repl_rect, help_state, help_bindings);
        }
        if drawer == DrawerContent::Repl {
            frame.render_widget(scroll_para, repl_split[0]);
            frame.render_widget(input_para, repl_split[1]);
        }
    }

    fn paint_focus_hint(
        &self,
        buf: &mut ratatui::buffer::Buffer,
        nav_frame_rect: ratatui::layout::Rect,
        preview_rect: ratatui::layout::Rect,
        llm_rect: ratatui::layout::Rect,
        repl_rect: ratatui::layout::Rect,
        border_style: Style,
        focus_title_style: Style,
    ) {
        let ChromeView { focus, help_context, help_bindings, .. } = *self;
        let focused_rect = match focus {
            PaneFocus::NavTree => nav_frame_rect, PaneFocus::Preview => preview_rect,
            PaneFocus::Llm => llm_rect, PaneFocus::Repl => repl_rect,
        };
        if focused_rect.width > 2 {
            let width = focused_rect.width.saturating_sub(2) as usize;
            let title = format!(" {} · ", help_context.title());
            let title_width = unicode_width::UnicodeWidthStr::width(title.as_str());
            let hint = help::border(&help_context, help_bindings, width.saturating_sub(title_width));
            let title = help::truncate(&format!("{title}{hint}"), width);
            // Clear old title glyphs before writing the shorter dynamic title.
            for x in focused_rect.x..focused_rect.x + focused_rect.width {
                buf[(x, focused_rect.y.saturating_sub(1))].set_symbol("─").set_style(border_style);
            }
            write_title(buf, focused_rect.x + 1, focused_rect.y.saturating_sub(1),
                &title, width as u16, focus_title_style);
        }
    }

    fn paint_clock(&self, buf: &mut ratatui::buffer::Buffer, area: ratatui::layout::Rect, idle_title_style: Style) {
        let ChromeView { clock_now, battery, .. } = *self;
        // Live local-time clock, right-aligned on the top edge just
        // inside the outer-right corner glyph. Same chrome text style
        // as an idle pane title. Repaints ~1×/second via the
        // `about_to_wait` WaitUntil scheduling below.
        {
            let clock_label = format!(" {} ", clock_label(clock_now, area.width));
            let clock_cells = clock_label.chars().count() as u16;
            // Keep the ┐ corner; sit one cell to its left, then back
            // off by the label width. No-op if the window is too
            // narrow to fit the clock without colliding with a title.
            if area.width > clock_cells + 2 {
                let clock_x = area.x + area.width - 1 - clock_cells;
                write_title(
                    buf,
                    clock_x,
                    area.y,
                    &clock_label,
                    clock_cells,
                    idle_title_style,
                );

                // Battery indicator sits immediately left of the clock
                // with a one-cell gap, same dim chrome style. Painted
                // only if a battery is present (cached label is `Some`)
                // AND the window is wide enough to fit it left of the
                // clock without colliding with the left border. When
                // it's too narrow we drop the battery and keep the
                // clock.
                if let Some(batt) = battery.as_deref() {
                    let batt_label = format!(" {batt} ");
                    let batt_cells = batt_label.chars().count() as u16;
                    // Need: left border (x) + at least one cell, then
                    // the battery, then the clock. Guard with the same
                    // ">" slack the clock uses.
                    if clock_x > area.x + batt_cells + 1 {
                        let batt_x = clock_x - batt_cells;
                        write_title(
                            buf,
                            batt_x,
                            area.y,
                            &batt_label,
                            batt_cells,
                            idle_title_style,
                        );
                    }
                }
            }
        }
    }

    fn paint_version_stamp(
        &self,
        buf: &mut ratatui::buffer::Buffer,
        area: ratatui::layout::Rect,
        idle_title_style: Style,
    ) {
        let ChromeView { version_stamp, version_skew, .. } = *self;
        // FE/BE version stamp, left-aligned on the BOTTOM outer edge
        // — the mirror of the `nav · mode:` title on the top edge,
        // same `write_title` treatment and the same two-cell inset
        // from the corner glyph. Sits ON the border line; the session
        // strip is a pixel overlay one row lower, so the two don't
        // fight for the same cells.
        //
        // Dark gray when FE and BE agree, yellow when they don't:
        // the halves drift independently (rebuild one, forget the
        // other), and a skew you have to read character-by-character
        // to notice isn't surfaced at all.
        {
            let stamp_cells = version_stamp.chars().count() as u16;
            let bot_y = area.y + area.height - 1;
            // Same guard shape as the clock: skip entirely rather
            // than smear a truncated version across the corner when
            // the window is too narrow to hold it.
            if area.width > stamp_cells + 2 {
                write_title(
                    buf,
                    area.x + 2,
                    bot_y,
                    &version_stamp,
                    stamp_cells,
                    if version_skew {
                        Style::default().fg(Color::Yellow)
                    } else {
                        idle_title_style
                    },
                );
            }
        }
    }

    fn paint_pane_terminals(
        &mut self,
        buf: &mut ratatui::buffer::Buffer,
        llm_rect: ratatui::layout::Rect,
        repl_rect: ratatui::layout::Rect,
    ) {
        let ChromeView { pty_screen, pane_overlay, drawer, term_screen, mut term_size_observed, .. } = *self;
        let pty_size_observed;
        // LLM pane: paint the vt100 terminal grid into the
        // BL content rect. Walk every cell of the emulator
        // screen at (row, col), look up its glyph + colour,
        // and write into the chrome buffer at the matching
        // (llm_rect.x + col, llm_rect.y + row). The emulator
        // was sized to llm_rect earlier, so the grid fits
        // exactly.
        paint_terminal(buf, llm_rect, &pty_screen);
        // ADR 0030 §8 "Where it is shown", widened by ADR 0045
        // decision 1 (Codex review): overlays the persistent reason
        // line, and under it the discarded-input count, whenever
        // either is set —
        // whatever `pty_screen` actually painted underneath,
        // including a checkpointed client's own now-STALE frozen
        // content (a live failure/retry must never hide behind
        // real-but-old output), not only the dead-uncheckpointed
        // fallback to the (usually blank, unrelated) tmux screen
        // this originally covered.
        if llm_rect.width > 2 {
            for (row, line) in pane_overlay.iter().enumerate().take(llm_rect.height as usize) {
                write_title(
                    buf,
                    llm_rect.x + 1,
                    llm_rect.y + row as u16,
                    line,
                    llm_rect.width - 2,
                    Style::default().fg(Color::Yellow),
                );
            }
        }
        pty_size_observed = (llm_rect.width, llm_rect.height);
        // G3: local terminal drawer — paint its vt100 grid into the
        // drawer rect (same renderer as the LLM pane). Record the
        // rect so the PTY can be resized to match after the closure.
        if drawer == DrawerContent::Terminal && repl_rect.width > 0 {
            if let Some(scr) = term_screen {
                paint_terminal(buf, repl_rect, scr);
            }
            term_size_observed = (repl_rect.width, repl_rect.height);
        }
        // (The active-workspace indicator is now the bottom session
        // strip — all sessions, active centered + bold — drawn as a
        // pixel-positioned overlay after this ratatui pass via
        // `session_strip_lines`. It supersedes the old single
        // centered marker that used to paint here.)
        self.pty_size_observed = pty_size_observed;
        self.term_size_observed = term_size_observed;
    }
}

fn render_nav_widgets(
    frame: &mut ratatui::Frame<'_>,
    nav_body: Paragraph<'static>,
    nav_list_rect: ratatui::layout::Rect,
    nav_pinned: Vec<String>,
    nav_rect: ratatui::layout::Rect,
    nav_list_h: usize,
    prompt_open: bool,
) {
        // Render content widgets into the interior content
        // rects (no borders). The drawer's REPL scrollback + input
        // only render when the drawer is actually showing the REPL;
        // when it shows the Terminal (G3) the vt100 grid is painted
        // into `repl_rect` after the wireframe instead.
        frame.render_widget(nav_body, nav_list_rect);
        frame.render_widget(
            Paragraph::new(
                nav_pinned
                    .iter()
                    .map(|r| RtLine::from(Span::styled(r.clone(), Style::default().fg(Color::LightGreen))))
                    .collect::<Vec<_>>(),
            ),
            ratatui::layout::Rect {
                y: nav_rect.y + nav_list_h as u16,
                height: nav_pinned.len() as u16,
                ..nav_rect
            },
        );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_pinned_cells_have_attention_style() {
        let prompts = [
            NavPrompt::CreateFile { dir_node_id: "files:".into(), input: "name".into() },
            NavPrompt::ConfirmDelete { node_id: "files:a".into(), label: "a".into() },
            NavPrompt::ConfirmQuit { keep: false },
            NavPrompt::ScaleEntry { node_id: "files:a".into(), input: "1".into() },
        ];
        for prompt in prompts.iter().map(Some).chain(std::iter::once(None)) {
            for (width, height) in [(80, 8), (16, 8), (16, 1), (16, 0)] {
                let text = prompt.map(|p| match p {
                    NavPrompt::ConfirmQuit { keep } => quit_prompt_line(*keep),
                    _ => ("a wrapped navigation prompt with a choice".into(), "[choice]".into()),
                });
                let (list_h, rows, _) = nav_pinned_rows(text.as_ref().map(|(a,b)| (a.as_str(),b.as_str())),
                    Some("notice"), None, width, height);
                let rect = ratatui::layout::Rect::new(0, 0, width as u16, height as u16);
                let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width as u16, height as u16)).unwrap();
                term.draw(|frame| render_nav_widgets(frame, Paragraph::new(""),
                    ratatui::layout::Rect { height: list_h as u16, ..rect }, rows.clone(), rect, list_h, prompt.is_some())).unwrap();
                for (i, row) in rows.iter().enumerate() {
                    for x in 0..row.chars().count().min(width) {
                        let cell = &term.backend().buffer()[(x as u16, (list_h + i) as u16)];
                        assert_eq!(cell.fg, if prompt.is_some() { Color::Yellow } else { Color::LightGreen }, "prompt foreground");
                        assert_eq!(cell.modifier.contains(Modifier::BOLD), prompt.is_some(), "prompt weight");
                    }
                }
                if height == 1 && prompt.is_some() { assert!(rows[0].contains('['), "whole choice survives clipping"); }
                if height == 0 { assert!(rows.is_empty()); }
            }
        }
    }
}
