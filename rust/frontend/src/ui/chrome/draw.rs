//! The chrome draw: `draw_chrome` snapshots the frame's chrome inputs, lends them to the ratatui draw as a
//! `ChromeView` and copies its results back; the snapshot pieces it calls.

use super::*;

impl State {
    pub(in crate::ui) fn draw_chrome(
        &mut self,
    ) -> Result<(
        ratatui::layout::Rect,
        ratatui::layout::Rect,
        ratatui::layout::Rect,
        (usize, usize),
        Option<((u16, u16), (u16, u16))>,
        Vec<(HostKey, u32)>,
        Option<String>,
        bool,
        bool,
    )> {
        // Single preview pane rect that the preview-layer surface draws
        // into. The exact source (PNG quad / SVG quad / cosmic-text
        // markdown buffer / cosmic-text concept buffer) is picked below
        // via priority cascade so the pane "switches based on context"
        // per the user's layout intent.
        let mut preview_cells = ratatui::layout::Rect::default();
        // Cell-rect of the bottom drawer (REPL/Terminal/Monitor share it),
        // carried out of the closure the same way as `preview_cells` so the
        // Ctrl+M monitor chart quad can be sized to the drawer rect after the
        // draw returns.
        let mut repl_cells = ratatui::layout::Rect::default();
        // Scrollback sub-rect + visible line window, exported for the
        // inline REPL image paint pass (same borrow pattern as repl_cells).
        let mut repl_scrollback_cells = ratatui::layout::Rect::default();
        let mut repl_window: (usize, usize) = (0, 0);
        // Cache the four pane content rects + clamped REPL scroll across
        // the closure. Same pattern as `preview_cells` — captured by
        // mutable borrow inside the draw closure, then written back to
        // self after `draw` returns.
        let mut new_pane_rects = self.pane_rects;
        let mut new_repl_scroll = self.repl_scroll;
        // Nav-spill overlay rows collected by this draw (same captured-
        // local pattern as `preview_cells`); written to
        // `self.nav_spill_segments` after `draw` returns for the render-
        // pass tail to paint.
        let mut nav_spill_segs_out: Vec<NavSpillSeg> = Vec::new();

        let (status, nav_prompt_line, owed, owed_line, nav_line) = self.nav_pinned_text();
        let mut owed_drawn = false;
        let mut leaving_drawn = false;
        let lease_notice = self.leases.notice();
        let (clock_now, battery, last_key, version_stamp, version_skew) = self.chrome_labels();
        let mode = self.mode;
        let focus = self.focus;
        let help_context = self.help_context();
        let maximize_slot =
            maximize_slot(self.maximized, focus, self.leaving.as_ref().and_then(|l| l.line()).is_some());
        // State-nav selected-session contrast lever, snapshotted for the draw
        // closure (it mustn't borrow `self`).
        let contrast_dim = self.contrast_dim;
        // Owner ruling (2026-09-06): the wordmark gets the nav pane's own first
        // row, top-left -- right-aligned on the first tree row it collided with
        // text on every non-ultrawide. Snapshotted here: the draw closure must
        // not borrow `self`.
        let nav_logo_row = self.wordmark_quad.is_some();
        self.decode_repl_images();
        let repl_lines;
        (repl_lines, new_repl_scroll) = self.build_repl_view(new_repl_scroll);
        let repl_input = self.repl_input.clone();
        let repl_pkg_mode = self.repl_pkg_mode;
        let (nav_cursor_body_pos, nav_has_cursor) = self.nav_cursor_row();
        // Mutable copy of the persistent scroll. The draw closure updates
        // this in place based on the cursor's viewport position; the
        // result is written back to self.tree_scroll after the draw.
        let mut nav_scroll = self.tree_scroll;
        self.fire_due_read_mark();
        if self.pane_attach_term.is_some() {
            self.pump_pane_attach_term();
        }
        self.pump_drawer_terminals();
        // Re-read after a possible spawn-failure close above; this is the
        // value the renderer branches on.
        let drawer = self.drawer;
        let (pane_screen, pane_overlay) = self.session_pane_view();
        let blank_pty_screen;
        let pty_screen = match match pane_screen {
            PaneScreen::Client => self.pane_attach_term.as_ref().map(|t| t.screen()),
            PaneScreen::Hold => self.pane_hold.as_ref().map(|h| h.screen()),
            PaneScreen::Empty => None,
        } {
            Some(s) => s,
            None => {
                let (cols, rows) = self.pty_size.unwrap_or((80, 24));
                blank_pty_screen = blank_pane_screen(cols, rows);
                &blank_pty_screen
            }
        };
        #[cfg(windows)]
        let attach_screen = self.attach_term.as_ref().map(|t| t.screen());
        #[cfg(not(windows))]
        let attach_screen: Option<&vt100::Screen> = None;
        let term_screen = if drawer == DrawerContent::Terminal {
            self.local_term
                .as_ref()
                .map(|t| t.screen())
                .or(attach_screen)
        } else {
            None
        };
        let llm_selection = self.llm_selection;
        // Captured by the closure and written when the LLM pane's
        // content rect is final; read after the closure to decide
        // whether to fire `pty.open` / `pty.resize`.
        let mut pty_size_observed: (u16, u16) = (0, 0);
        // Same idea for the local terminal drawer: capture the final
        // drawer rect in the closure, resize the PTY to match after.
        let mut term_size_observed: (u16, u16) = (0, 0);
        let concept_status = self.concept_status_line();
        let selected_stale = self.selected_row_stale();
        let (tree_lines, tree_empty): (Vec<NavRow>, bool) = if let Some(p) = &self.workspace_picker
        {
            self.picker_nav_rows(p)
        } else {
            self.tree_nav_rows(selected_stale)
        };
        // Transient nav spill: a nav-cursor move (re)arms the timer; while
        // it runs, nav rows whose text overflows the nav column render
        // their FULL text as a floating overlay across the preview pane's
        // left edge (segment collection below in the draw closure; painted
        // by the overlay text layer at the end of the render pass — pane
        // geometry never moves). It vanishes `[nav] spill_ms` after the
        // last move. Detected here frame-side, by diffing the cursor
        // tuple, instead of in every input path — keys, wheel, mode and
        // workspace switches all trigger uniformly. The picker cursor
        // rides in the tuple so workspace-picker browsing spills too.
        let spill_cursor_now = (
            self.mode,
            self.active_workspace_id.clone(),
            self.tree.selected,
            self.workspace_picker.as_ref().map(|p| p.selected),
            self.tree.generation,
        );
        if self.nav_spill_cursor.as_ref() != Some(&spill_cursor_now) {
            // Arm ONLY on a user cursor move: same mode+workspace, same tree
            // CONTENT (generation), different cursor. Everything else that
            // perturbs the tuple — boot, the async initial tree load, a
            // cursor restore, a refresh splicing rows around the cursor, a
            // mode/workspace switch swapping the tree — updates the baseline
            // without arming (codex review: first-frame-only suppression
            // made startup spill timing-dependent).
            let user_move = match (self.nav_spill_cursor.as_ref(), &spill_cursor_now) {
                (Some((pm, pw, ps, pp, pg)), (m, w, s, p, g)) => {
                    pm == m && pw == w && pg == g && (ps != s || pp != p)
                }
                (None, _) => false,
            };
            self.nav_spill_cursor = Some(spill_cursor_now);
            if user_move && self.settings.nav_spill_ms > 0 {
                self.nav_spill_until = Some(
                    std::time::Instant::now()
                        + std::time::Duration::from_millis(self.settings.nav_spill_ms),
                );
            }
        }
        let nav_spill_active = self
            .nav_spill_until
            .map(|u| std::time::Instant::now() < u)
            .unwrap_or(false);
        let layout_preset = self.layout_preset_for(drawer);
        // `drawer` was bound above (after the terminal lazy-spawn/close).
        let drawer_open = drawer.is_open();
        let (preview_name, monitor_host_label, monitor_host_connected) = self.pane_title_inputs();
        // Borrowed LAST, after every `&mut self` pump above (the Windows-only
        // attach-term pumps included): the draw closure captures these two
        // references, so taking them any earlier spans those mutations and
        // fails the borrow check on the platform that has them.
        let help_state = &self.help;
        let help_bindings = &self.bindings;
        let mut view = ChromeView {
            layout_preset: &layout_preset, drawer_open, maximize_slot, nav_logo_row, focus, mode,
            nav_prompt_line: &nav_prompt_line, lease_notice, nav_line: &nav_line, owed_line: &owed_line,
            status: &status, nav_cursor_body_pos, tree_empty, contrast_dim, concept_status: &concept_status,
            last_key: &last_key, nav_has_cursor, nav_spill_active, preview_name: &preview_name,
            pinned_preview_node_id: &self.pinned_preview_node_id, drawer, monitor_host_connected,
            monitor_host_label: &monitor_host_label, repl_input: &repl_input, repl_lines: &repl_lines,
            repl_pkg_mode, help_state, help_bindings, help_context: &help_context, clock_now,
            battery: &battery, version_stamp: &version_stamp, version_skew, pty_screen,
            pane_overlay: &pane_overlay, term_screen, owed_drawn, leaving_drawn, nav_scroll, preview_cells,
            repl_cells, new_pane_rects, new_repl_scroll, repl_scrollback_cells, repl_window,
            pty_size_observed, term_size_observed,
        };
        self.terminal
            .draw(|frame| {
                view.paint(frame, tree_lines, &mut nav_spill_segs_out);
            })
            .context("ratatui draw failed")?;
        ChromeView {
            owed_drawn, leaving_drawn, nav_scroll, preview_cells, repl_cells, new_pane_rects, new_repl_scroll,
            repl_scrollback_cells, repl_window, pty_size_observed, term_size_observed, ..
        } = view;
        // Persist the scroll the draw closure landed on so the next
        // frame starts from the same offset (sticky behaviour); the
        // closure can't write to self.tree_scroll directly because the
        // ratatui draw API takes a &mut self method, not self.
        self.tree_scroll = nav_scroll;
        self.repl_scroll = new_repl_scroll;
        self.pane_rects = new_pane_rects;
        self.nav_spill_segments = nav_spill_segs_out;

        self.sync_pane_pty_size(pty_size_observed);

        self.sync_terminal_drawer_size(term_size_observed);
        Ok((preview_cells, repl_cells, repl_scrollback_cells, repl_window, llm_selection, owed, owed_line, owed_drawn, leaving_drawn))
    }

