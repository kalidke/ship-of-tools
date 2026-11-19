//! Up and Down by the row on screen: where the cursor lands on the row above or below it, read from the body shaped
//! at the pane's width. The body is shaped alone, with no cursor mark, so a row starts where it does once the cursor
//! has moved there, and no header line or mark cell sits between a byte of the body and a place in the layout.

use cosmic_text::{Buffer, LayoutRun};

/// Where a vertical step lands, and the x the next vertical steps keep across rows shorter than it.
pub(super) struct Landing {
    pub(super) cursor: usize,
    pub(super) goal_x: f32,
}

/// The places the cursor can stand on a row, as `(x, byte in the line)`: before each glyph, and after the last glyph
/// of a line's last row (the end of a wrapped row is the next row's start). An empty line has the one place.
fn stops<'a>(run: &'a LayoutRun<'a>, last_row: bool) -> impl Iterator<Item = (f32, usize)> + 'a {
    let end = run.glyphs.last().map_or((0.0, 0), |g| (g.x + g.w, g.end));
    run.glyphs.iter().map(|g| (g.x, g.start)).chain(last_row.then_some(end))
}

/// The cursor at `cursor` in `body` moved `rows` rows (negative up) over `buffer`, the body shaped alone. The goal
/// column is `goal_x` when a step before set it, else the cursor's own x. Up from the first row lands at 0 and Down
/// from the last at the body's end. `None` where the rows do not line up with the body's lines (a separator the
/// shaper ends a line at, or one it turns into a space) or a row is right to left: the caller steps by the file's
/// lines then.
pub(super) fn vertical(buffer: &Buffer, body: &str, cursor: usize, goal_x: Option<f32>, rows: isize) -> Option<Landing> {
    if !buffer.lines.iter().map(|l| l.text()).eq(body.split('\n')) {
        return None;
    }
    let runs: Vec<LayoutRun> = buffer.layout_runs().collect();
    let last_row = |i: usize| runs.get(i + 1).map_or(true, |next| next.line_i != runs[i].line_i);
    let before = &body[..cursor];
    let (line, in_line) = (before.matches('\n').count(), cursor - before.rfind('\n').map_or(0, |i| i + 1));
    let (from, x) = (0..runs.len()).filter(|&i| runs[i].line_i == line).find_map(|i| {
        stops(&runs[i], last_row(i)).find(|&(_, at)| at == in_line).map(|(x, _)| (i, x))
    })?;
    let goal_x = goal_x.unwrap_or(x);
    let Some(to) = from.checked_add_signed(rows).filter(|&to| to < runs.len()) else {
        return Some(Landing { cursor: if rows < 0 { 0 } else { body.len() }, goal_x });
    };
    if [from, to].iter().any(|&i| runs[i].glyphs.iter().any(|g| g.level.is_rtl())) {
        return None;
    }
    let (_, at) = stops(&runs[to], last_row(to)).min_by(|a, b| (a.0 - goal_x).abs().total_cmp(&(b.0 - goal_x).abs()))?;
    let line_start: usize = body.split('\n').take(runs[to].line_i).map(|l| l.len() + 1).sum();
    let cursor = line_start + at;
    body.is_char_boundary(cursor).then_some(Landing { cursor, goal_x })
}

/// The characters the shaper ends a line at that the body does not: a lone CR, NEL, LS and PS (`normalize_newlines`).
const SPLITS: [char; 4] = ['\r', '\u{85}', '\u{2028}', '\u{2029}'];

/// The shaped lines of one body line, as `(byte offset in the body line, text)`: a CR that ends the line is the
/// first half of a CRLF and goes with the line end.
fn pieces(line: &str) -> impl Iterator<Item = (usize, &str)> {
    let text = line.strip_suffix('\r').unwrap_or(line);
    text.split(SPLITS).map(move |piece| (piece.as_ptr() as usize - line.as_ptr() as usize, piece))
}

/// Where the body's lines are in the edit's shaped buffer: the header's lines, then each body line as one or more
/// shaped lines, then the footer's two. Counted back from the footer, so the header needs no count.
struct Map<'a> {
    buffer: &'a Buffer,
    body: &'a str,
    /// Per body line: its byte start in the body and the index of its first shaped line.
    lines: Vec<(usize, usize)>,
}

impl<'a> Map<'a> {
    fn new(buffer: &'a Buffer, body: &'a str) -> Option<Self> {
        let (mut at, mut shaped, mut lines) = (0, 0, Vec::new());
        for line in body.split('\n') {
            lines.push((at, shaped));
            (at, shaped) = (at + line.len() + 1, shaped + pieces(line).count());
        }
        let first = buffer.lines.len().checked_sub(shaped + 2)?;
        lines.iter_mut().for_each(|l| l.1 += first);
        Some(Self { buffer, body, lines })
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
}

/// Where the cursor stands: `x` and `top` px in the layout, and the row's `height`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::ui) struct Place {
    pub(in crate::ui) x: f32,
    pub(in crate::ui) top: f32,
    pub(in crate::ui) height: f32,
}

/// The row of `runs` that shaped line `shaped` holds `byte` in, with the x of that place. A row holds the bytes from
/// its first glyph to the next row's, so the space a wrap drops belongs to the row it ends, at the row's right end;
/// a byte inside a glyph stands at the glyph's left edge; the byte where one row ends and the next begins (a word
/// broken between glyphs) belongs to the next. `None` for right-to-left text.
fn stand(runs: &[LayoutRun], shaped: usize, byte: usize) -> Option<(usize, f32)> {
    let first = runs.partition_point(|r| r.line_i < shaped);
    let rows = runs[first..].iter().take_while(|r| r.line_i == shaped).count();
    let row_start = |j: usize| if j == 0 { 0 } else { runs[first + j].glyphs.first().map_or(0, |g| g.start) };
    let j = (0..rows).rev().find(|&j| row_start(j) <= byte)?;
    let run = &runs[first + j];
    if run.glyphs.iter().any(|g| g.level.is_rtl()) {
        return None;
    }
    let x = match run.glyphs.iter().find(|g| g.start <= byte && byte < g.end) {
        Some(g) => g.x,
        None => run.glyphs.last().map_or(0.0, |g| g.x + g.w),
    };
    Some((first + j, x))
}

/// The cursor's place in `buffer`, the edit's shaped buffer, for the body `body` and its cursor `cursor`.
pub(super) fn place(buffer: &Buffer, body: &str, cursor: usize) -> Option<Place> {
    let (shaped, byte) = Map::new(buffer, body)?.to_shaped(cursor)?;
    let runs: Vec<LayoutRun> = buffer.layout_runs().collect();
    let (row, x) = stand(&runs, shaped, byte)?;
    Some(Place { x, top: runs[row].line_top, height: runs[row].line_height })
}
