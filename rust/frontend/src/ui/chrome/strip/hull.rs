//! The ships drawn on the band: marks, hull geometry, vertical placement and box-name ink.

use super::*;

/// Vertical lift (in cell-widths) applied to the ACTIVE session's name in the
/// bottom strip so it's distinguishable by POSITION, not just colour — it sits
/// raised above its peers like a selected tab.
///
/// The `2.0 / BASE_CELL_W` term is the owner's second look at the band: with the
/// strip tightened against the panes he asked the active name alone to rise
/// another 2 px, its peers staying put, so the selected tab reads at a glance
/// from the gap above it rather than from its ink. It is written in the same
/// cell-width unit as the 0.4 it adds to — a bare `+ 2.0` here would be a length
/// that does not scale with its neighbours, which is exactly the defect
/// `STRIP_TOP_AIR_ROWS` below was rewritten to delete. The lift rises INTO that
/// air, so raising it eats the air above the session names rather than the
/// band's row budget: the invariant test is what says how far it can go.
pub(in crate::ui) const STRIP_ACTIVE_LIFT_CELLS: f32 = 0.4;
/// Cells of waterline between a bow's wheel and its box name — the `_` in
/// `\ 0_-name-`. `ship_marks` alone spends it: the box name runs BENEATH the
/// session names on its own row, so no item width reserves the name or its air
/// (`strip_item_widths`), and the only thing that has to agree about where the
/// name starts is the line drawn around it.
const BOW_AIR_CELLS: f32 = 1.0;
/// Box-name ink — water blue `#58A6C4`, a tier of its own: 7.25:1 on the
/// strip's navy and 24–33° of hue off Julia blue `#4063D8`, which `AgentTone`
/// already spends on `Done`. The STEERED box takes the lighter tint of the same
/// water, `#8FD4EA`, so "which box do my keystrokes reach" is one step along
/// one hue rather than a sixth colour. Deliberately NOT the cream a session
/// name takes (`(250, 250, 215)`): pixel-identical ink made a host read as one
/// more session.
const BOX_NAME_RGB: (u8, u8, u8) = (88, 166, 196);
const BOX_NAME_STEERED_RGB: (u8, u8, u8) = (143, 212, 234);
/// Varnished-oak brown for a ship's hull — `#8B5A2B`, 3.4:1 against the
/// strip's midnight-navy background (above the 3:1 floor for a non-text
/// element) and 1.6× darker than the DIM fall-through ink, so a hull can
/// never read as a dimmed row. It paints a filled shape, never a name's ink,
/// so it spends no state meaning — same category as the border quads' grey.
pub(in crate::ui) const HULL_RGB: (u8, u8, u8) = (139, 90, 43);
/// Sea between the waterline's underside and the window's bottom edge, as a
/// fraction of `BASE_CELL_H`: **2 px at scale 1**, plus whatever the grid's row
/// remainder `u` adds to it. It is a named length because the band is no longer
/// measured in whole text rows — every px below the panes is now spent on
/// purpose, and this is the one that is left rather than drawn.
pub(in crate::ui) const STRIP_SEA_ROWS: f32 = 2.0 / BASE_CELL_H;
/// Air between the chrome grid's bottom edge — the panes' bottom border line —
/// and the session names, as a fraction of `BASE_CELL_H`: **0 at scale 1**. The
/// names start exactly where the grid's last row ends.
///
/// Zero is not "no air". The grid's last row IS the bottom border, and its
/// stroke is drawn from the cell's CENTRE (`project_border_quads` emits arms
/// from there), so `cell_h/2 + t/2` — 10 px at scale 1 — of that row lies below
/// the drawn line carrying nothing but the version stamp, far to the left of any
/// ship. The names sit in that dead space. Spending it is what lets the whole
/// band fit ONE reserved row instead of two, which is where the pane row this
/// change gives back comes from (owner: *"lets give pane one more row and get
/// the names below correct"*).
///
/// The band is placed from the GRID (`strip_row_tops`), so the distance from the
/// panes to the names is this constant at every window height and the
/// row-quantisation remainder `u` lands below the waterline as sea instead of
/// opening a gap here — the air test pins that at four different remainders.
///
/// Its floor is the active name's lift: the lifted name rises
/// `STRIP_ACTIVE_LIFT_CELLS * cell_w` (3.1 px at the measured advance) INTO this
/// air, so at 0 it rises into the border row's dead half — clearing the stroke
/// itself by `cell_h/2 - t/2 - lift`, 4.9 px at scale 1 and the measured
/// advance, and positive at every
/// scale. `strip_band_never_touches_the_grids_last_row` is what pins it.
pub(in crate::ui) const STRIP_TOP_AIR_ROWS: f32 = 0.0 / BASE_CELL_H;

