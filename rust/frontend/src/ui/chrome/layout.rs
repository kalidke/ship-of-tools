// layout.rs — geometry for the chrome's pane wireframe.
//
// Previously the chrome was hard-coded to a 2×2 quadrant (TL nav · TR
// preview · BL llm · BR repl) parameterised by two percentages. This
// module computes the *generic* geometry given a named preset: a list
// of full-height columns left-to-right + an optional bottom drawer
// for the rarely-used pane (today: REPL).
//
// The chrome rendering closure stays largely intact — it just asks
// this module for a `LayoutGeom` and reads rect per slot.
// `draw_wireframe` (in this file) now takes the inner vertical /
// horizontal line positions from the same struct, so the box-drawing
// junctions stay in sync with the pane geometry automatically.

use super::*;
use ratatui::layout::Rect;

use crate::ui::persist::settings::{LayoutPreset, Slot};

/// Concrete geometry for one frame: a rect per slot present in the
/// preset (others = `None`), the inner border positions, and any
/// information the closure needs to render titles + the bottom
/// drawer correctly.
#[derive(Debug, Clone, Default)]
pub struct LayoutGeom {
    /// Rect for each slot — `None` when the slot isn't in this
    /// preset's column list and isn't open as a drawer.
    pub nav: Option<Rect>,
    pub preview: Option<Rect>,
    pub llm: Option<Rect>,
    /// Drawer rect when the drawer is open; `None` otherwise (or when
    /// the preset has no drawer slot defined).
    pub repl: Option<Rect>,
    /// Inner vertical border x-positions (column dividers). Excludes
    /// the outer left/right edges, which are always drawn.
    pub vlines: Vec<u16>,
    /// Inner horizontal border y-positions (e.g. above the drawer).
    /// At most one for the v1 (drawer top). Excludes the outer
    /// top/bottom edges.
    pub hlines: Vec<u16>,
    /// The outer rect this geometry was computed for (echoes the
    /// caller's `area` — convenient for `draw_wireframe`).
    pub area: Rect,
    /// When the drawer doesn't span the full chrome width — i.e. the
    /// preset has an `Llm` column that should keep its full vertical
    /// extent — this is the x of the rightmost cell the drawer
    /// reaches. `None` means the drawer (when open) spans every
    /// column to the outer right edge (legacy behaviour, kept for
    /// presets with no `Llm` column).
    pub drawer_x_end: Option<u16>,
    /// X of the vertical divider that the `Llm` column starts at,
    /// when the drawer is open and partially scoped. This vline runs
    /// the *full* chrome height (top → bottom); every other vline
    /// stops at the drawer top. `None` when the drawer isn't
    /// partially scoped (drawer closed, or no `Llm` in the preset).
    pub llm_left_vline: Option<u16>,
}

impl LayoutGeom {
    /// Helper for callers that have a Slot enum value and want its
    /// rect. Returns a zero-area rect when the slot isn't laid out,
    /// so callers can pass it to `frame.render_widget` without
    /// special-casing — the widget no-ops at zero area.
    pub fn rect_for(&self, slot: Slot) -> Rect {
        let r = match slot {
            Slot::Nav => self.nav,
            Slot::Preview => self.preview,
            Slot::Llm => self.llm,
            Slot::Repl => self.repl,
        };
        r.unwrap_or(Rect {
            x: self.area.x,
            y: self.area.y,
            width: 0,
            height: 0,
        })
    }
}

