//! The agent pane's input: held keys by row, sends into the shown run, and local mouse selection.

use super::*;

pub(in crate::ui) const HELD_SEND_DWELL: Duration = Duration::from_secs(1);

/// One entry per typed key or paste, so Backspace removes exactly the last input.
#[derive(Default)]
pub(super) struct HeldKeys {
    pub(super) entries: Vec<Vec<u8>>,
    pub(super) bytes: usize,
}

/// The pane's count of discarded keystrokes and what a frame drew of the notice (its row and the total shown). The
/// fields are private and `dismiss` is the only clear, so nothing but a person's act, after the frame drew the count,
/// takes the notice away; a discard since the last frame is unseen and stays.
#[derive(Default)]
pub(super) struct DiscardNotice {
    count: usize,
    drawn: Option<(u16, usize)>,
}

impl DiscardNotice {
    pub(super) fn add(&mut self, n: usize) {
        self.count += n;
        self.drawn = None;
    }
    pub(super) fn count(&self) -> usize { self.count }
    /// What this frame drew: the notice's row and the total shown, or nothing.
    pub(super) fn drew(&mut self, drawn: Option<(u16, usize)>) { self.drawn = drawn; }
    pub(super) fn on_row(&self, row: u16) -> bool { self.drawn.is_some_and(|(r, _)| r == row) }
    /// Clears the count when the frame drew all of `total`; whether it did.
    fn dismiss(&mut self, total: usize) -> bool {
        let all_drawn = self.drawn.is_some_and(|(_, n)| n == total);
        if all_drawn { *self = Self::default(); }
        all_drawn
    }
}

impl<C: PaneClient> AgentPane<C> {
    /// The pane's count and the client's own, the number the notice shows.
    pub(super) fn discard_total(&self) -> usize {
        self.discard.count() + self.pane_attach_term.as_ref().map_or(0, |c| c.inputs_discarded())
    }

    /// A person's act (a key held or sent, a click on the notice) dismisses the notice, and clears the client's count
    /// with the pane's.
    fn dismiss_discard_notice(&mut self) {
        let total = self.discard_total();
        if self.discard.dismiss(total) {
            if let Some(c) = self.pane_attach_term.as_mut() { c.clear_inputs_discarded(); }
        }
    }

    /// The one routing point for typed keys and every paste. Unshown input stays with its row;
    /// once a row has held keys, later typing cannot pass them. Only an armed Enter releases them.
    /// False when the input was refused and counted.
    pub(in crate::ui) fn send_input(&mut self, bytes: &[u8]) -> bool {
        let live = self.refresh_live();
        let Some(target) = self.bl_pane_target.clone() else {
            self.discard.add(1);
            return false;
        };
        if !live || self.held_keys.contains_key(&target) {
            let held_bytes = self.held_keys.get(&target).map_or(0, |h| h.bytes);
            if held_bytes.saturating_add(bytes.len()) > sot_capsule::attach_client::attach::HELD_INPUT_CAP {
                self.discard.add(1);
                return false;
            } else if !bytes.is_empty() {
                self.dismiss_discard_notice();
                let held = self.held_keys.entry(target).or_default();
                held.bytes += bytes.len();
                held.entries.push(bytes.to_vec());
            }
        } else {
            self.dismiss_discard_notice();
            let c = self.pane_attach_term.as_mut().expect("live client");
            let run = c.screen_run().to_owned();
            c.send_input_in_run(bytes, &run);
        }
        true
    }

    /// A plain logical key takes the held-input path before ordinary terminal encoding.
    /// `None` leaves the key to the usual path, including Enter/Esc/Backspace with nothing held.
    pub(in crate::ui) fn held_key(&mut self, logical: &Key, repeat: bool, unmodified: bool, now: Instant) -> Option<String> {
        let target = self.bl_pane_target.clone()?;
        let count = self.held_keys.get(&target)?.entries.len();
        if !unmodified { return None; }
        match logical {
            Key::Named(NamedKey::Escape) => {
                self.dismiss_discard_notice();
                self.held_keys.remove(&target);
                Some(format!("dropped {count} held inputs, not sent"))
            }
            Key::Named(NamedKey::Backspace) => {
                self.dismiss_discard_notice();
                let held = self.held_keys.get_mut(&target).expect("selected held keys");
                if let Some(last) = held.entries.pop() { held.bytes -= last.len(); }
                if held.entries.is_empty() { self.held_keys.remove(&target); }
                Some("removed the last held key or paste".into())
            }
            Key::Named(NamedKey::Enter) => {
                if !self.refresh_live() {
                    if !self.send_input(b"\r") { return Some("Enter refused: held keys are full".into()); }
                    return Some("Enter held, not sent; wait for the row's screen".into());
                }
                let c = self.pane_attach_term.as_ref().expect("live client");
                if c.run() != c.screen_run() {
                    self.clear_live();
                    return Some("row restarted: held keys kept; the screen must show again".into());
                }
                if repeat { return Some("held keys not sent: release Enter and press it again".into()); }
                if !self.held_armed(now) { return Some("held keys not sent: wait for Enter to arm".into()); }
                self.dismiss_discard_notice();
                let held = self.held_keys.remove(&target).expect("selected held keys");
                let c = self.pane_attach_term.as_mut().expect("live client");
                let run = c.screen_run().to_owned();
                c.send_input_in_run(&held.entries.concat(), &run);
                Some(format!("sent {count} held inputs"))
            }
            _ => None,
        }
    }