/// The hull's glyph top below the session names' glyph top, as a fraction of
/// `BASE_CELL_H`: **14 px at scale 1**.
///
/// A PIXEL offset, not "one text row plus a drop" — the owner's *"do the session
/// names have to be on a row?"*. The names' glyph box is 18 px tall but their ink
/// ends 12 px into it, so the hull rides 4 px up into the box's dead descender
/// space without touching a letter, and the waterline — the part that runs under
/// the whole strip — still lands 25 px below the names' glyph top, clear of both
/// the deepest descender (12) and the bow wheel (17).
///
/// 14 is what makes the band fit ONE reserved row: with the air at 0 the budget
/// below the grid is `cell_h + oy` = 30 px at scale 1, spent 14 here, 11 on
/// `HULL_DROP_ROWS`, 3 on `HULL_THICKNESS_ROWS` and 2 on `STRIP_SEA_ROWS`. Every
/// px added here costs a px of sea; past 16 the reservation takes a second row
/// back and every pane loses the line this offset bought.
///
/// The rake's apex is placed from the BAND's top, so a shorter offset makes the
/// bow shallower rather than moving its apex: at 14 it rises 23 px, against 35
/// in rc9.6 and 27 in rc9.5.
pub(in crate::ui) const HULL_ROW_OFFSET_ROWS: f32 = 14.0 / BASE_CELL_H;
/// Hull thickness and waterline drop, as fractions of `BASE_CELL_H` — the row
/// height the mock was drawn against, named rather than a bare 18 in a
/// denominator so a change there can't silently detune the hull. The drop is
/// 11 — the locked rasteriser's own number. The wheel no longer shares this row
/// (it sits inline with the session names, `ship_vertical`), so the drop is set
/// by this row's typography alone: at 11 the line runs under the box name's
/// feet, which is what makes the name read as set INTO the water rather than
/// resting on it.
pub(in crate::ui) const HULL_THICKNESS_ROWS: f32 = 3.0 / BASE_CELL_H;
pub(in crate::ui) const HULL_DROP_ROWS: f32 = 11.0 / BASE_CELL_H;
/// Glyph-top drop of the box name inside the hull row, as a fraction of
/// `BASE_CELL_H` — 1 px at scale 1. At +0 the waterline rode high across the
/// letters instead of running along their feet.
const HULL_NAME_DROP_ROWS: f32 = 1.0 / BASE_CELL_H;
/// Where the bow rake's apex lands, as a fraction of `BASE_CELL_H` below the
/// BAND's top — 2 px at scale 1. The rake rises out of the waterline all the
/// way into the session-name row, so the `\` reads as one vessel's bow instead
/// of a notch in the line; 2 px of air keeps its top pixel inside the band.
const HULL_RAKE_TOP_ROWS: f32 = 2.0 / BASE_CELL_H;
/// Slack on the RIGHT of the break the box name leaves in the waterline, as a
/// fraction of ONE CELL. The break is sized to the character ADVANCE, but a
/// trailing `k`, `y`, `j` or `f` overhangs its advance and its ink blends into
/// the line (measured on an 8-letter host name). At 1–2 px the name still reads
/// as set into the water, so the slack costs nothing and buys the glyph its
/// edge back.
///
/// A fifth of a cell, NOT `1.0 / BASE_CELL_W`. The overhang is the font's ink
/// against its own advance, so it scales with the REAL `cell_w` — and the
/// shipped monospace advances ~7.7 px at scale 1, not `BASE_CELL_W`'s 9.0.
/// Against 9.0 the slack resolves to 0.86 px, LESS than the 1 px overhang it
/// exists to clear: a constant that looks right at the base cell and fails at
/// the actual one. A fifth of a cell is ~1.5 px at the shipped advance and
/// stays inside the 1–2 px that still reads as set-in.
const HULL_NAME_SLACK_CELLS: f32 = 0.2;
/// Horizontal run of the slanted bow rake, in cell-widths. It reaches back
/// from the bow item's left edge — its foot lands on the wheel's own left edge
/// — and `strip_gap_before` pays for the run ON TOP of the water a ship
/// boundary opens, so a rake can never reach over the stern of the ship ahead
/// of it: `STRIP_SHIP_GAP_CELLS` of navy always separate the two.
pub(in crate::ui) const HULL_BOW_RUN_CELLS: f32 = 3.0;


