//! The REPL drawer's wrap: a logical line longer than the pane continues on the next row, at the pane's width in cells.
//! The log holds the text whole; only these rows are cut, so nothing here is ever the text a copy would take.

use super::colors;
use crate::ui::*;

/// A row's source: the eval, the frame (`usize::MAX` for the entry's own rows) and the logical line within the frame.
pub(super) type Anchor = (u64, usize, usize);

/// Rows for one frame's logical lines, and the row (within them) each logical line starts at.
pub(super) type Rows = (Vec<RtLine<'static>>, Vec<usize>);

/// `text` cut into pieces of at most `first` cells, then `rest` cells: nothing dropped, no space eaten, a grapheme
/// cluster (a flag, a family emoji, a letter with its marks) never split, a cluster that does not fit starts the next
/// piece, and one wider than a piece still takes one so the loop advances. Clusters and cells are measured as ratatui's
/// buffer draws them (its segmentation and its `unicode-width`, through `Span`), so a piece fills the pane exactly as far
/// as it is drawn. A width of 0 (no layout yet) returns the text whole. A line with no space is cut at the edge, in
/// the middle of a word.
fn cut(text: &str, first: usize, rest: usize) -> Vec<&str> {
    if first == 0 || rest == 0 {
        return vec![text];
    }
    let (mut out, mut start, mut limit) = (Vec::new(), 0, first);
    if text.is_ascii() {
        while text.len() - start > limit {
            out.push(&text[start..start + limit]);
            (start, limit) = (start + limit, rest);
        }
    } else {
        let (span, mut used) = (Span::raw(text), 0);
        for g in span.styled_graphemes(Style::default()) {
            // The cluster's offset in `text`, read from where it points: the segmentation skips a newline, so adding up
            // lengths would drift off a character boundary past one.
            let at = g.symbol.as_ptr() as usize - text.as_ptr() as usize;
            let w = Span::raw(g.symbol).width();
            if used + w > limit && at > start {
                out.push(&text[start..at]);
                (start, used, limit) = (at, 0, rest);
            }
            used += w;
        }
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
        let head_w = head.as_ref().map_or(hang, Span::width);
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

/// The rows of one line of text in one style, for a pane `cols` cells wide: the drawer's notes and markers.
pub(super) fn one(text: &str, style: Style, cols: usize) -> Vec<RtLine<'static>> {
    rows(&[text.to_string()], style, cols, None, 0).0
}

/// The prompt's rows of typed code: the prompt (in `prompt_style`) first, the code under it at the same column.
pub(super) fn code_rows(code: &str, pkg_mode: bool, cols: usize, prompt_style: Style) -> Vec<RtLine<'static>> {
    let prompt = if pkg_mode { "pkg> " } else { "julia> " };
    let lines: Vec<String> = code.split('\n').map(|seg| cell_row(seg).into_owned()).collect();
    rows(&lines, colors::plain(), cols, Some(Span::styled(prompt, prompt_style)), prompt.len()).0
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