    fn nav_pinned_text(
        &self,
    ) -> (String, Option<(String, String)>, Vec<(HostKey, u32)>, Option<String>, Option<String>) {
        // A NavTree prompt, the not-ended count or `closing…`, and the lease
        // notice are pinned under the nav list (`nav_pinned_rows`), so no
        // scroll hides what Enter would confirm. A text prompt is the
        // input field the user types into; a block cursor (▏) marks the
        // insertion point.
        let status = self.status.clone();
        let nav_prompt_line = match &self.nav_prompt {
            Some(NavPrompt::CreateFile { input, .. }) => Some((format!("new file or dir/: {input}▏"), String::new())),
            Some(NavPrompt::ConfirmDelete { label, .. }) => Some((format!("delete {label}? [y/N]"), String::new())),
            Some(NavPrompt::ScaleEntry { input, .. }) => Some((format!("pixel size (nm): {input}▏"), String::new())),
            Some(NavPrompt::ConfirmQuit { keep }) => Some(quit_prompt_line(*keep)),
            None => None,
        };
        // The counts owed, read once: this frame draws their sum, and the frame that
        // presents it whole acks exactly these; then the line holds as `not_ended_shown`.
        let owed = self.leases.owed();
        let owed_line = crate::lease::not_ended_line(owed.iter().map(|(_, n)| n).sum());
        let nav_line = self.leaving.as_ref().and_then(|l| l.line()).or_else(|| owed_line.clone()).or_else(|| {
            let now = std::time::Instant::now();
            self.not_ended_shown.as_ref().filter(|(_, until)| now < *until).map(|(l, _)| l.clone())
        });
        (status, nav_prompt_line, owed, owed_line, nav_line)
    }

