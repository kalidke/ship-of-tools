// layout.rs — geometry for the chrome's pane wireframe (ADR 0014
// layout-presets rework).
//
// Previously the chrome was hard-coded to a 2×2 quadrant (TL nav · TR
// preview · BL llm · BR repl) parameterised by two percentages. This
// module computes the *generic* geometry given a named preset: a list
// of full-height columns left-to-right + an optional bottom drawer
// for the rarely-used pane (today: REPL).
//
// The chrome rendering closure stays largely intact — it just asks
// this module for a `LayoutGeom` and reads rect per slot.
// `draw_wireframe` (in `gpu.rs`) now takes the inner vertical /
// horizontal line positions from the same struct, so the box-drawing
// junctions stay in sync with the pane geometry automatically.

use ratatui::layout::Rect;

use crate::settings::{LayoutPreset, Slot};

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
    let drawer_top_y = if drawer_open && preset.drawer.is_some() {
        let h = (area.height as f32 * preset.drawer_height).round() as u16;
        // Leave at least 2 rows above the drawer line and 2 inside it
        // so neither half collapses; clamp inside the area.
        let h = h.clamp(3, area.height.saturating_sub(4));
        let y = area.y + area.height - 1 - h;
        Some(y)
    } else {
        None
    };

    // When the preset has an Llm column AND the drawer is open, the
    // drawer is scoped to the columns left of Llm — Llm stays full
    // height. Saves the long-vertical pane (LLM with long plans /
    // tables) from being trimmed every time the user pops the REPL.
    // `llm_idx` is the 0-based index in `preset.columns`; `None` falls
    // back to the full-width drawer.
    let llm_idx = preset.columns.iter().position(|s| *s == Slot::Llm);
    let partial_drawer = drawer_top_y.is_some()
        && llm_idx.map(|i| i > 0).unwrap_or(false);

    let full_inner_top = area.y + 1;
    let full_inner_bot = area.y + area.height - 2;
    let drawer_clipped_bot = drawer_top_y
        .map(|y| y.saturating_sub(1))
        .unwrap_or(full_inner_bot);
    if full_inner_top > full_inner_bot {
        return geom;
    }

    // Walk the columns left-to-right, slicing the area's width.
    // `widths` is normalised at parse time so sums to ~1.0; we still
    // saturate the last column to whatever's left over after rounding
    // so the right border lands on `last_col` exactly.
    let inner_left = area.x + 1;
    let inner_right = area.x + area.width - 2; // last interior column
    let inner_total = (inner_right as i32 - inner_left as i32 + 1).max(0) as u16;

    let mut cursor = inner_left;
    let n = preset.columns.len();
    for (i, slot) in preset.columns.iter().enumerate() {
        let span = if i + 1 == n {
            // Final column takes whatever's left — avoids the cumulative
            // rounding gap that would otherwise show as a 1-cell strip
            // at the right edge.
            inner_right.saturating_sub(cursor).saturating_add(1)
        } else {
            ((inner_total as f32) * preset.widths[i]).round() as u16
        };
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
        match slot {
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
            let (drawer_x, drawer_w, drawer_x_end) = if partial_drawer {
                // Drawer ends one cell before Llm's left divider.
                let lv = geom.llm_left_vline.expect("partial_drawer ⇒ llm_left_vline");
                let dwidth = lv.saturating_sub(inner_left); // [inner_left..lv-1] inclusive
                (inner_left, dwidth, Some(lv.saturating_sub(1)))
            } else {
                (inner_left, inner_total, None)
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

/// Resolve an aspect ratio (width / height of the primary monitor)
/// into the descriptive label used by `Settings::resolve_preset`'s
/// auto path. Exposed so the chrome's status line can mention which
/// preset was chosen.
#[allow(dead_code)] // exported for the status-line label; not yet wired
pub fn aspect_to_label(aspect: f32) -> &'static str {
    if aspect > 1.9 {
        "ultrawide"
    } else if aspect >= 1.5 {
        "laptop"
    } else {
        "portrait"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::LayoutPreset;

    fn area(w: u16, h: u16) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        }
    }

    /// Test helper: call `compute` without maximisation.
    fn compute_default(area: Rect, preset: &LayoutPreset, drawer_open: bool) -> LayoutGeom {
        compute(area, preset, drawer_open, None)
    }

    #[test]
    fn ultrawide_default_lays_out_three_columns() {
        let preset = LayoutPreset::default_ultrawide();
        let g = compute_default(area(120, 40), &preset, false);
        assert!(g.nav.is_some());
        assert!(g.preview.is_some());
        assert!(g.llm.is_some());
        assert!(g.repl.is_none());
        // Two inner vertical borders between three columns.
        assert_eq!(g.vlines.len(), 2);
        // No horizontal inner borders without a drawer.
        assert!(g.hlines.is_empty());
        // Columns add up (with their borders) to the full width.
        let total = g.nav.unwrap().width
            + g.preview.unwrap().width
            + g.llm.unwrap().width
            + g.vlines.len() as u16;
        // total + 2 outer borders == area width
        assert_eq!(total + 2, 120);
    }

    #[test]
    fn drawer_open_reserves_bottom_strip() {
        let preset = LayoutPreset::default_ultrawide();
        let g = compute_default(area(120, 40), &preset, true);
        assert!(g.repl.is_some());
        // One inner horizontal border above the drawer.
        assert_eq!(g.hlines.len(), 1);
        // Drawer height ≈ 0.35 * 40 = 14, clamped, but ≥ 3.
        assert!(g.repl.unwrap().height >= 3);
        // Columns lose vertical extent to the drawer.
        assert!(g.nav.unwrap().height < 36);
    }

    #[test]
    fn laptop_default_lays_out_three_columns_too() {
        let preset = LayoutPreset::default_laptop();
        let g = compute_default(area(80, 25), &preset, false);
        assert_eq!(g.vlines.len(), 2);
        assert!(g.nav.is_some());
        assert!(g.llm.is_some());
    }

    #[test]
    fn portrait_default_two_columns() {
        let preset = LayoutPreset::default_portrait();
        let g = compute_default(area(60, 80), &preset, false);
        assert_eq!(g.vlines.len(), 1);
        assert!(g.llm.is_none());
    }

    #[test]
    fn degenerate_area_returns_empty_geom() {
        let preset = LayoutPreset::default_ultrawide();
        let g = compute_default(area(3, 3), &preset, false);
        assert!(g.nav.is_none());
        assert!(g.preview.is_none());
    }

    #[test]
    fn rect_for_returns_zero_when_slot_absent() {
        let preset = LayoutPreset::default_portrait();
        let g = compute_default(area(60, 80), &preset, false);
        let r = g.rect_for(Slot::Llm);
        assert_eq!(r.width, 0);
        assert_eq!(r.height, 0);
    }

    #[test]
    fn aspect_label_buckets() {
        assert_eq!(aspect_to_label(2.4), "ultrawide");
        assert_eq!(aspect_to_label(1.6), "laptop");
        assert_eq!(aspect_to_label(1.3), "portrait");
    }

    #[test]
    fn partial_drawer_keeps_llm_full_height() {
        // Ultrawide preset has columns [nav, preview, llm] + drawer=repl.
        // With the drawer open, nav + preview should be shorter (clipped
        // at drawer_top) and llm should run the full inner height.
        let preset = LayoutPreset::default_ultrawide();
        let g = compute(area(120, 40), &preset, true, None);
        let nav = g.nav.unwrap();
        let prev = g.preview.unwrap();
        let llm = g.llm.unwrap();
        let repl = g.repl.unwrap();
        // nav + preview reduced.
        assert!(nav.height < 38, "nav height should be < full inner");
        assert_eq!(nav.height, prev.height);
        // llm is full inner height (area.height - 2).
        assert_eq!(llm.height, 38);
        // drawer is partial-width and aligned with nav+preview.
        assert_eq!(repl.x, nav.x);
        // Both end at the same column (the cell just before the
        // full-height vline that separates them from Llm).
        assert_eq!(repl.x + repl.width, prev.x + prev.width);
        // The vline immediately left of LLM should be the full-height
        // divider, and `drawer_x_end` should be one cell before it.
        assert!(g.llm_left_vline.is_some());
        let lv = g.llm_left_vline.unwrap();
        assert_eq!(lv, llm.x.saturating_sub(1));
        assert_eq!(g.drawer_x_end, Some(lv.saturating_sub(1)));
    }

    #[test]
    fn partial_drawer_not_applied_without_llm_column() {
        // Portrait preset has columns [nav, preview] and no llm. Even
        // with the drawer open, llm_left_vline stays None and the
        // drawer spans the whole chrome width (legacy behaviour).
        let preset = LayoutPreset::default_portrait();
        let g = compute(area(60, 80), &preset, true, None);
        assert!(g.llm_left_vline.is_none());
        assert!(g.drawer_x_end.is_none());
        let repl = g.repl.unwrap();
        assert_eq!(repl.x, 1); // inner_left
        // Spans full inner width.
        let inner_total = 60 - 2;
        assert_eq!(repl.width, inner_total);
    }

    #[test]
    fn maximize_fills_one_slot_zero_borders() {
        let preset = LayoutPreset::default_ultrawide();
        let g = compute(area(120, 40), &preset, false, Some(Slot::Preview));
        assert!(g.preview.is_some());
        assert!(g.nav.is_none());
        assert!(g.llm.is_none());
        // No inner borders when maximised.
        assert!(g.vlines.is_empty());
        assert!(g.hlines.is_empty());
        let pr = g.preview.unwrap();
        assert_eq!(pr.width, 118);
        assert_eq!(pr.height, 38);
    }
}
