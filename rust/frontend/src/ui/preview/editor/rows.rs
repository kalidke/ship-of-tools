//! The edit buffer's rows on screen: where the cursor stands among them (`place`), and where Up and Down land it
//! (`vertical`). Both read the shaped buffer the window draws, so they see the rows the person sees. The cursor is
//! never a character of that text: the render pass draws it at `place`.

use crate::ui::ScreenRect;
use crate::ui::render::brushes::Solid;
use cosmic_text::{Buffer, LayoutGlyph};

/// The least byte any glyph of the row starts at: the row's first byte, whichever way it runs.
fn lead(glyphs: &[LayoutGlyph]) -> Option<usize> {
    glyphs.iter().map(|g| g.start).min()
}

/// One row of the shaped buffer: its line, its place among that line's rows, and its top and height in px.
struct Row {
    line: usize,
    layout: usize,
    top: f32,
    height: f32,
}

#[cfg(test)]
thread_local! {
    /// How many indexes this thread built, for the test that counts them per key.
    pub(super) static BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// What the edit's shaped buffer says of its body, built once when the buffer is shaped (`Editor::reshape`) and read
/// by the bar, the step and the follow: the body's lines and the buffer's rows. It is of one body, the one whose
/// revision (`EditBuffer::rev`, raised by every change of the text) it carries, and of one shaping: a body that has
/// changed since, even to a text of the same length, or a re-shape, makes it out of date, and `place` and `vertical`
/// answer `None` for a revision other than its own.
pub(super) struct Index {
    rev: u64,
    /// Per body line, as `buffer::lines` gives them: its bytes in the body without its end. Body line `k` is shaped
    /// line `first + k`: the shaper ends lines where `buffer::lines` does.
    lines: Vec<(usize, usize)>,
    first: usize,
    rows: Vec<Row>,
}

impl Index {
    /// The index of `buffer`, the edit's shaped buffer: the header's lines, then the body's, one for each of
    /// `buffer::lines`, then the footer's two, so the body's lines are counted back from the footer and the header
    /// needs no count. `None` where the buffer has fewer lines than that.
    pub(super) fn new(buffer: &Buffer, body: &str, rev: u64) -> Option<Self> {
        #[cfg(test)]
        BUILDS.with(|b| b.set(b.get() + 1));
        let lines: Vec<(usize, usize)> = super::buffer::lines(body).collect();
        let first = buffer.lines.len().checked_sub(lines.len() + 2)?;
        let (mut rows, mut layout): (Vec<Row>, usize) = (Vec::new(), 0);
        for run in buffer.layout_runs() {
            layout = if rows.last().is_some_and(|r| r.line == run.line_i) { layout + 1 } else { 0 };
            rows.push(Row { line: run.line_i, layout, top: run.line_top, height: run.line_height });
        }
        Some(Self { rev, lines, first, rows })
    }

    /// The body's lines, as `buffer::lines` gives them, at the shaping.
    pub(super) fn lines(&self) -> &[(usize, usize)] {
        &self.lines
    }

    /// The shaped line of the body's first line.
    pub(super) fn first(&self) -> usize {
        self.first
    }

    /// The index's lines, first line and rows (`(line, place among its rows, top, height)`), to hold it equal to another.
    #[cfg(test)]
    pub(super) fn parts(&self) -> (&[(usize, usize)], usize, Vec<(usize, usize, f32, f32)>) {
        (&self.lines, self.first, self.rows.iter().map(|r| (r.line, r.layout, r.top, r.height)).collect())
    }
}

/// The edit's shaped buffer read against its body through its index.
struct Rows<'a> {
    buffer: &'a Buffer,
    body: &'a str,
    index: &'a Index,
}

impl<'a> Rows<'a> {
    /// `None` where the index is not of this body (`rev` is the body's revision).
    fn new(buffer: &'a Buffer, index: &'a Index, body: &'a str, rev: u64) -> Option<Self> {
        (index.rev == rev).then_some(Self { buffer, body, index })
    }

