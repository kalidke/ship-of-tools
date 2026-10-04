//! The agent pane's input: bytes to the selected row's client, and mouse selection over its screen.

use super::*;

impl State {
    /// ADR 0042 slice L1b fix 2/3, narrowed post-notmux (every row is a
    /// capsule; there is no tmux fallback anymore): routes session-pane
    /// input bytes to the capsule client's own `send_input`, or counts them
    /// as discarded while resolution is still `Pending`. The
    /// ONE routing point every input source (typed keystrokes, LLM-pane
    /// paste, ROI paste) shares, so a future source only has to call
    /// this rather than re-derive the branch.
    pub(in crate::ui) fn send_pane_input(&mut self, bytes: &[u8]) {
        match self.pane_feed {
            PaneFeed::Capsule => {
                if let Some(t) = self.pane_attach_term.as_mut() {
                    t.send_input(bytes);
                } else {
                    // (a logic bug) `pane_feed` says Capsule but there's
                    // no live client — count the drop, never silent.
                    self.pane_inputs_discarded += 1;
                }
            }
            PaneFeed::Pending => {
                self.pane_inputs_discarded += 1;
            }
        }
    }

    /// Convert a physical-pixel cursor position into LLM-pane cell coords
    /// `(row, col)`. `strict` rejects positions outside the pane rect
    /// (used for mouse-down — don't start a selection on click outside);
    /// when false, the position is clamped to the pane (used for drag —
    /// allow extending past the pane edge). Returns `None` when the LLM
    /// pane has no area (zero rows or cols, e.g. layout collapsed).
    pub(in crate::ui) fn llm_cell_at_px(&self, px: (f32, f32), strict: bool) -> Option<(u16, u16)> {
        let rect = self.pane_rects.llm;
        if rect.width == 0 || rect.height == 0 {
            return None;
        }
        let cell_w = self.cell_w.max(1.0);
        let cell_h = self.cell_h.max(1.0);
        let origin_x = self.chrome_origin_x + rect.x as f32 * cell_w;
        let origin_y = self.chrome_origin_y + rect.y as f32 * cell_h;
        let dx = px.0 - origin_x;
        let dy = px.1 - origin_y;
        let pane_w_px = rect.width as f32 * cell_w;
        let pane_h_px = rect.height as f32 * cell_h;
        if strict && (dx < 0.0 || dy < 0.0 || dx >= pane_w_px || dy >= pane_h_px) {
            return None;
        }
        let col = (dx / cell_w).floor().clamp(0.0, rect.width as f32 - 1.0) as u16;
        let row = (dy / cell_h).floor().clamp(0.0, rect.height as f32 - 1.0) as u16;
        Some((row, col))
    }

    /// Walk the LLM-pane selection range in the vt100 grid, build a UTF-8
    /// string, and push it to the OS clipboard via `arboard`. Linear range
    /// (terminal-style line wrap, not rectangular); trailing whitespace on
    /// each line trims. Clears `llm_selection` on success. Returns `true`
    /// iff something was written.
    pub(in crate::ui) fn copy_llm_selection(&mut self) -> bool {
        let Some(sel) = self.llm_selection else {
            return false;
        };
        let (a, b) = sel;
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        let (sr, sc) = start;
        let (er, ec) = end;
        let pane_cols = self.pane_rects.llm.width;
        if pane_cols == 0 {
            return false;
        }
        // ADR 0042 slice L1b: same source as the paint path (`redraw`'s
        // `pty_screen`) — a capsule attach's own screen when one is
        // live, else blank. Selecting text against the wrong (idle)
        // screen would copy stale or blank content.
        let blank;
        let screen = match self.pane_attach_term.as_ref().map(|t| t.screen()) {
            Some(s) => s,
            None => {
                let (cols, rows) = self.pty_size.unwrap_or((80, 24));
                blank = blank_pane_screen(cols, rows);
                &blank
            }
        };
        let mut out = String::new();
        for row in sr..=er {
            if row != sr {
                out.push('\n');
            }
            let cs = if row == sr { sc } else { 0 };
            let ce = if row == er {
                ec
            } else {
                pane_cols.saturating_sub(1)
            };
            let mut line = String::new();
            for col in cs..=ce {
                match screen.cell(row, col) {
                    Some(cell) => {
                        let g = cell.contents();
                        if g.is_empty() {
                            line.push(' ');
                        } else {
                            line.push_str(g);
                        }
                    }
                    None => line.push(' '),
                }
            }
            while line.ends_with(' ') {
                line.pop();
            }
            out.push_str(&line);
        }
        if out.is_empty() {
            return false;
        }
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(out.clone())) {
            Ok(()) => {
                tracing::info!(bytes = out.len(), "llm.copy → clipboard");
                self.llm_selection = None;
                true
            }
            Err(e) => {
                tracing::warn!(error = %e, "clipboard write failed; selection kept");
                false
            }
        }
    }
}
