//! The edit buffer's rows on screen: where the cursor stands among them (`place`), and where Up and Down land it
//! (`vertical`). Both read the shaped buffer the window draws, so they see the rows the person sees. The cursor is
//! never a character of that text: the render pass draws it at `place`.

use cosmic_text::{Buffer, LayoutRun};

/// The characters the shaper ends a line at that the body does not: a lone CR, NEL, LS and PS (`normalize_newlines`).
const SPLITS: [char; 4] = ['\r', '\u{85}', '\u{2028}', '\u{2029}'];

/// The shaped lines of one body line, as `(byte offset in the body line, text)`: a CR that ends the line is the
/// first half of a CRLF and goes with the line end.
fn pieces(line: &str) -> impl Iterator<Item = (usize, &str)> {
    let text = line.strip_suffix('\r').unwrap_or(line);
    text.split(SPLITS).map(move |piece| (piece.as_ptr() as usize - line.as_ptr() as usize, piece))
}

/// The least byte any glyph of the row starts at: the row's first byte, whichever way it runs.
fn lead(run: &LayoutRun) -> Option<usize> {
    run.glyphs.iter().map(|g| g.start).min()
}

/// The edit's shaped buffer read against its body: the header's lines, then each body line as one or more shaped
/// lines, then the footer's two, so the body's lines are counted back from the footer and the header needs no count.
struct Rows<'a> {
    buffer: &'a Buffer,
    body: &'a str,
    /// Per body line: its byte start in the body and the index of its first shaped line.
    lines: Vec<(usize, usize)>,
    /// Every row of the buffer, in order.
    runs: Vec<LayoutRun<'a>>,
}

impl<'a> Rows<'a> {
    fn new(buffer: &'a Buffer, body: &'a str) -> Option<Self> {
        let (mut at, mut shaped, mut lines) = (0, 0, Vec::new());
        for line in body.split('\n') {
            lines.push((at, shaped));
            (at, shaped) = (at + line.len() + 1, shaped + pieces(line).count());
        }
        let first = buffer.lines.len().checked_sub(shaped + 2)?;
        lines.iter_mut().for_each(|l| l.1 += first);
        Some(Self { buffer, body, lines, runs: buffer.layout_runs().collect() })
    }

    fn line_text(&self, k: usize) -> &'a str {
        let end = self.lines.get(k + 1).map_or(self.body.len(), |next| next.0 - 1);
        &self.body[self.lines[k].0..end]
    }

    /// The shaped line a body byte is on and its byte in it; `None` where the shaped line is not the body's text.
    fn to_shaped(&self, at: usize) -> Option<(usize, usize)> {
        let k = self.lines.partition_point(|l| l.0 <= at) - 1;
        let in_line = at - self.lines[k].0;
        let (j, (start, piece)) = pieces(self.line_text(k)).enumerate().take_while(|(_, (s, _))| *s <= in_line).last()?;
        let shaped = self.lines[k].1 + j;
        (self.buffer.lines.get(shaped)?.text().len() == piece.len()).then_some((shaped, (in_line - start).min(piece.len())))
    }

    /// The body byte at `byte` of shaped line `shaped`; `None` for a line that is not the body's.
    fn to_body(&self, shaped: usize, byte: usize) -> Option<usize> {
        let k = self.lines.partition_point(|l| l.1 <= shaped).checked_sub(1)?;
        let (start, piece) = pieces(self.line_text(k)).nth(shaped - self.lines[k].1)?;
        (self.buffer.lines.get(shaped)?.text().len() == piece.len()).then_some(self.lines[k].0 + start + byte)
    }

    /// The body's rows: the indexes in `runs` from the body's first row to the footer's.
    fn body_rows(&self) -> std::ops::Range<usize> {
        let row = |line: usize| self.runs.partition_point(|r| r.line_i < line);
        row(self.lines[0].1)..row(self.buffer.lines.len() - 2)
    }

    /// The row that holds `byte` of shaped line `shaped`, with the x of that place. A row holds the bytes from its
    /// first glyph (the least start in it, for a row that runs right to left) to the next row's, so the space a wrap
    /// drops belongs to the row it ends; the byte where one row ends and the next begins (a word broken between
    /// glyphs) belongs to the next. The bar stands at the leading edge of the glyph that holds the byte (its left
    /// edge, its right edge for a right-to-left glyph: a byte inside a glyph stands at its edge), and where no glyph
    /// holds it (the end of the row's text, the dropped space) at the trailing edge of the last glyph before it in
    /// the text (its right edge, its left edge for a right-to-left glyph). Where a left-to-right and a right-to-left
    /// glyph meet, the byte stands at the leading edge of the glyph that starts there.
    fn stand(&self, shaped: usize, byte: usize) -> Option<(usize, f32)> {
        let first = self.runs.partition_point(|r| r.line_i < shaped);
        let rows = self.runs[first..].iter().take_while(|r| r.line_i == shaped).count();
        let row_start = |j: usize| if j == 0 { 0 } else { lead(&self.runs[first + j]).unwrap_or(0) };
        let j = (0..rows).rev().find(|&j| row_start(j) <= byte)?;
        let run = &self.runs[first + j];
        let x = match run.glyphs.iter().find(|g| g.start <= byte && byte < g.end) {
            Some(g) if g.level.is_rtl() => g.x + g.w,
            Some(g) => g.x,
            None => match run.glyphs.iter().filter(|g| g.end <= byte).max_by_key(|g| g.end) {
                Some(g) if g.level.is_rtl() => g.x,
                Some(g) => g.x + g.w,
                None => run.glyphs.iter().min_by_key(|g| g.start).map_or(0.0, |g| if g.level.is_rtl() { g.x + g.w } else { g.x }),
            },
        };
        Some((first + j, x))
    }

    /// The places the cursor can stand on row `row`, as `(x, byte in its shaped line)`: before each glyph, after the
    /// last glyph of the line's last row, and after the last glyph of a row that wrap dropped a space from (a word
    /// broken between glyphs ends at the next row's first place instead). An empty line has the one place.
    fn stops(&self, row: usize) -> Vec<(f32, usize)> {
        let run = &self.runs[row];
        let mut stops: Vec<_> = run.glyphs.iter().map(|g| (g.x, g.start)).collect();
        let end = run.glyphs.last().map_or((0.0, 0), |g| (g.x + g.w, g.end));
        match self.runs.get(row + 1).filter(|next| next.line_i == run.line_i) {
            None => stops.push((end.0, run.text.len())),
            Some(next) if lead(next).is_some_and(|start| start > end.1) => stops.push(end),
            Some(_) => {}
        }
        stops
    }
}