    pub(super) fn held_armed(&self, now: Instant) -> bool {
        self.live_since.is_some_and(|since| now.saturating_duration_since(since) >= HELD_SEND_DWELL)
    }

    /// Inclusive linear range from client-or-blank. A row the terminal wrapped joins the next with no newline and
    /// keeps its trailing spaces; a row the program ended is trimmed and followed by a newline. `one_line` (Copy as
    /// one line) makes every other break one space with the next row's indent dropped, no space where the row
    /// reached the last column and the word goes on below it (`cut_word`, the selection's continuation indent); a blank row is skipped, so a gap
    /// between paragraphs is one space too.
    /// State writes the owned text to the clipboard, then reports success through `copy_finished`.
    pub(in crate::ui) fn copy_text(&self, pane_cols: u16, one_line: bool) -> Option<String> {
        let Some(sel) = self.llm_selection else {
            return None;
        };
        let (a, b) = sel;
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        let (sr, sc) = start;
        let (er, ec) = end;
        if pane_cols == 0 {
            return None;
        }
        // Copy reads client-or-blank even when painting uses the departing hold.
        let blank;
        let screen = match self.pane_attach_term.as_ref().map(|t| t.screen()) {
            Some(s) => s,
            None => {
                let (cols, rows) = self.pty_size.unwrap_or((80, 24));
                blank = blank_pane_screen(cols, rows);
                &blank
            }
        };
        let indent = if one_line { continuation_indent(screen, sr..=er, pane_cols) } else { None };
        let mut out = String::new();
        for row in sr..=er {
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
                        if cell.is_wide_continuation() {
                            continue;
                        }
                        if g.is_empty() {
                            // The cell a wide character left empty at a wrap is no text.
                            if col != pane_cols - 1 || row == er || !screen.row_wrapped(row) {
                                line.push(' ');
                            }
                        } else {
                            line.push_str(g);
                        }
                    }
                    None => line.push(' '),
                }
            }
            if row == er || !screen.row_wrapped(row) {
                line.truncate(line.trim_end_matches(' ').len());
            }
            if row != sr && !screen.row_wrapped(row - 1) {
                if !one_line {
                    out.push('\n');
                } else if line.trim_start().is_empty() {
                    continue;
                } else {
                    if !out.is_empty() && !cut_word(screen, row - 1, pane_cols, indent) { out.push(' '); }
                    line = line.trim_start().to_owned();
                }
            }
            out.push_str(&line);
        }
        if one_line { out.truncate(out.trim_end_matches(' ').len()); }
        if out.is_empty() {
            return None;
        }
        Some(out)
    }
    pub(in crate::ui) fn reset_scroll(&mut self) {
        self.roll.stop();
        if let Some(c) = self.pane_attach_term.as_mut() { c.reset_scroll(); }
    }

    pub(in crate::ui) fn press(&mut self, cell: Option<(u16, u16)>) -> bool {
        if let Some(cell) = cell {
            if self.discard.on_row(cell.0) { self.dismiss_discard_notice(); }
            self.llm_selection = Some((cell, cell));
            self.llm_drag_active = true;
            true
        } else { self.llm_selection.take().is_some() }
    }
    pub(in crate::ui) fn extend_drag(&mut self, cell: Option<(u16, u16)>) -> bool {
        if self.llm_drag_active {
            if let (Some((start, _)), Some(end)) = (self.llm_selection, cell) {
                self.llm_selection = Some((start, end));
                return true;
            }
        }
        false
    }
    pub(in crate::ui) fn release(&mut self) { self.llm_drag_active = false; }
    pub(in crate::ui) fn copy_finished(&mut self, success: bool) {
        if success { self.llm_selection = None; }
    }
    pub(in crate::ui) fn alternate_screen(&self) -> bool {
        self.pane_feed == PaneFeed::Capsule && self.pane_attach_term.as_ref().is_some_and(|c| c.screen().alternate_screen())
    }
    /// True consumes the page key; alternate-screen applications receive its original bytes.
    pub(in crate::ui) fn page(&mut self, up: bool, height: u16, now: Instant, length: Duration) -> bool {
        if self.alternate_screen() { return false; }
        if self.pane_feed == PaneFeed::Capsule {
            let step = (height as i32 / 3).max(1);
            self.ask_scroll(if up { step } else { -step }, now, length);
        }
        true
    }

    /// A person asked the local ring to move by `rows` (positive is older): a row at a time over `length`, at once
    /// when it is zero. A program that owns its screen is scrolled by nothing here, so it moves at once.
    fn ask_scroll(&mut self, rows: i32, now: Instant, length: Duration) {
        let Some(c) = self.pane_attach_term.as_mut() else { return };
        if c.screen().alternate_screen() { ring(c)(rows); } else { self.roll.ask(rows, now, length, ring(c)); }
    }

    /// One frame of what a person's scroll owes, after the pump. Whether frames must keep coming.
    pub(in crate::ui) fn spend_scroll(&mut self, now: Instant) -> bool {
        let Some(c) = self.pane_attach_term.as_mut() else { self.roll.stop(); return false };
        self.roll.spend(now, c.screen().size().0 as f32, ring(c))
    }

    pub(in crate::ui) fn wheel(&mut self, rows: i32, now: Instant, length: Duration) -> WheelEffect {
        let live = self.refresh_live();
        if self.pane_feed == PaneFeed::Pending { return WheelEffect::Pending; }
        let Some(c) = self.pane_attach_term.as_mut() else { return WheelEffect::ClearResidue };
        if live && c.mouse_tracking_on() {
            let button = if rows > 0 { 64 } else { 65 };
            let seq = format!("\x1b[<{button};1;1M");
            let run = c.screen_run().to_owned();
            for _ in 0..rows.unsigned_abs().min(8) { c.send_input_in_run(seq.as_bytes(), &run); }
        } else { self.ask_scroll(rows, now, length); }
        WheelEffect::Redraw
    }
}

