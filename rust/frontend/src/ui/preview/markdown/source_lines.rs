//! `SourceLines`: which file lines each run of a rendered markdown buffer's text came from, for the share with the agent.

use super::*;

use crate::ui::render::text::one_paragraph_per_line;
use std::ops::Range;

pub(in crate::ui) struct SourceLines {
    /// The text the buffer was rendered from, so the lines and their text are one.
    source: String,
    /// Per buffer line, its runs of source text: a byte range of the line and the file lines it came from.
    runs: Vec<Vec<(Range<usize>, (usize, usize))>>,
}

impl SourceLines {
    /// The map of `buffer`, shaped from `spans` of `source` with the walk's `marks`: each span is split as the shaper
    /// splits it, and every piece must equal the buffer's bytes at the place it lands. `None` when one does not, or
    /// when the buffer's line count or a line's length differs from what the spans make.
    pub(super) fn build(
        source: &str, spans: &[(String, Attrs<'static>)], marks: &[(Range<usize>, Lines)], buffer: &Buffer,
    ) -> Option<Self> {
        let mut runs: Vec<Vec<(Range<usize>, (usize, usize))>> = vec![Vec::new()];
        let mut lens = vec![0usize];
        let mut mark = 0;
        let mut k = 0;
        for (i, (text, _)) in spans.iter().enumerate() {
            while marks.get(mark).is_some_and(|(r, _)| r.end <= i) {
                mark += 1;
            }
            let inside = marks.get(mark).filter(|(r, _)| r.contains(&i)).map(|(_, lines)| lines);
            if marks.get(mark).is_some_and(|(r, _)| r.start == i) {
                k = 0;
            }
            for (j, piece) in one_paragraph_per_line(text).split(['\n', '\r']).enumerate() {
                if j > 0 {
                    runs.push(Vec::new());
                    lens.push(0);
                    k += 1;
                }
                let n = runs.len() - 1;
                let at = lens[n];
                if !piece.is_empty() {
                    if buffer.lines.get(n)?.text().get(at..at + piece.len())? != piece {
                        return None;
                    }
                    let lines = match inside {
                        Some(Lines::Block(a, b)) => Some((*a, *b)),
                        Some(Lines::Code(f)) => Some((f + k, f + k)),
                        None => None,
                    };
                    if let Some(lines) = lines.filter(|_| !piece.chars().all(|c| c == MATH_PLACEHOLDER)) {
                        runs[n].push((at..at + piece.len(), lines));
                    }
                    lens[n] += piece.len();
                }
            }
        }
        // A final break makes no empty last buffer line.
        if runs.len() > 1 && lens.last() == Some(&0) && buffer.lines.len() == runs.len() - 1 {
            runs.pop();
            lens.pop();
        }
        let lined_up = runs.len() == buffer.lines.len()
            && lens.iter().zip(buffer.lines.iter()).all(|(len, line)| *len == line.text().len());
        lined_up.then(|| Self { source: source.to_string(), runs })
    }

    /// The least first and greatest last file line of every run that shares a byte with the range, `None` when none does.
    pub(super) fn lines_of(&self, range: ((usize, usize), (usize, usize))) -> Option<(usize, usize)> {
        let ((first, from), (last, to)) = range;
        let mut found: Option<(usize, usize)> = None;
        for n in first..=last {
            let lo = if n == first { from } else { 0 };
            let hi = if n == last { to } else { usize::MAX };
            for (bytes, (a, b)) in self.runs.get(n)? {
                if bytes.start < hi && lo < bytes.end {
                    found = Some(found.map_or((*a, *b), |(x, y)| (x.min(*a), y.max(*b))));
                }
            }
        }
        found
    }

    /// The range and text of the whole file lines the selected range was rendered from, as `selection.set` names a
    /// file's text.
    pub(in crate::ui) fn file_selection(
        &self, range: ((usize, usize), (usize, usize)),
    ) -> Option<(((usize, usize), (usize, usize)), String)> {
        let (a, b) = self.lines_of(range)?;
        sot_protocol::ops::whole_lines(&self.source, a, b)
    }
}

#[cfg(test)]
#[path = "source_lines_tests.rs"]
mod tests;
