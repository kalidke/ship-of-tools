use super::*;
use crate::ui::persist::settings::{Column, LayoutPreset};

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

#[test]
fn wireframe_right_edges_survive_a_partial_drawer() {
    use crate::ui::persist::settings::{LayoutPreset, Slot};
    use ratatui::{buffer::Buffer, layout::Rect};
    let area = Rect::new(0, 0, 60, 20);
    let paint = |p: &LayoutPreset| {
        let geom = crate::ui::chrome::layout::compute(area, p, true, None);
        let mut buf = Buffer::empty(area);
        draw_wireframe(
            &mut buf,
            area,
            &geom.vlines,
            &geom.hlines,
            geom.drawer_x_end,
            geom.llm_left_vline,
            Style::default(),
        );
        (buf, geom)
    };
    // (a) the outer right edge is unbroken at the drawer line.
    let (buf, geom) = paint(&LayoutPreset::default_ultrawide());
    let y = geom.hlines[0];
    assert_eq!(buf[(59, y)].symbol(), "│");
    // (b) a divider right of the agent column runs full height.
    let p = LayoutPreset {
        columns: vec![
            Column { slot: Slot::Nav, width: 0.2 },
            Column { slot: Slot::Llm, width: 0.4 },
            Column { slot: Slot::Preview, width: 0.4 },
        ],
        drawer: Some(Slot::Repl),
        drawer_height: 0.4,
    };
    let (buf, geom) = paint(&p);
    let y = geom.hlines[0];
    let x = geom.vlines[1];
    assert_eq!(buf[(x, y)].symbol(), "│");
    assert_eq!(buf[(x, y + 1)].symbol(), "│");
    assert_eq!(buf[(x, 19)].symbol(), "┴");
}

#[test]
fn tiny_preview_width_with_drawer_open_does_not_panic() {
    // Preview's span rounds below 2 cells, so the divider left of Llm
    // is never laid out and the drawer must span the full width.
    let preset = LayoutPreset {
        columns: vec![
            Column { slot: Slot::Nav, width: 0.18 },
            Column { slot: Slot::Preview, width: 0.005 },
            Column { slot: Slot::Llm, width: 0.815 },
        ],
        drawer: Some(Slot::Repl),
        drawer_height: 0.40,
    };
    let g = compute(area(160, 45), &preset, true, None);
    assert!(g.llm_left_vline.is_none());
    assert!(g.drawer_x_end.is_none());
    assert!(g.repl.is_some());
    assert!(g.llm.is_some());
}

#[test]
fn six_cell_wide_window_with_drawer_open_does_not_panic() {
    // Inner width 4: the column left of Llm rounds below 2 cells in the
    // ultrawide and laptop presets; portrait has no Llm column.
    for preset in [
        LayoutPreset::default_ultrawide(),
        LayoutPreset::default_laptop(),
        LayoutPreset::default_portrait(),
    ] {
        let g = compute(area(6, 20), &preset, true, None);
        assert!(g.llm_left_vline.is_none());
        assert!(g.repl.is_some());
    }
}

#[test]
fn drawer_opens_only_from_seven_rows() {
    // Below 7 rows the drawer's height clamp has no room (its minimum
    // of 3 exceeds the rows left after the 4 reserved), so none opens.
    let preset = LayoutPreset::default_ultrawide();
    for rows in 5..7 {
        let g = compute(area(120, rows), &preset, true, None);
        assert!(g.repl.is_none(), "no drawer at {rows} rows");
        assert!(g.hlines.is_empty());
    }
    let g = compute(area(120, 7), &preset, true, None);
    assert!(g.repl.is_some());
}

/// Every column rect, divider and drawer edge lies between the border
/// cells of `area`.
fn assert_inside_interior(area: Rect, g: &LayoutGeom) {
    let left = area.x + 1;
    let right = area.x + area.width - 2;
    let bottom = area.y + area.height - 2;
    for rect in [g.nav, g.preview, g.llm, g.repl].into_iter().flatten() {
        assert!(rect.x >= left, "{rect:?} starts left of the interior of {area:?}");
        assert!(rect.x + rect.width - 1 <= right, "{rect:?} ends right of the interior of {area:?}");
        assert!(rect.y + rect.height - 1 <= bottom, "{rect:?} ends below the interior of {area:?}");
    }
    for &x in g.vlines.iter().chain(g.drawer_x_end.iter()) {
        assert!(x >= left && x <= right, "divider at {x} is outside the interior of {area:?}");
    }
}

#[test]
fn every_column_and_divider_stays_inside_the_area() {
    // The widths renormalize on parse: 1,1,1.02,0.0001 becomes these. The
    // repl share asks for more than the room left, and llm is too small
    // to lay out, so the cap decides where the divider lands.
    let squeezed = LayoutPreset {
        columns: vec![
            Column { slot: Slot::Nav, width: 0.331_115 },
            Column { slot: Slot::Preview, width: 0.331_115 },
            Column { slot: Slot::Repl, width: 0.337_737 },
            Column { slot: Slot::Llm, width: 0.000_033 },
        ],
        drawer: Some(Slot::Repl),
        drawer_height: 0.40,
    };
    for preset in [
        squeezed,
        LayoutPreset::default_ultrawide(),
        LayoutPreset::default_laptop(),
        LayoutPreset::default_portrait(),
    ] {
        for width in 5..=130 {
            for height in 5..=40 {
                for drawer_open in [false, true] {
                    let a = area(width, height);
                    let g = compute_default(a, &preset, drawer_open);
                    assert_inside_interior(a, &g);
                }
            }
        }
    }
}