/// What one strip mark draws — the five pieces of one ship. `Wheel` is the
/// bow's brand wheel, full size for every ship and INLINE WITH THE SESSION
/// NAMES on the upper row (which one you steer is `BoxName`'s `steered`, spent
/// in the INK, so switching ships can never reflow the strip); `BoxName` is
/// that bow's host name, set into the waterline a row below; `Hull` is one flat
/// waterline segment; `BowRake` is the slanted bow, stepping up-left out of the
/// waterline as far as the wheel's own row; `Stern` is the short vertical riser
/// that closes the `\_|`. `ship_vertical` places all of them vertically.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::ui) enum StripMarkKind {
    Wheel,
    BoxName { name: String, steered: bool },
    Hull,
    BowRake,
    Stern,
}

/// One rect the strip actually draws: its screen-space left edge and width
/// (physical px), and what it is. Every mark goes through ONE cull into one
/// `Vec`, so what the draw loop paints is exactly what survived the cull.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::ui) struct StripMark {
    pub(in crate::ui) left: f32,
    pub(in crate::ui) w: f32,
    pub(in crate::ui) kind: StripMarkKind,
}

/// Screen-space left edge of a strip element whose strip-local left edge is
/// `local`. Every element in the band is centred on the active session the
/// same way, so this is the one copy of that arithmetic.
pub(in crate::ui) fn strip_screen_left(local: f32, scroll: f32, win_w: f32) -> f32 {
    win_w / 2.0 + (local - scroll)
}

/// The strip's one visibility predicate: an element fully off either window
/// edge is not drawn — and therefore not bracketed.
pub(in crate::ui) fn strip_visible(left: f32, w: f32, win_w: f32) -> bool {
    left + w >= 0.0 && left <= win_w
}

