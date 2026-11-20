//! The REPL drawer's wrap: a logical line longer than the pane continues on the next row, at the pane's width in cells.
//! The log holds the text whole; only these rows are cut, so nothing here is ever the text a copy would take.

use super::colors;
use super::sgr::{Reader, Sgr};
use crate::ui::*;

/// A row's source: the eval, the frame (`usize::MAX` for the entry's own rows) and the logical line within the frame.
pub(super) type Anchor = (u64, usize, usize);

/// Rows for one frame's logical lines, and the row (within them) each logical line starts at.
pub(super) type Rows = (Vec<RtLine<'static>>, Vec<usize>);

/// A stretch of one logical line in one style.
pub(super) type Run = (String, Style);

/// A logical line: its styled runs, the span that starts its first row (the prompt, a label) and how many cells each
/// continuation row is indented.
pub(super) struct Logical {
    pub(super) runs: Vec<Run>,
    pub(super) head: Option<Span<'static>>,
    pub(super) indent: usize,
}

/// The longest prefix of `text`, in bytes and cells, that fits `room` cells: whole grapheme clusters, measured and
/// segmented as ratatui's buffer draws them (its `unicode-width`, through `Span`), so a row fills the pane exactly as far
/// as it is drawn. With `force` a first cluster wider than `room` is taken anyway, so a cut always advances. Only the
/// part of `text` that fits is read, so cutting a long line into rows costs the line once, not once per row.
fn fit(text: &str, room: usize, force: bool) -> (usize, usize) {
    let head = &text.as_bytes()[..text.len().min(room)];
    // A prefix of ASCII is a cell a byte, unless the next byte is a mark that joins the last one.
    if head.is_ascii() && text.as_bytes().get(head.len()).is_none_or(u8::is_ascii) {
        let n = if head.is_empty() && force { text.len().min(1) } else { head.len() };
        return (n, n);
    }
    let (span, mut at, mut width, mut any) = (Span::raw(text), 0, 0, false);
    for g in span.styled_graphemes(Style::default()) {
        // The segmentation drops a lone newline, so its bytes are skipped here, not counted as a cluster.
        while text.as_bytes()[at] == b'\n' {
            at += 1;
        }
        let w = Span::raw(g.symbol).width();
        if width + w > room && (any || !force) {
            return (at, width);
        }
        (at, width, any) = (at + g.symbol.len(), width + w, true);
    }
    (text.len(), width)
}

/// The rows of `lines` for a pane `cols` cells wide, cut by cell and cluster with nothing dropped and no space eaten, and
/// every line cut at the edge, in the middle of a word. A run keeps its style across a cut. A width of 0 (no layout yet)
/// cuts nothing. A pane no wider than a line's head leaves the text no width and cuts that line nowhere.
pub(super) fn rows(lines: &[Logical], cols: usize) -> Rows {
    let (mut out, mut starts) = (Vec::new(), Vec::new());
    for line in lines {
        starts.push(out.len());
        let pad = || (line.indent > 0).then(|| Span::raw(" ".repeat(line.indent)));
        let mut limit = cols.saturating_sub(line.head.as_ref().map_or(0, Span::width));
        let mut spans: Vec<Span<'static>> = line.head.iter().cloned().collect();
        let (mut used, mut text_from) = (0, spans.len());
        for (text, style) in &line.runs {
            let mut rest = text.as_str();
            if rest.is_empty() {
                spans.push(Span::styled("", *style));
            }
            while !rest.is_empty() {
                let room = if limit == 0 { usize::MAX } else { limit.saturating_sub(used) };
                let (take, w) = fit(rest, room, used == 0);
                if take > 0 {
                    if spans.len() > text_from && spans.last().is_some_and(|l| l.style == *style) {
                        spans.last_mut().unwrap().content.to_mut().push_str(&rest[..take]);
                    } else {
                        spans.push(Span::styled(rest[..take].to_string(), *style));
                    }
                    (used, rest) = (used + w, &rest[take..]);
                }
                if !rest.is_empty() {
                    out.push(RtLine::from(std::mem::replace(&mut spans, pad().into_iter().collect())));
                    (used, limit, text_from) = (0, cols.saturating_sub(line.indent), spans.len());
                }
            }
        }
        out.push(RtLine::from(spans));
    }
    (out, starts)
}

/// Plain lines (already cell-safe text) as logical lines of one run each, in `style`, the first starting with `prefix` and
/// every later one with `hang` blank cells, which every continuation row starts with too.
pub(super) fn uniform(lines: &[String], style: Style, prefix: Option<Span<'static>>, hang: usize) -> Vec<Logical> {
    lines.iter().enumerate().map(|(i, l)| Logical {
        runs: vec![(l.clone(), style)],
        head: if i == 0 { prefix.clone() } else { (hang > 0).then(|| Span::raw(" ".repeat(hang))) },
        indent: hang,
    }).collect()
}

/// Plain lines of styled runs as logical lines with no head and no indent.
pub(super) fn logical(lines: &[Vec<Run>]) -> Vec<Logical> {
    lines.iter().map(|runs| Logical { runs: runs.clone(), head: None, indent: 0 }).collect()
}

/// The rows of one line of text in one style, for a pane `cols` cells wide: the drawer's notes and markers.
pub(super) fn one(text: &str, style: Style, cols: usize) -> Vec<RtLine<'static>> {
    rows(&uniform(&[text.to_string()], style, None, 0), cols).0
}