/// The client's ring as `RingRoll` moves it: by `rows` (none reads it), answering the offset after.
pub(super) fn ring<C: PaneClient>(c: &mut C) -> impl FnMut(i32) -> usize + '_ {
    move |rows| {
        if rows != 0 { c.scroll(rows); }
        c.screen().scrollback()
    }
}

pub(in crate::ui) enum WheelEffect { Pending, ClearResidue, Redraw }

impl State {
    pub(in crate::ui) fn send_pane_input(&mut self, bytes: &[u8]) { self.agent_pane.send_input(bytes); }
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
        let cell_w = self.canvas.cell_w().max(1.0);
        let cell_h = self.canvas.cell_h().max(1.0);
        let origin_x = self.canvas.origin_x() + rect.x as f32 * cell_w;
        let origin_y = self.canvas.origin_y() + rect.y as f32 * cell_h;
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

    pub(in crate::ui) fn copy_llm_selection(&mut self, one_line: bool) -> bool {
        let Some(out) = self.agent_pane.copy_text(self.pane_rects.llm.width, one_line) else { return false };
        match crate::ui::input::write_clipboard_text(out.clone()) {
            Ok(()) => {
                tracing::info!(bytes = out.len(), "llm.copy → clipboard");
                self.agent_pane.copy_finished(true);
                true
            }
            Err(e) => {
                tracing::warn!(error = %e, "clipboard write failed; selection kept");
                self.agent_pane.copy_finished(false);
                false
            }
        }
    }
}

/// Whether the word that ends `row` of a screen `cols` wide goes on in the next row: the program cut a word wider
/// than its row at the last column (Claude Code's wrap-ansi, `trim: false, hard: true`), so the two pieces are one
/// word. Only a row that reaches the last column can be cut. `indent` is the selection's own continuation indent
/// (`continuation_indent`): a next row starting there continues the cut word, and one starting a blank further in
/// is the row wrap-ansi starts with a space after an exactly full one, so the word ended at the edge. Without an
/// indent (one continuation row) the row's last word and the next row's first must not fit one row together.
fn cut_word(screen: &vt100::Screen, row: u16, cols: u16, indent: Option<u16>) -> bool {
    let solid = |r: u16, c: u16| screen.cell(r, c).is_some_and(|x| x.is_wide_continuation() || !x.contents().trim().is_empty());
    let Some(end) = (0..cols).rev().find(|&c| solid(row, c)).map(|c| c + 1) else { return false };
    let Some(next) = (0..cols).find(|&c| solid(row + 1, c)) else { return false };
    if end != cols { return false; }
    match indent {
        Some(indent) => next <= indent,
        None => {
            let run = (0..end).rev().take_while(|&c| solid(row, c)).count();
            let word = (next..cols).take_while(|&c| solid(row + 1, c)).count();
            run + word > usize::from(cols - next)
        }
    }
}

/// The first-text column the rows after the selection's first share: the smallest of them, when at least two rows
/// are not blank (one row cannot be told from a row that starts one blank further in).
fn continuation_indent(screen: &vt100::Screen, rows: std::ops::RangeInclusive<u16>, cols: u16) -> Option<u16> {
    let leads: Vec<u16> = rows
        .skip(1)
        .filter_map(|r| (0..cols).find(|&c| screen.cell(r, c).is_some_and(|x| x.is_wide_continuation() || !x.contents().trim().is_empty())))
        .collect();
    if leads.len() < 2 { return None; }
    leads.into_iter().min()
}