    fn chrome_labels(&mut self) -> (chrono::NaiveDateTime, Option<String>, Option<String>, String, bool) {
        // Local wall-clock of the machine running the frontend, sampled once
        // per frame and turned into the top-right chrome clock text at the
        // paint site below (`clock_label`, which also prefixes the date
        // when there's room). `chrono::Local` is cross-platform (same
        // behaviour on Windows/macOS/Linux); the once-per-second repaint is
        // scheduled in `about_to_wait`, which also covers the date rolling
        // over at midnight — no separate timer needed.
        let clock_now = chrono::Local::now().naive_local();
        // Battery readout painted just left of the clock. The OS query isn't
        // free, so refresh the cache at most once per `BATTERY_QUERY_INTERVAL`
        // (the clock repaints ~1×/s and reuses the cached value between
        // refreshes). `None` => no battery / query failed => paint nothing.
        self.refresh_battery_label();
        let battery = self.battery_label.clone();
        let last_key = self.last_key.clone();
        // FE/BE version stamp for the bottom chrome edge. Snapshotted here
        // with the other draw locals because the draw closure can't borrow
        // `self` again.
        let (version_stamp, version_skew) = version_label(
            &sot_protocol::app_version(),
            self.backend_version.as_deref(),
        );
        (clock_now, battery, last_key, version_stamp, version_skew)
    }

