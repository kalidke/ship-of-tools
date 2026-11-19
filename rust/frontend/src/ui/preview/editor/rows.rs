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