/// Every mark the fleet's ships contribute, culled once. `items` and
/// `item_positions`/`item_widths` are lock-step (the one cursor walk);
/// `logo_w` is the wheel's width — full size for every ship now, and the same
/// value the layout reserved (`strip_item_widths`' `wheel_w`), 0 with no
/// decoded logo; `bar_w` is the waterline's thickness, so a stern mark's span
/// is exactly the bar the draw site paints there.
///
/// `steered` is the host whose box name takes the bright ink —
/// `State::active_host`, the box your keystrokes actually reach, which stays
/// right even when the active session's row has just dropped out of
/// `workspace_slugs` (a lookup-by-index fallback would then mark the wrong
/// ship). `reachable` culls an unreachable box's ship, STRIP-LOCALLY: the
/// mock's rule is that the ship is simply gone, and this is the only place
/// that may act on it — filtering `workspace_slugs` was reverted in
/// `39def8d8` because three other consumers read that list. The steered box
/// keeps its ship whatever its link is doing, which is what makes the cull
/// safe where the filter was not: the one ship that must stay drawn is the one
/// whose name says where the keystrokes are going.
pub(in crate::ui) fn ship_marks(
    items: &[StripItem],
    item_positions: &[f32],
    item_widths: &[f32],
    steered: &HostKey,
    reachable: impl Fn(&HostKey) -> bool,
    logo_w: f32,
    cell_w: f32,
    bar_w: f32,
    scroll: f32,
    win_w: f32,
) -> Vec<StripMark> {
    let mut out = Vec::new();
    let mut push = |left: f32, w: f32, kind: StripMarkKind| {
        if w > 0.0 && strip_visible(left, w, win_w) {
            out.push(StripMark { left, w, kind });
        }
    };
    for (bow, stern) in ship_spans(items) {
        let (Some(&bow_x), Some(&bow_w)) = (item_positions.get(bow), item_widths.get(bow)) else {
            continue;
        };
        let StripItem::Bow { host, name } = &items[bow] else {
            continue;
        };
        let steering = host == steered;
        if !steering && !reachable(host) {
            continue;
        }
        let bow_left = strip_screen_left(bow_x, scroll, win_w);
        // `\__|`, left to right. The rake reaches back across the inter-ship
        // gap and puts its foot on the wheel's left edge, so the wheel sits
        // inside the bow and the waterline starts again past it.
        let run = HULL_BOW_RUN_CELLS * cell_w;
        push(bow_left - run, run, StripMarkKind::BowRake);
        if logo_w > 0.0 {
            push(bow_left, logo_w, StripMarkKind::Wheel);
        }
        // The stern stands `STRIP_STERN_CLEAR_CELLS` past the group's last
        // session name — that clearance is reserved by no item width, so it is
        // added here and paid for on the far side by `strip_gap_before`.
        let stern_r = item_positions
            .get(stern)
            .zip(item_widths.get(stern))
            .map(|(&x, &w)| strip_screen_left(x, scroll, win_w) + w)
            .unwrap_or(bow_left + bow_w)
            + STRIP_STERN_CLEAR_CELLS * cell_w;
        // The name sits past the wheel and its air, and CLAMPS to the window's
        // left edge once its own bow has scrolled off it: the name wins, the
        // waterline yields. The clamp only ever pushes the name RIGHT, and
        // never further right than its OWN stern leaves room for — so a ship
        // that has scrolled off ENTIRELY takes its name with it instead of
        // leaving it pinned to the edge, while a ship whose sessions are
        // NARROWER than its host name (the bow reserves the wheel alone now,
        // so that is reachable) can never have the name squeezed back over its
        // own wheel. The name is already truncated to `STRIP_MAX_LABEL` by the
        // caller, so a clamped one can't run away with the row either.
        let name_w = name.chars().count() as f32 * cell_w;
        let name_x = if logo_w > 0.0 {
            bow_left + logo_w + BOW_AIR_CELLS * cell_w
        } else {
            bow_left
        };
        let name_left = name_x + (-name_x).max(0.0).min((stern_r - name_w - name_x).max(0.0));
        // The waterline, in the two spans the name leaves it: rake's foot →
        // name and name → stern. Both are measured off the CLAMPED name, so the
        // line touches it on BOTH sides (`-name-`) however far the ship has
        // scrolled. The first run starts at the bow itself, PAST no wheel: the
        // wheel sits a row up now (`ship_vertical`), so a run starting at its
        // right edge left a wheel-wide hole in the line exactly where the rake
        // lands — the locked rasteriser runs the line under the disc. With no
        // wheel at all the name starts at the bow and this run is empty, which
        // `push`'s own `w > 0` guard drops.
        push(bow_left, name_left - bow_left, StripMarkKind::Hull);
        push(
            name_left,
            name_w,
            StripMarkKind::BoxName {
                name: name.clone(),
                steered: steering,
            },
        );
        // `HULL_NAME_SLACK_CELLS` of slack on the break's RIGHT: the break is
        // sized to the character advance, and a trailing descender overhangs it.
        let run_x = name_left + name_w + HULL_NAME_SLACK_CELLS * cell_w;
        push(run_x, stern_r - run_x, StripMarkKind::Hull);
        push(stern_r - bar_w, bar_w, StripMarkKind::Stern);
    }
    out
}