    fn nav_cursor_row(&self) -> (usize, bool) {
        // The navigation body begins with status + spacer. The picker adds
        // two path/header rows before its entries. Keep scroll and hit testing
        // aligned when the help legend moves from the body to the border.
        let (nav_cursor_body_pos, nav_has_cursor) = match &self.workspace_picker {
            Some(p) => (4usize.saturating_add(p.selected), !p.entries.is_empty()),
            None => (
                2usize.saturating_add(self.tree.selected),
                !self.tree.rows.is_empty(),
            ),
        };
        (nav_cursor_body_pos, nav_has_cursor)
    }

    fn concept_status_line(&self) -> String {
        // Annotation snapshot for the chrome status line. `fired` is what
        // we asked the backend about; `cached` matches when the response
        // is in hand for the current cursor. Three states: no target
        // (None/None), loading (Some/None or mismatched), and ready
        // (Some/Some with the same target).
        let concept_target = self.concept_target_fired.clone();
        let concept_status: String = match (&concept_target, &self.concept) {
            (None, _) => "annotation: (no target for this row)".to_string(),
            (Some(t), Some(info)) if info.target == *t => {
                if info.exists {
                    let drift = match (
                        info.synced_against.as_deref(),
                        info.target.strip_prefix("files/"),
                    ) {
                        (Some(synced), Some(path)) => match self.file_ast_hashes.get(path) {
                            Some(h) if h == synced => " · in sync",
                            Some(_) => " · STALE (file ast_hash differs from synced_against)",
                            None => match self.file_parse_retry.get(path) {
                                Some(&(_, n)) if n >= FILE_PARSE_MAX_RETRIES => {
                                    " · drift check unavailable (file.parse failing)"
                                }
                                _ => " · checking…",
                            },
                        },
                        (Some(_), None) => " · sync check N/A",
                        (None, _) => " · no synced_against frontmatter",
                    };
                    format!("annotation: present — {t}{drift}")
                } else {
                    format!("annotation: (none) — {t}")
                }
            }
            (Some(t), _) => format!("annotation: loading — {t}"),
        };
        concept_status
    }