/// The prompt's rows of typed code: the prompt (in `prompt_style`) first, the code under it at the same column.
pub(super) fn code_rows(code: &str, pkg_mode: bool, cols: usize, prompt_style: Style) -> Vec<RtLine<'static>> {
    let prompt = if pkg_mode { "pkg> " } else { "julia> " };
    let lines: Vec<String> = code.split('\n').map(|seg| cell_row(seg).into_owned()).collect();
    rows(&uniform(&lines, colors::plain(), Some(Span::styled(prompt, prompt_style)), prompt.len()), cols).0
}

/// What the window wrapped for a frame, kept while the frame's text and the pane's width stay what they were.
struct Cached {
    cols: usize,
    len: usize,
    seen: u64,
    rows: Rows,
    /// The SGR state the text left, which the next frame of its stream starts from.
    end: Sgr,
}

/// What the window read of a run of writes that share a line, and wrapped: kept while the writes before stay what they
/// were, so a write that arrives costs reading what it adds and cutting the line it is in, never the line so far.
struct RunCache {
    /// The state the run started in, and what it was read in (`Reader::new`): a different one is another run.
    start: Sgr,
    base: Style,
    frames: usize,
    bytes: usize,
    reader: Reader,
    /// The width the rows below were cut for, the lines of the reader already cut into `done`, and the rows of those.
    cols: usize,
    cut: usize,
    done: Rows,
    /// The rows of what the text would be if it ended here beyond the `done` lines (the lines its held text ends and the open
    /// line), and whether they are still current.
    tail: Rows,
    fresh: bool,
    seen: u64,
}

impl RunCache {
    fn new(base: Style, start: Sgr) -> Self {
        RunCache {
            start,
            base,
            frames: 0,
            bytes: 0,
            reader: Reader::new(base, start, true),
            cols: usize::MAX,
            cut: 0,
            done: Rows::default(),
            tail: Rows::default(),
            fresh: false,
            seen: 0,
        }
    }
}

/// The wrapped rows of the log's text frames, so a frame that changes nothing costs a copy, not a re-wrap.
#[derive(Default)]
pub(super) struct Wrap {
    pub(super) cols: usize,
    pass: u64,
    frames: HashMap<(WsKey, u64, usize), Cached>,
    runs: HashMap<(WsKey, u64, usize), RunCache>,
    /// The bytes of writes fed to a reader in all, across every rebuild of a run (what the tests count: each is read once).
    #[cfg(test)]
    read: usize,
}

impl Wrap {
    /// Starts a build: frames not asked for during it are dropped at its end.
    pub(super) fn begin(&mut self) {
        self.pass += 1;
    }

    pub(super) fn end(&mut self) {
        let pass = self.pass;
        self.frames.retain(|_, c| c.seen == pass);
        self.runs.retain(|_, c| c.seen == pass);
    }

    /// The rows of the run of writes `texts` of one stream (stdout or stderr, in `base`) that begins at frame `key`, and
    /// the SGR state it ends in, after the stream's state `start` before it. The run is read as a terminal reads it, a write
    /// at a time: a write that arrives is fed to the reader kept for the run and the line it is in is cut again, the lines
    /// it ended are cut once for a width, and a run no write was added to is the rows it had. A different `start` or a
    /// different run before the writes kept (what never happens to a log, which only grows) reads the run again.
    pub(super) fn run(&mut self, key: (WsKey, u64, usize), texts: &[&str], base: Style, start: Sgr) -> ((&Rows, &Rows), Sgr) {
        let (cols, pass) = (self.cols, self.pass);
        let c = self.runs.entry(key).or_insert_with(|| RunCache::new(base, start));
        let kept: usize = texts.iter().take(c.frames).map(|t| t.len()).sum();
        if c.frames > texts.len() || kept != c.bytes || c.start != start || c.base != base {
            *c = RunCache::new(base, start);
        }
        for t in &texts[c.frames..] {
            c.reader.feed(t);
            #[cfg(test)]
            {
                self.read += t.len();
            }
            c.bytes += t.len();
            c.fresh = false;
        }
        c.frames = texts.len();
        if c.cols != cols {
            (c.cols, c.cut, c.done, c.fresh) = (cols, 0, Rows::default(), false);
        }
        if !c.fresh {
            let lines = c.reader.lines();
            if c.cut < lines.len() {
                let (rows, starts) = rows(&logical(&lines[c.cut..]), cols);
                let at = c.done.0.len();
                c.done.1.extend(starts.into_iter().map(|s| s + at));
                c.done.0.extend(rows);
                c.cut = lines.len();
            }
            let (mut pending, open) = c.reader.ended();
            pending.extend(open);
            c.tail = rows(&logical(&pending), cols);
            c.fresh = true;
        }
        c.seen = pass;
        ((&c.done, &c.tail), c.reader.sgr())
    }