/// Compute the geometry for one frame. `area` is the full chrome
/// area (from ratatui's frame), `preset` selects the column shape,
/// and `drawer_open` toggles whether the bottom drawer eats its
/// configured fraction of vertical space. When `maximize` is `Some`,
/// the named slot fills the entire inner area and every other slot
/// + every inner border collapses to zero — the wireframe degenerates
/// to just the outer rectangle.
///
/// The returned rects are *content* rects — interior of each pane,
/// excluding the surrounding border cells. They match the semantics
/// the previous 2×2 code expected (`tl_content` etc.).
#[allow(clippy::too_many_lines, reason = "computes every pane's content rect for the current layout; predates the 100-line limit")]
pub fn compute(
    area: Rect,
    preset: &LayoutPreset,
    drawer_open: bool,
    maximize: Option<Slot>,
) -> LayoutGeom {
    // Maximised mode: the focused slot eats the whole inner area; no
    // inner borders, no other rects. The downstream renderer paints
    // that slot's body across the full area and skips zero-sized
    // siblings without special casing.
    if let Some(slot) = maximize {
        let mut geom = LayoutGeom {
            area,
            ..LayoutGeom::default()
        };
        if area.width < 3 || area.height < 3 {
            return geom;
        }
        let inner = Rect {
            x: area.x + 1,
            y: area.y + 1,
            width: area.width - 2,
            height: area.height - 2,
        };
        match slot {
            Slot::Nav => geom.nav = Some(inner),
            Slot::Preview => geom.preview = Some(inner),
            Slot::Llm => geom.llm = Some(inner),
            Slot::Repl => geom.repl = Some(inner),
        }
        return geom;
    }
    let mut geom = LayoutGeom {
        area,
        ..LayoutGeom::default()
    };
    if area.width < 5 || area.height < 5 {
        // Degenerate — caller's render paths all no-op on zero rects.
        return geom;
    }

    // Reserve the bottom drawer first (if any) so the column rects
    // get their final vertical extent in one pass.
    let drawer_top_y = if drawer_open && preset.drawer.is_some() && area.height >= 7 {
        let h = (area.height as f32 * preset.drawer_height).round() as u16;
        // Leave at least 2 rows above the drawer line and 2 inside it
        // so neither half collapses; clamp inside the area. Below 7 rows
        // the minimum of 3 exceeds the rows left, so no drawer opens.
        let h = h.clamp(3, area.height.saturating_sub(4));
        let y = area.y + area.height - 1 - h;
        Some(y)
    } else {
        None
    };

    let full_inner_top = area.y + 1;
    let full_inner_bot = area.y + area.height - 2;
    let drawer_clipped_bot = drawer_top_y
        .map(|y| y.saturating_sub(1))
        .unwrap_or(full_inner_bot);
    if full_inner_top > full_inner_bot {
        return geom;
    }

    // Walk the columns left-to-right, slicing the area's width.
    // Each column's `width` is normalized at parse time so they sum to ~1.0; we still
    // saturate the last column to whatever's left over after rounding
    // so the right border lands on `last_col` exactly.
    let inner_left = area.x + 1;
    let inner_right = area.x + area.width - 2; // last interior column
    let inner_total = (inner_right as i32 - inner_left as i32 + 1).max(0) as u16;
    // Column `i`'s share of the interior, from its fraction. Only read for
    // `i < n - 1`: the last column takes the remainder instead.
    let share = |i: usize| ((inner_total as f32) * preset.columns[i].width).round() as u16;

    // Every column's span, computed once before the walk. A column before the
    // last takes its share, capped so its right border still lands inside the
    // interior; the last column takes the remainder. A span under 2 is skipped
    // and moves the cursor by nothing, as the walk below does.
    let n = preset.columns.len();
    let mut spans = Vec::with_capacity(n);
    let mut next = inner_left;
    for i in 0..n {
        let span = if i + 1 == n {
            inner_right.saturating_sub(next).saturating_add(1)
        } else {
            share(i).min(inner_right.saturating_sub(next))
        };
        if span >= 2 {
            next = next.saturating_add(span).saturating_add(1);
        }
        spans.push(span);
    }

    // When the preset has an Llm column AND the drawer is open, the
    // drawer is scoped to the columns left of Llm — Llm stays full
    // height. Saves the long-vertical pane (LLM with long plans /
    // tables) from being trimmed every time the user pops the REPL.
    // `llm_idx` is the 0-based index in `preset.columns`; `None` falls
    // back to the full-width drawer. The column left of Llm must be laid
    // out (span >= 2) for its divider to exist; otherwise the drawer stays
    // full width and every column is clipped alike.
    let llm_idx = preset.columns.iter().position(|c| c.slot == Slot::Llm);
    let partial_drawer = drawer_top_y.is_some()
        && llm_idx.map(|i| i > 0 && spans[i - 1] >= 2).unwrap_or(false);

    let mut cursor = inner_left;
    for (i, column) in preset.columns.iter().enumerate() {
        let span = spans[i];
        if span < 2 {
            // Too small to render; skip but keep cursor stable so the
            // next column doesn't double-count.
            continue;
        }
        // Each column's vertical extent. With the partial drawer, only
        // columns to the *left* of Llm get clipped; Llm and anything
        // to its right run full height. Without partial drawer, every
        // column shrinks together (legacy behaviour, kept for presets
        // that have no Llm column to anchor on).
        let bot_y = if partial_drawer && i < llm_idx.unwrap() {
            drawer_clipped_bot
        } else if partial_drawer {
            full_inner_bot
        } else {
            drawer_clipped_bot
        };
        let height = bot_y.saturating_sub(full_inner_top).saturating_add(1);
        let rect = Rect {
            x: cursor,
            y: full_inner_top,
            width: span,
            height,
        };
        match column.slot {
            Slot::Nav => geom.nav = Some(rect),
            Slot::Preview => geom.preview = Some(rect),
            Slot::Llm => geom.llm = Some(rect),
            // A `repl` listed as a column (rather than drawer) is
            // allowed — the user may pin it open as a fourth column on
            // very wide screens. Drawer is the default location, not
            // the only one.
            Slot::Repl => geom.repl = Some(rect),
        }
        // Step past this column + its right border.
        cursor = cursor.saturating_add(span).saturating_add(1);
        // Record the inner vertical border position (one cell past
        // this column's right edge) — unless this is the last column.
        if i + 1 < n {
            let vline_x = cursor.saturating_sub(1);
            geom.vlines.push(vline_x);
            // The vline immediately to Llm's left runs the *full*
            // chrome height so Llm and the drawer can coexist
            // without the divider breaking at the drawer line.
            if partial_drawer && i + 1 == llm_idx.unwrap() {
                geom.llm_left_vline = Some(vline_x);
            }
        }
    }

    // Drawer rect — partial-width when Llm pins the right side, full
    // chrome width otherwise.
    if let Some(y) = drawer_top_y {
        let drawer_inner_top = y + 1;
        let drawer_inner_bot = area.y + area.height - 2;
        if drawer_inner_top <= drawer_inner_bot {
            // Narrow only when the divider left of Llm was laid out.
            let (drawer_x, drawer_w, drawer_x_end) = match geom.llm_left_vline {
                Some(lv) => {
                    // Drawer ends one cell before Llm's left divider.
                    let dwidth = lv.saturating_sub(inner_left); // [inner_left..lv-1] inclusive
                    (inner_left, dwidth, Some(lv.saturating_sub(1)))
                }
                None => (inner_left, inner_total, None),
            };
            if drawer_w >= 2 {
                let drawer_rect = Rect {
                    x: drawer_x,
                    y: drawer_inner_top,
                    width: drawer_w,
                    height: drawer_inner_bot - drawer_inner_top + 1,
                };
                if let Some(slot) = preset.drawer {
                    match slot {
                        Slot::Nav => geom.nav = Some(drawer_rect),
                        Slot::Preview => geom.preview = Some(drawer_rect),
                        Slot::Llm => geom.llm = Some(drawer_rect),
                        Slot::Repl => geom.repl = Some(drawer_rect),
                    }
                }
                geom.drawer_x_end = drawer_x_end;
                geom.hlines.push(y);
            }
        }
    }

    geom
}