    fn glyphs(&self, row: usize) -> &'a [LayoutGlyph] {
        let row = &self.index.rows[row];
        self.buffer.lines[row.line].layout_opt().as_ref().and_then(|l| l.get(row.layout)).map_or(&[][..], |l| &l.glyphs[..])
    }

    fn line_of(&self, row: usize) -> usize {
        self.index.rows[row].line
    }

    /// The shaped line a body byte is on and its byte in it (a byte in a line end is the end of its line); `None`
    /// where the shaped line is not the body's text.
    fn to_shaped(&self, at: usize) -> Option<(usize, usize)> {
        let lines = &self.index.lines;
        let k = lines.partition_point(|l| l.0 <= at) - 1;
        let shaped = self.index.first + k;
        (self.buffer.lines.get(shaped)?.text().len() == lines[k].1 - lines[k].0).then_some((shaped, (at - lines[k].0).min(lines[k].1 - lines[k].0)))
    }

    /// The body byte at `byte` of shaped line `shaped`; `None` for a line that is not the body's.
    fn to_body(&self, shaped: usize, byte: usize) -> Option<usize> {
        let (start, end) = *self.index.lines.get(shaped.checked_sub(self.index.first)?)?;
        (self.buffer.lines.get(shaped)?.text().len() == end - start).then_some(start + byte)
    }

    /// The body's rows: the indexes in `runs` from the body's first row to the footer's.
    fn body_rows(&self) -> std::ops::Range<usize> {
        let row = |line: usize| self.index.rows.partition_point(|r| r.line < line);
        row(self.index.first)..row(self.buffer.lines.len() - 2)
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
        let first = self.index.rows.partition_point(|r| r.line < shaped);
        let rows = self.index.rows[first..].iter().take_while(|r| r.line == shaped).count();
        let row_start = |j: usize| if j == 0 { 0 } else { lead(self.glyphs(first + j)).unwrap_or(0) };
        let j = (0..rows).rev().find(|&j| row_start(j) <= byte)?;
        let glyphs = self.glyphs(first + j);
        let x = match glyphs.iter().find(|g| g.start <= byte && byte < g.end) {
            Some(g) if g.level.is_rtl() => g.x + g.w,
            Some(g) => g.x,
            None => match glyphs.iter().filter(|g| g.end <= byte).max_by_key(|g| g.end) {
                Some(g) if g.level.is_rtl() => g.x,
                Some(g) => g.x + g.w,
                None => glyphs.iter().min_by_key(|g| g.start).map_or(0.0, |g| if g.level.is_rtl() { g.x + g.w } else { g.x }),
            },
        };
        Some((first + j, x))
    }

    /// The places the cursor can stand on row `row`, as `(x, byte in its shaped line)`: before each glyph, after the
    /// last glyph of the line's last row, and after the last glyph of a row that wrap dropped a space from (a word
    /// broken between glyphs ends at the next row's first place instead). An empty line has the one place.
    fn stops(&self, row: usize) -> Vec<(f32, usize)> {
        let glyphs = self.glyphs(row);
        let mut stops: Vec<_> = glyphs.iter().map(|g| (g.x, g.start)).collect();
        let end = glyphs.last().map_or((0.0, 0), |g| (g.x + g.w, g.end));
        match self.index.rows.get(row + 1).filter(|next| next.line == self.line_of(row)) {
            None => stops.push((end.0, self.buffer.lines[self.line_of(row)].text().len())),
            Some(_) if lead(self.glyphs(row + 1)).is_some_and(|start| start > end.1) => stops.push(end),
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

/// The cursor's place in `buffer`, the edit's shaped buffer with its `index`, for the body `body` (of revision `rev`)
/// and its cursor `cursor`.
pub(super) fn place(buffer: &Buffer, index: &Index, body: &str, rev: u64, cursor: usize) -> Option<Place> {
    let rows = Rows::new(buffer, index, body, rev)?;
    let (shaped, byte) = rows.to_shaped(cursor)?;
    let (row, x) = rows.stand(shaped, byte)?;
    Some(Place { x, top: index.rows[row].top, height: index.rows[row].height })
}

/// Where a vertical step lands, and the column it keeps across shorter rows, in ems so a zoom between two steps
/// leaves it.
pub(super) struct Landing {
    pub(super) cursor: usize,
    pub(super) goal: f32,
}

/// The cursor at `cursor` in `body` moved `rows` rows (negative up) over `buffer`, the edit's shaped buffer. The goal
/// column is `goal` when a step before set it, else the cursor's own. Up from the first row lands at 0 and Down from
/// the last at the body's end. `None` where the index is not the body's or a row involved is right to left: the
/// caller steps by the file's lines then.
pub(super) fn vertical(
    buffer: &Buffer, index: &Index, body: &str, rev: u64, cursor: usize, goal: Option<f32>, rows: isize,
) -> Option<Landing> {
    let layout = Rows::new(buffer, index, body, rev)?;
    let (shaped, byte) = layout.to_shaped(cursor)?;
    let (from, x) = layout.stand(shaped, byte)?;
    let em = buffer.metrics().font_size;
    let goal = goal.unwrap_or(x / em);
    let body_rows = layout.body_rows();
    let Some(to) = from.checked_add_signed(rows).filter(|to| body_rows.contains(to)) else {
        return Some(Landing { cursor: if rows < 0 { 0 } else { body.len() }, goal });
    };
    // A row that runs right to left is not stepped by columns: the step is the file's line.
    if [from, to].iter().any(|&row| layout.glyphs(row).iter().any(|g| g.level.is_rtl())) {
        return None;
    }
    let (_, at) = layout.stops(to).into_iter().min_by(|a, b| (a.0 / em - goal).abs().total_cmp(&(b.0 / em - goal).abs()))?;
    let cursor = layout.to_body(layout.line_of(to), at)?;
    body.is_char_boundary(cursor).then_some(Landing { cursor, goal })
}

/// The cursor's bar for the cursor at `place`, in the pane `pane` with its text scrolled `scroll_px`. Vertically it is
/// clipped exactly as the text it stands on is: the text is drawn from `pane.y + EXTRA_TOP_PAD_PX - scroll_px` and cut
/// at the pane's top edge and at `clip_bottom` (the foot of the text's last whole line), in whole pixels
/// (`TextBounds`). Horizontally it is whole: centered on its place and moved in as far as the pane's edges (the text
/// has no gutter, and a letter's ink is not cut there, so a bar at the first column or the end of a full row would
/// lose half of itself); `None` where its place is a bar's width outside the pane, or no height of it is inside the
/// clip.
pub(in crate::ui) fn bar(place: Place, pane: ScreenRect, scroll_px: f32, clip_bottom: f32) -> Option<ScreenRect> {
    let w = (place.height * 0.12).max(2.0);
    let (y0, y1) = clip_rows(place.top, place.height, pane, scroll_px, clip_bottom);
    let inside = (-w..=pane.w + w).contains(&place.x) && pane.w >= w;
    (inside && y1 > y0).then(|| ScreenRect { x: (pane.x + place.x - w / 2.0).clamp(pane.x, pane.x + pane.w - w), y: y0, w, h: y1 - y0 })
}

/// The edit cursor's paints over the selection's bands and under the glyphs, in order: its edge, a pixel wider on each
/// side in the surface's color, then the bar in the edit text's color, both opaque. On the ground the edge is not seen
/// and the bar reads by its own color; on a band, where the bar's color is about as light as the band, the dark edge
/// outlines it, so the selection's moving end is told from its anchor.
pub(in crate::ui) fn bar_paints(bar: ScreenRect) -> [(Solid, ScreenRect); 2] {
    [(Solid::EditBarEdge, ScreenRect { x: bar.x - 1.0, w: bar.w + 2.0, ..bar }), (Solid::EditBar, bar)]
}

/// A band of the selection at `(x, top, w, h)` px in the layout, on screen in the pane `pane` with its text scrolled
/// `scroll_px`: clipped vertically as the bar is and the text is, and horizontally at the pane's edges; `None` where
/// nothing of it is inside.
pub(in crate::ui) fn band((x, top, w, h): (f32, f32, f32, f32), pane: ScreenRect, scroll_px: f32, clip_bottom: f32) -> Option<ScreenRect> {
    let (y0, y1) = clip_rows(top, h, pane, scroll_px, clip_bottom);
    let (x0, x1) = ((pane.x + x).max(pane.x), (pane.x + x + w).min(pane.x + pane.w));
    (x1 > x0 && y1 > y0).then(|| ScreenRect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
}

/// The part on screen of a row `top..top + height` px in the layout, as the text is clipped: drawn from
/// `pane.y + EXTRA_TOP_PAD_PX - scroll_px` and cut at the pane's top edge and at `clip_bottom`, in whole pixels.
fn clip_rows(top: f32, height: f32, pane: ScreenRect, scroll_px: f32, clip_bottom: f32) -> (f32, f32) {
    let top = pane.y + crate::ui::render::text::EXTRA_TOP_PAD_PX + top - scroll_px;
    (top.max(pane.y.floor()), (top + height).min(clip_bottom.ceil()))
}

/// The selection `a..b` of `body` (of revision `rev`) as bands, `(x, top, w, h)` px in the layout of `buffer`, on the
/// rows whose tops are in `y0..y1`: on each row one band for each run of selected glyphs side by side (a row that mixes
/// directions can have more than one), a glyph selected where any of its bytes is; and for a line whose end is
/// selected, a band half an em wide after the last glyph of its last row (before the first, on a right-to-left line),
/// so a selected empty line shows. None where the index is not of this body.
pub(super) fn selection(buffer: &Buffer, index: &Index, body: &str, rev: u64, (a, b): (usize, usize), (y0, y1): (f32, f32)) -> Vec<(f32, f32, f32, f32)> {
    let Some(layout) = Rows::new(buffer, index, body, rev) else { return Vec::new() };
    let body_rows = layout.body_rows();
    let mut bands = Vec::new();
    for i in index.rows.partition_point(|r| r.top + r.height <= y0).max(body_rows.start)..body_rows.end {
        let row = &index.rows[i];
        if row.top >= y1 {
            break;
        }
        let (Some(start), Some(&(_, end))) = (layout.to_body(row.line, 0), index.lines.get(row.line - index.first)) else { continue };
        let glyphs = layout.glyphs(i);
        let mut run: Option<(f32, f32)> = None;
        for g in glyphs {
            if start + g.start < b && start + g.end > a {
                run = Some(run.map_or((g.x, g.x + g.w), |(x0, x1)| (x0.min(g.x), x1.max(g.x + g.w))));
            } else if let Some((x0, x1)) = run.take() {
                bands.push((x0, row.top, x1 - x0, row.height));
            }
        }
        bands.extend(run.map(|(x0, x1)| (x0, row.top, x1 - x0, row.height)));
        let last_row = index.rows.get(i + 1).is_none_or(|next| next.line != row.line);
        if last_row && a <= end && end < b {
            let w = buffer.metrics().font_size / 2.0;
            let rtl = buffer.lines[row.line].shape_opt().as_ref().is_some_and(|s| s.rtl);
            let (lo, hi) = glyphs.iter().fold(None, |e: Option<(f32, f32)>, g| Some(e.map_or((g.x, g.x + g.w), |(lo, hi)| (lo.min(g.x), hi.max(g.x + g.w))))).unwrap_or((0.0, 0.0));
            bands.push((if rtl { lo - w } else { hi }, row.top, w, row.height));
        }
    }
    bands
}