    /// The bytes of writes read by any run's reader, rebuilds of a run included.
    #[cfg(test)]
    pub(super) fn bytes_read(&self) -> usize {
        self.read
    }

    /// The rows of the frame `(row, eval_id, frame)` holding `len` bytes of text, and the SGR state it ends in, made by
    /// `make` unless the last ask was for the same width and length. Frames only grow in number and a frame's text never
    /// changes, so the state a frame starts in, set by the frames of its stream before it, is the same each time.
    pub(super) fn frame(&mut self, key: (WsKey, u64, usize), len: usize, make: impl FnOnce(usize) -> (Rows, Sgr)) -> (&Rows, Sgr) {
        let (cols, pass) = (self.cols, self.pass);
        let cached = self.frames.entry(key).or_insert_with(|| Cached {
            cols: usize::MAX, len, seen: 0, rows: Rows::default(), end: Sgr::default(),
        });
        if cached.cols != cols || cached.len != len || cached.rows.0.is_empty() {
            let (rows, end) = make(cols);
            *cached = Cached { cols, len, seen: pass, rows, end };
        }
        cached.seen = pass;
        (&cached.rows, cached.end)
    }
}

impl Wrap {
    /// The source of row `at`, given where each entry and frame starts (`marks`).
    pub(super) fn anchor(&self, row: &WsKey, marks: &[(usize, u64, usize)], at: usize) -> Option<Anchor> {
        let (start, eval, frame) = marks[marks.partition_point(|m| m.0 <= at).checked_sub(1)?];
        let line = self.starts(&(row.clone(), eval, frame)).map_or(0, |s| s.line_at(at - start));
        Some((eval, frame, line))
    }

    /// The row where `anchor`'s logical line starts now.
    pub(super) fn find(&self, row: &WsKey, marks: &[(usize, u64, usize)], (eval, frame, line): Anchor) -> Option<usize> {
        let (start, ..) = marks.iter().rev().find(|m| m.1 == eval && m.2 == frame)?;
        let starts = self.starts(&(row.clone(), eval, frame));
        Some(start + starts.and_then(|s| s.start_of(line)).unwrap_or(0))
    }

    /// Where each logical line of the frame or run `key` starts, in its rows.
    fn starts(&self, key: &(WsKey, u64, usize)) -> Option<Starts<'_>> {
        match (self.frames.get(key), self.runs.get(key)) {
            (Some(c), _) => Some(Starts { head: &c.rows.1, tail: &[], shift: 0 }),
            (None, Some(c)) => Some(Starts { head: &c.done.1, tail: &c.tail.1, shift: c.done.0.len() }),
            _ => None,
        }
    }
}

/// The starts of a frame's or run's logical lines: a run's are those of its finished lines and then those of the tail, which
/// begin `shift` rows in.
struct Starts<'a> {
    head: &'a [usize],
    tail: &'a [usize],
    shift: usize,
}

impl Starts<'_> {
    /// The logical line row `at` belongs to.
    fn line_at(&self, at: usize) -> usize {
        let in_head = self.head.partition_point(|&l| l <= at);
        let n = if in_head < self.head.len() || self.tail.is_empty() {
            in_head
        } else {
            in_head + self.tail.partition_point(|&l| l + self.shift <= at)
        };
        n.saturating_sub(1)
    }

    /// The row logical line `line` starts at.
    fn start_of(&self, line: usize) -> Option<usize> {
        match line.checked_sub(self.head.len()) {
            None => self.head.get(line).copied(),
            Some(k) => self.tail.get(k).map(|l| l + self.shift),
        }
    }
}

#[cfg(test)]
#[path = "wrap_tests.rs"]
mod tests;