/// One flat waterline segment, `left` to `right` at `y..y+h`. `None` when the
/// name — or a scroll — left it no columns: the bow rake and the stern are
/// their own rects, so a ship with no flat run still draws its ends.
pub(in crate::ui) fn hull_bar_rect(left: f32, right: f32, y: f32, h: f32) -> Option<ScreenRect> {
    (right > left && h > 0.0).then(|| ScreenRect {
        x: left,
        y,
        w: right - left,
        h,
    })
}

/// The slanted bow — `\` — as axis-aligned bars stepping up-left from the
/// waterline's top surface at `left + run`, one bar per pixel of `rise`, so the
/// staircase is contiguous at every scale. Each bar carries the waterline's own
/// thickness `h` in BOTH axes (`step_w + h` wide, `h` tall): a 1 px step
/// vanished at true size, and a steep rake only reads at the line's weight if
/// its steps are as thick as the line. The rake now rises out of the waterline
/// into the SESSION-NAME row (`ship_vertical`'s `rake_rise`), which is inside
/// the band's two rows, so no row reservation has to grow for it. Stepped, not
/// rotated: the quad path is axis-aligned, and the owner chose the stepped
/// build over a new primitive, for speed.
pub(in crate::ui) fn hull_bow_rects(left: f32, run: f32, y: f32, h: f32, rise: f32) -> Vec<ScreenRect> {
    let steps = rise.floor();
    if run <= 0.0 || h <= 0.0 || steps < 1.0 {
        return Vec::new();
    }
    let step_w = run / steps;
    (0..steps as usize)
        .map(|i| ScreenRect {
            x: left + run - (i as f32 + 1.0) * step_w,
            y: y - i as f32,
            w: step_w + h,
            h,
        })
        .collect()
}

/// The vertical stern — `|` — that closes the hull: one bar-width bar at the
/// ship's `right` edge, from the waterline's bottom up to `top`. Axis-aligned
/// already, so unlike the bow it needs no stepping. `None` with no thickness or
/// nothing to rise through.
///
/// A SHORT riser, not the bow's twin: `top` is the hull row's own glyph top
/// (`ship_vertical` places it), so the stern spans one text row's worth of the
/// band — exactly where the `|` sits in the owner's ASCII — while the bow rakes
/// a whole row higher. The two ends of a ship are not symmetric; a stern as tall
/// as the bow read as a box around the session names.
pub(in crate::ui) fn hull_stern_rect(right: f32, y: f32, h: f32, top: f32) -> Option<ScreenRect> {
    (h > 0.0 && y + h > top).then(|| ScreenRect {
        x: right - h,
        y: top,
        w: h,
        h: y + h - top,
    })
}

/// The pane borders' bar width in physical px — `project_border_quads`' arms,
/// and anything that has to know where the drawn line's own edges are. ~9% of
/// cell height: a thin 1-2 px light-border weight that scales with DPI.
pub(in crate::ui) fn border_thickness_px(cell_h: f32) -> f32 {
    (cell_h * 0.09).round().max(1.0)
}