    fn selected_row_stale(&self) -> bool {
        // Drift detection for the cursored row: we have both pieces of
        // information cached only for the selection (concept.read fires on
        // cursor move; file.parse fires once per visited path). Stale when
        // the annotation parses a `synced_against` AND the file's
        // `ast_hash` differs. Expanding to non-cursored rows needs a
        // per-row concept cache — phase 2.
        let selected_stale: bool = match (self.tree.rows.get(self.tree.selected), &self.concept) {
            (Some(row), Some(info)) if info.exists => {
                if let (Some(synced), Some(path)) = (
                    info.synced_against.as_ref(),
                    row.node.id.strip_prefix("files:"),
                ) {
                    self.file_ast_hashes
                        .get(path)
                        .map(|h| h != synced)
                        .unwrap_or(false)
                } else {
                    false
                }
            }
            _ => false,
        };
        selected_stale
    }

    fn picker_nav_rows(&self, p: &WorkspacePicker) -> (Vec<NavRow>, bool) {
        let mut rows: Vec<NavRow> = Vec::with_capacity(p.entries.len() + 4);
        rows.push((
            format!("workspace picker · {}", p.current_path),
            false,
            false,
            false,
            None,
            0.0,
            false,
            false,
        ));
        // Two footer rows: NAVIGATION first (→ is how you descend into
        // a folder — Enter does NOT, it creates), then the create keys.
        // Splitting them stops the common muscle-memory error of hitting
        // Enter to open a folder and instead spawning a session.
        rows.push((
            format!("  {} into · {} up · {} move · {} cancel",
                self.bindings.first_label(Action::NavExpand), self.bindings.first_label(Action::NavCollapse),
                self.bindings.first_label(Action::NavDown), self.bindings.first_label(Action::Cancel)),
            false,
            false,
            false,
            None,
            0.0,
            false,
            false,
        ));
        rows.push((
            format!("  {} Claude · {} bare · {} Codex", self.bindings.first_label(Action::SessionCreate),
                self.bindings.first_label(Action::SessionCreateBare), self.bindings.first_label(Action::SessionCreateCodex)),
            false,
            false,
            false,
            None,
            0.0,
            false,
            false,
        ));
        // Per-session accounts (owner-simplified brief, 2026-09-15):
        // hidden entirely when the daemon reports only "default" (or
        // never answered `accounts.list` — same empty state). A
        // never-logged-in folder is a NORMAL choice, not an error —
        // the row's own pane runs the login on first start — so it's
        // still selectable, just marked; this row renders with the
        // same dim treatment as the two footer rows above (no
        // agent/pinned/stale/selected tone applies to it).
        if p.account_choice_visible() {
            let acct = &p.accounts[p.account_selected];
            let marker = if acct.any_logged_in() { "" } else { " (not logged in)" };
            rows.push((
                format!("  account: {}{marker} · {} next", acct.name,
                    self.bindings.first_label(Action::SessionAccountNext)),
                false,
                false,
                false,
                None,
                0.0,
                false,
                // The only picker row naming a key you must press
                // BEFORE Enter: Enter commits the default account
                // immediately, so a dim hint here is one a user reads
                // past — reported from the field, 2026-09-15.
                true,
            ));
        }
        for (i, e) in p.entries.iter().enumerate() {
            let selected = i == p.selected;
            let caret = if selected { ">" } else { " " };
            let disclosure = if e.has_children { "▸" } else { "·" };
            rows.push((
                format!("{caret} {disclosure} {}/", e.name),
                selected,
                false,
                false,
                None,
                0.0,
                false,
                false,
            ));
        }
        let empty = p.entries.is_empty();
        (rows, empty)
    }