/// Where the cursor stands: `x` and `top` px in the layout, and the row's `height`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::ui) struct Place {
    pub(in crate::ui) x: f32,
    pub(in crate::ui) top: f32,
    pub(in crate::ui) height: f32,
}

/// The cursor's place in `buffer`, the edit's shaped buffer, for the body `body` and its cursor `cursor`.
pub(super) fn place(buffer: &Buffer, body: &str, cursor: usize) -> Option<Place> {
    let rows = Rows::new(buffer, body)?;
    let (shaped, byte) = rows.to_shaped(cursor)?;
    let (row, x) = rows.stand(shaped, byte)?;
    Some(Place { x, top: rows.runs[row].line_top, height: rows.runs[row].line_height })
}

/// Where a vertical step lands, and the column it keeps across shorter rows, in ems so a zoom between two steps
/// leaves it.
pub(super) struct Landing {
    pub(super) cursor: usize,
    pub(super) goal: f32,
}

/// The cursor at `cursor` in `body` moved `rows` rows (negative up) over `buffer`, the edit's shaped buffer. The goal
/// column is `goal` when a step before set it, else the cursor's own. Up from the first row lands at 0 and Down from
/// the last at the body's end. `None` where the buffer is not the body's text or a row involved is right to left: the
/// caller steps by the file's lines then.
pub(super) fn vertical(buffer: &Buffer, body: &str, cursor: usize, goal: Option<f32>, rows: isize) -> Option<Landing> {
    let layout = Rows::new(buffer, body)?;
    let (shaped, byte) = layout.to_shaped(cursor)?;
    let (from, x) = layout.stand(shaped, byte)?;
    let em = buffer.metrics().font_size;
    let goal = goal.unwrap_or(x / em);
    let body_rows = layout.body_rows();
    let Some(to) = from.checked_add_signed(rows).filter(|to| body_rows.contains(to)) else {
        return Some(Landing { cursor: if rows < 0 { 0 } else { body.len() }, goal });
    };
    // A row that runs right to left is not stepped by columns: the step is the file's line.
    if [from, to].iter().any(|&row| layout.runs[row].glyphs.iter().any(|g| g.level.is_rtl())) {
        return None;
    }
    let (_, at) = layout.stops(to).into_iter().min_by(|a, b| (a.0 / em - goal).abs().total_cmp(&(b.0 / em - goal).abs()))?;
    let cursor = layout.to_body(layout.runs[to].line_i, at)?;
    body.is_char_boundary(cursor).then_some(Landing { cursor, goal })
}