/// The waterline's y and thickness inside the ship row whose glyph-top is
/// `ship_y`. Both are fractions of `BASE_CELL_H` — the row height the mock was
/// drawn against, NAMED rather than left as a bare 18 in a denominator, so a
/// change there can't silently detune the hull — and the thickness carries the
/// same `.max(1.0)` floor the border quads do, so it can't thin to a sub-pixel
/// rect at small scale.
pub(in crate::ui) fn hull_band(ship_y: f32, cell_h: f32) -> (f32, f32) {
    (
        ship_y + cell_h * HULL_DROP_ROWS,
        (cell_h * HULL_THICKNESS_ROWS).max(1.0),
    )
}

/// Glyph-top y (physical px) of the strip's two rows — `(names, hull)`. Both are
/// measured DOWN from the chrome grid's bottom edge (`grid_bottom`, which is the
/// panes' bottom border line), so the air above the session names is
/// `STRIP_TOP_AIR_ROWS` at every window height and the row-quantisation
/// remainder the grid discards lands below the waterline as sea. Anchoring to
/// the window's bottom edge instead is what put that remainder in the gap.
/// `strip_reserved_rows` is what keeps the whole extent inside the window.
pub(in crate::ui) fn strip_row_tops(grid_bottom: f32, cell_h: f32) -> (f32, f32) {
    let names = grid_bottom + cell_h * STRIP_TOP_AIR_ROWS;
    (names, names + cell_h * HULL_ROW_OFFSET_ROWS)
}

/// Where one ship's parts sit VERTICALLY in the two-row band — the whole of the
/// locked vertical geometry, in one place, so no draw site re-derives a second
/// answer:
///
/// * `wheel_y` — the wheel's top, centred in the NAMES row. The owner's "the
///   wheel should be inline with the session names": it is not in the hull row
///   at all, which is what lets the hull be a LINE rather than a mass.
/// * `name_y` — the box name's glyph top, `HULL_NAME_DROP_ROWS` into the hull
///   row so the waterline runs along the letters' feet.
/// * `rake_rise` — pixels from the waterline up to the rake's apex, which lands
///   `HULL_RAKE_TOP_ROWS` below the band's top: the drop plus a whole row, less
///   that air.
///
/// The stern's top is the hull row's glyph top itself (`hull_stern_rect`'s
/// `top`), so it needs no field to name it. Every term is a fraction of
/// `BASE_CELL_H` times `cell_h`, so the band scales as one shape.
pub(in crate::ui) struct ShipVertical {
    pub(in crate::ui) wheel_y: f32,
    pub(in crate::ui) name_y: f32,
    pub(in crate::ui) rake_rise: f32,
}

pub(in crate::ui) fn ship_vertical(names_top: f32, hull_top: f32, cell_h: f32, logo_h: f32) -> ShipVertical {
    let (water_y, _) = hull_band(hull_top, cell_h);
    ShipVertical {
        wheel_y: names_top + (cell_h - logo_h) / 2.0,
        name_y: hull_top + cell_h * HULL_NAME_DROP_ROWS,
        rake_rise: water_y - (names_top + cell_h * HULL_RAKE_TOP_ROWS),
    }
}

/// Ink for one box name: water blue, the lighter tint for the box you are
/// steering (`BOX_NAME_RGB` / `BOX_NAME_STEERED_RGB`). The `--contrast-mode
/// dim` lever fades the plain ones the same way a non-active session name is
/// faded — and never the steered one, which is the one fact this row exists to
/// carry. Always an explicit colour, so a box name never falls through to the
/// text layer's default ink.
pub(in crate::ui) fn box_name_rgb(steered: bool, contrast_dim: bool) -> (u8, u8, u8) {
    if steered {
        BOX_NAME_STEERED_RGB
    } else if contrast_dim {
        scale_rgb(BOX_NAME_RGB, CONTRAST_DIM_FACTOR)
    } else {
        BOX_NAME_RGB
    }
}

#[cfg(test)]
#[path = "hull_tests.rs"]
mod tests;