/// Paint a unified box-drawing wireframe over `area`, using `vlines`
/// as the x-positions of inner vertical dividers and `hlines` as the
/// y-positions of inner horizontal dividers (today: at most one — the
/// drawer's top edge). Junctions are computed from the line lists so
/// the wireframe is consistent regardless of how many columns the
/// preset declared; `hlines.is_empty()` means full-height columns,
/// and an empty `vlines` is a single-pane (e.g. maximised) layout.
///
/// In the v1, hlines only ever clip vertical lines (drawer top): the
/// horizontal line crosses the whole width and the verticals stop
/// above it (the drawer below has no inner dividers).
// `drawer_x_end`: when `Some(x)`, the topmost hline (drawer top)
// ends at `x` instead of running to the outer right edge. The cell at
// `x + 1` is `llm_left_vline` (a full-height divider) and gets a ┤
// junction; cells to the right of `x` get the outer vertical run
// unchanged.
// `llm_left_vline`: x of the vertical divider that runs full height
// (Llm's left edge) when the drawer is partially scoped. It and every
// vline right of it run full height; vlines left of it stop at the
// drawer line.
pub(in crate::ui) fn draw_wireframe(
    buf: &mut ratatui::buffer::Buffer,
    area: ratatui::layout::Rect,
    vlines: &[u16],
    hlines: &[u16],
    drawer_x_end: Option<u16>,
    llm_left_vline: Option<u16>,
    style: Style,
) {
    if area.width < 3 || area.height < 3 {
        return;
    }
    let left = area.x;
    let right = area.x + area.width - 1;
    let top = area.y;
    let bot = area.y + area.height - 1;

    let is_vline = |x: u16| vlines.iter().any(|&v| v == x);
    let is_hline = |y: u16| hlines.iter().any(|&h| h == y);
    // The drawer-clip y for *short* vlines. Full-height vlines
    // (llm_left_vline) ignore this and run to bot - 1.
    let short_vline_bot_y = hlines
        .iter()
        .copied()
        .min()
        .map(|h| h.saturating_sub(1))
        .unwrap_or(bot.saturating_sub(1));

    // Outer top/bottom horizontal runs.
    for x in (left + 1)..right {
        if !is_vline(x) {
            buf.set_string(x, top, "─", style);
            buf.set_string(x, bot, "─", style);
        }
    }
    // The drawer line runs to `hline_right_excl`; a vertical at `x` past
    // that is not crossed by it and so is drawn on the hline rows too.
    let hline_right_excl = drawer_x_end.map(|x| x + 1).unwrap_or(right);
    let hline_reaches = |x: u16| x <= hline_right_excl;
    let full_height = |x: u16| llm_left_vline.is_some_and(|lv| x >= lv);
    // Outer left/right vertical runs (skip the cell where an hline
    // crosses — junction handling fills it in below).
    for y in (top + 1)..bot {
        if !is_hline(y) {
            buf.set_string(left, y, "│", style);
        }
        if !is_hline(y) || !hline_reaches(right) {
            buf.set_string(right, y, "│", style);
        }
    }
    // Inner horizontal runs (drawer top). Optionally truncated at
    // `drawer_x_end` when the Llm column wants the right side full
    // height.
    for &y in hlines {
        for x in (left + 1)..hline_right_excl {
            if !is_vline(x) {
                buf.set_string(x, y, "─", style);
            }
        }
    }
    // Inner vertical runs. The Llm-left vline and every vline right of
    // it run full height; the others stop at the drawer line.
    for &x in vlines {
        let v_bot = if full_height(x) {
            bot.saturating_sub(1)
        } else {
            short_vline_bot_y
        };
        for y in (top + 1)..=v_bot {
            if !is_hline(y) || !hline_reaches(x) {
                buf.set_string(x, y, "│", style);
            }
        }
    }

    // Outer corners.
    buf.set_string(left, top, "┌", style);
    buf.set_string(right, top, "┐", style);
    buf.set_string(left, bot, "└", style);
    buf.set_string(right, bot, "┘", style);
    // Top-edge T-junctions (vline meets outer top).
    for &x in vlines {
        buf.set_string(x, top, "┬", style);
    }
    // Bottom-edge T-junctions for every vline that reaches the
    // outer bottom — i.e. when no drawer is open, OR when this vline
    // is a full-height divider running past the drawer.
    for &x in vlines {
        let reaches_bot = hlines.is_empty() || full_height(x);
        if reaches_bot {
            buf.set_string(x, bot, "┴", style);
        }
    }
    // Drawer-line endpoints + vline crossings.
    for &y in hlines {
        buf.set_string(left, y, "├", style);
        // Right end of the hline: outer ┤ when the hline spans full
        // width; nothing special at the outer right edge otherwise —
        // the vertical run above continues uninterrupted there.
        if drawer_x_end.is_none() {
            buf.set_string(right, y, "┤", style);
        }
        for &x in vlines {
            if !hline_reaches(x) {
                continue;
            }
            if full_height(x) {
                // Hline approaches from the left only; vline runs
                // top → bottom through this cell: ┤.
                buf.set_string(x, y, "┤", style);
            } else {
                // Short vline terminates at the hline from above; no
                // continuation below. Hline crosses both sides: ┴.
                buf.set_string(x, y, "┴", style);
            }
        }
    }
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
