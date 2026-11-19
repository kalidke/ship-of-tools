//! The REPL drawer's wrap: a logical line longer than the pane continues on the next row, at the pane's width in cells.
//! The log holds the text whole; only these rows are cut, so nothing here is ever the text a copy would take.

use super::colors;
use crate::ui::*;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// A row's source: the eval, the frame (`usize::MAX` for the entry's own rows) and the logical line within the frame.
pub(super) type Anchor = (u64, usize, usize);

/// Rows for one frame's logical lines, and the row (within them) each logical line starts at.
pub(super) type Rows = (Vec<RtLine<'static>>, Vec<usize>);

/// `text` cut into pieces of at most `first` cells, then `rest` cells: nothing dropped, no space eaten, a wide glyph
/// that does not fit starts the next piece, and a glyph wider than a piece still takes one so the loop advances.
/// A width of 0 (no layout yet) returns the text whole.
fn cut(text: &str, first: usize, rest: usize) -> Vec<&str> {
    if first == 0 || rest == 0 {
        return vec![text];
    }
    let (mut out, mut start, mut used, mut limit) = (Vec::new(), 0, 0, first);
    for (i, c) in text.char_indices() {
        let w = c.width().unwrap_or(0);
        if used + w > limit && i > start {
            out.push(&text[start..i]);
            (start, used, limit) = (i, 0, rest);
        }
        used += w;
    }
    out.push(&text[start..]);
    out
}

/// The rows of `lines` in `style` for a pane `cols` cells wide. The first logical line starts with `prefix` and each
/// later one with `hang` blank cells, which every continuation row starts with too; the text gets the width left.
pub(super) fn rows(lines: &[String], style: Style, cols: usize, prefix: Option<Span<'static>>, hang: usize) -> Rows {
    let (mut out, mut starts) = (Vec::new(), Vec::new());
    for (i, line) in lines.iter().enumerate() {
        starts.push(out.len());
        let head = if i == 0 { prefix.clone() } else { None };
        let head_w = head.as_ref().map_or(hang, |h| h.content.width());
        let (first, rest) = (cols.saturating_sub(head_w), cols.saturating_sub(hang));
        for (j, piece) in cut(line, first, rest).into_iter().enumerate() {
            let lead = match (j, &head) {
                (0, Some(h)) => Some(h.clone()),
                (_, _) if hang > 0 => Some(Span::raw(" ".repeat(hang))),
                _ => None,
            };
            out.push(RtLine::from(lead.into_iter().chain([Span::styled(piece.to_string(), style)]).collect::<Vec<_>>()));
        }
    }
    (out, starts)
}

/// The prompt's rows of typed code: the prompt first, the code under it at the same column.
pub(super) fn code_rows(code: &str, pkg_mode: bool, cols: usize) -> Vec<RtLine<'static>> {
    let prompt = if pkg_mode { "pkg> " } else { "julia> " };
    let lines: Vec<String> = code.split('\n').map(|seg| cell_row(seg).into_owned()).collect();
    rows(&lines, colors::plain(), cols, Some(Span::styled(prompt, colors::scrollback_prompt(pkg_mode))), prompt.len()).0
}

/// What the window wrapped for a frame, kept while the frame's text and the pane's width stay what they were.
struct Cached {
    cols: usize,
    len: usize,
    seen: u64,
    rows: Rows,
}

/// The wrapped rows of the log's text frames, so a frame that changes nothing costs a copy, not a re-wrap.
#[derive(Default)]
pub(super) struct Wrap {
    pub(super) cols: usize,
    pass: u64,
    frames: HashMap<(WsKey, u64, usize), Cached>,
}

impl Wrap {
    /// Starts a build: frames not asked for during it are dropped at its end.
    pub(super) fn begin(&mut self) {
        self.pass += 1;
    }

    pub(super) fn end(&mut self) {
        let pass = self.pass;
        self.frames.retain(|_, c| c.seen == pass);
    }

    /// The rows of the frame `(row, eval_id, frame)` holding `len` bytes of text, made by `make` unless the last ask
    /// was for the same width and length.
    pub(super) fn frame(&mut self, key: (WsKey, u64, usize), len: usize, make: impl FnOnce(usize) -> Rows) -> &Rows {
        let (cols, pass) = (self.cols, self.pass);
        let cached = self.frames.entry(key).or_insert_with(|| Cached { cols: usize::MAX, len, seen: 0, rows: Rows::default() });
        if cached.cols != cols || cached.len != len || cached.rows.0.is_empty() {
            *cached = Cached { cols, len, seen: pass, rows: make(cols) };
        }
        cached.seen = pass;
        &cached.rows
    }
}

impl Wrap {
    /// The source of row `at`, given where each entry and frame starts (`marks`).
    pub(super) fn anchor(&self, row: &WsKey, marks: &[(usize, u64, usize)], at: usize) -> Option<Anchor> {
        let (start, eval, frame) = marks[marks.partition_point(|m| m.0 <= at).checked_sub(1)?];
        let starts = self.frames.get(&(row.clone(), eval, frame)).map(|c| &c.rows.1);
        Some((eval, frame, starts.map_or(0, |s| s.partition_point(|&l| l <= at - start).saturating_sub(1))))
    }

    /// The row where `anchor`'s logical line starts now.
    pub(super) fn find(&self, row: &WsKey, marks: &[(usize, u64, usize)], (eval, frame, line): Anchor) -> Option<usize> {
        let (start, ..) = marks.iter().rev().find(|m| m.1 == eval && m.2 == frame)?;
        let cached = self.frames.get(&(row.clone(), eval, frame));
        Some(start + cached.and_then(|c| c.rows.1.get(line).copied()).unwrap_or(0))
    }
}

#[cfg(test)]
#[path = "wrap_tests.rs"]
mod tests;