    fn tree_nav_rows(&self, selected_stale: bool) -> (Vec<NavRow>, bool) {
        let pinned_id = self.pinned_preview_node_id.as_deref();
        let now = chrono::Utc::now();
        let flash_now = std::time::Instant::now();
        let rows: Vec<NavRow> = self
            .tree
            .rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let selected = i == self.tree.selected;
                // ADR 0030 §8 decision 31c: a capsule row held by a
                // foreign build folds into the SAME "stale" colour
                // slot as annotation drift (both are cross-cutting
                // yellow, never mode-specific) — these two conditions
                // never both hold in practice (concept-annotation
                // staleness is computed only for `files:`-prefixed
                // rows, `is_foreign` only for `kind == "session"`
                // ones), so sharing the flag adds no new ambiguity.
                let is_foreign = capsule_row_is_foreign(
                    &r.node.kind,
                    r.node.payload.get("phase").and_then(|v| v.as_str()),
                );
                let stale = (selected && selected_stale) || is_foreign;
                let pinned = pinned_id == Some(r.node.id.as_str());
                // Agent tone + status-change flash only on Sessions
                // rows (kind "session"), keyed by the row's slug so it
                // matches the bottom strip. `pending` (badge floor, ADR
                // 0025 §1) is set when that workspace has a pending
                // nav.preview result waiting, keyed by the row's own
                // (host, slug) — ADR 0042 L2a codex review item E: a
                // slug-only check couldn't tell this host's row apart
                // from another host's same-slug pending badge.
                let (agent, flash, pending) = if r.node.kind == "session" {
                    let slug = r.node.payload.get("slug").and_then(|v| v.as_str());
                    let host = r.node.payload.get("host").and_then(|v| v.as_str());
                    let flash = host
                        .zip(slug)
                        .map(|(h, s)| self.flash_factor_for(h, s, flash_now))
                        .unwrap_or(0.0);
                    let pending = host
                        .zip(slug)
                        .map(|(h, s)| {
                            self.pending_nav
                                .contains_key(&(h.to_string(), s.to_string()))
                        })
                        .unwrap_or(false);
                    (agent_tone_for(&r.node.payload, now), flash, pending)
                } else {
                    (None, 0.0, false)
                };
                (
                    format_tree_row(r, selected, pinned),
                    selected,
                    stale,
                    pinned,
                    agent,
                    flash,
                    pending,
                    // No nav tree row is an attention row: the slot
                    // exists for picker affordances, not for content.
                    false,
                )
            })
            .collect();
        let empty = self.tree.rows.is_empty();
        (rows, empty)
    }

    fn layout_preset_for(&self, drawer: DrawerContent) -> crate::settings::LayoutPreset {
        // Layout proportions from the user's settings file (or
        // defaults). Snapshotted here so the ratatui closure doesn't
        // borrow `self`. Maximisation overrides the geom inside the
        // closure by passing a `maximize_slot`. Wide-preview rewrites
        // the preset itself (Llm column dropped, width to Preview) so
        // layout::compute needs no new inputs — the Llm-less path is
        // the same one the portrait preset already exercises.
        let mut layout_preset = {
            let p = self.settings.resolve_preset(self.monitor_aspect);
            if self.wide_preview {
                p.wide_preview()
            } else {
                p.clone()
            }
        };
        if drawer == DrawerContent::Help && layout_preset.drawer.is_none() {
            layout_preset.drawer = Some(crate::settings::Slot::Repl);
        }
        layout_preset
    }

    fn pane_title_inputs(&self) -> (Option<String>, HostKey, bool) {
        // T1: full path of the file the preview is showing, snapshotted here
        // so the draw closure doesn't borrow `self`.
        let preview_name = self.preview_pane_name();
        // 4.3: the monitor tab label's source, snapshotted here (String +
        // bool) for the same reason as `preview_name` above — the draw
        // closure must not borrow `self`.
        let monitor_host_label = self.monitor_host();
        let monitor_host_connected = self.conns.iter().any(|(h, _)| h == &monitor_host_label);
        // Sessions create-legend gate: is the workspace picker open? Snapshotted
        // here (Copy bool) so the header inside the draw closure can decide
        // whether to show the standalone three-key create legend without
        // borrowing `self`. Suppressed while the picker is open — the picker's
        // own footer already carries the legend inline.

        (preview_name, monitor_host_label, monitor_host_connected)
    }
}
