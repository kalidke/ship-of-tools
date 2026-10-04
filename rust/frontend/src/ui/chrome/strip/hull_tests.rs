use super::*;

/// Fixture for the scrolled two-host strip: eight 8-char session names
/// across two hosts, a window far narrower than the strip, and a scroll
/// that puts the whole first group off the left edge while the second
/// group's first NAME starts 10 px inside the window — and the second
/// bow, whose wheel shares that row, hangs off that edge, carrying its box
/// name (a row below) off it too.
fn scrolled_fleet() -> (Vec<StripMark>, f32, f32, f32) {
    let cell_w = 10.0;
    let win_w = 400.0;
    let logo_w = 25.0;
    let slugs: Vec<WsKey> = (0..8)
        .map(|i| {
            let host = if i < 4 { "alpha" } else { "beta" };
            (host.to_string(), format!("ws-{i}"))
        })
        .collect();
    let labels: Vec<String> = (0..8).map(|i| format!("sess-00{i}")).collect();
    let items = strip_items(&slugs, |h| h.clone());
    let label_widths: Vec<f32> = labels
        .iter()
        .map(|l| l.chars().count() as f32 * cell_w)
        .collect();
    let item_widths = strip_item_widths(&items, &label_widths, logo_w);
    let item_positions = strip_cursor_positions(&item_widths, |i| strip_gap_before(&items[i], cell_w));
    let offsets = strip_divider_offsets(&items, &item_widths, &label_widths, cell_w);
    // Place the first beta NAME (item 6) 10 px inside the window. The bow
    // reserves its wheel alone now, so the wheel and the `STRIP_GAP_CELLS`
    // after it are all that stand left of that name — 10 px is what puts
    // the whole bow, and the unclamped box name with it, off the edge.
    let scroll = win_w / 2.0 + item_positions[6] - 10.0;
    let strip_lines = session_strip_lines(
        &labels,
        4,
        scroll,
        win_w,
        cell_w,
        100.0,
        &[],
        false,
        &[],
        &[],
        &offsets,
    );
    assert_eq!(
        strip_lines.len(),
        4,
        "the whole first group must be culled for this to be the scrolled case"
    );
    let marks = ship_marks(
        &items,
        &item_positions,
        &item_widths,
        &"alpha".to_string(),
        |_| true,
        logo_w,
        cell_w,
        3.0,
        scroll,
        win_w,
    );
    (marks, win_w, cell_w, logo_w)
}

#[test]
fn a_ship_off_the_left_edge_draws_nothing_while_a_scrolled_bow_clamps_its_name() {
    // Two different things, in one scrolled fixture. The first group is
    // entirely off-screen left: every one of its marks — wheel, box name,
    // waterline, rake, stern — is gone, not clamped to the edge. The
    // second group's BOW has scrolled off while its stern has not, and
    // there the name wins: it clamps to the window's left edge and the
    // waterline yields to it on both sides.
    let (marks, win_w, cell_w, _) = scrolled_fleet();
    let names: Vec<(&str, f32)> = marks
        .iter()
        .filter_map(|m| match &m.kind {
            StripMarkKind::BoxName { name, .. } => Some((name.as_str(), m.left)),
            _ => None,
        })
        .collect();
    assert_eq!(
        names,
        vec![("beta", 0.0)],
        "one name survives, clamped to the left edge: {marks:?}"
    );
    assert!(
        !marks.iter().any(|m| m.kind == StripMarkKind::Wheel),
        "both wheels are off the left edge: {marks:?}"
    );
    assert!(
        !marks.iter().any(|m| m.kind == StripMarkKind::BowRake),
        "and so are both bow rakes: {marks:?}"
    );
    let flats: Vec<(f32, f32)> = marks
        .iter()
        .filter(|m| m.kind == StripMarkKind::Hull)
        .map(|m| (m.left, m.left + m.w))
        .collect();
    assert_eq!(
        flats.len(),
        2,
        "only the visible ship's waterline is drawn — the culled one's is \
         not clamped to the edge: {marks:?}"
    );
    assert!(
        flats[0].1.abs() < 1e-3
            && (flats[1].0 - (4.0 * cell_w + HULL_NAME_SLACK_CELLS * cell_w)).abs() < 1e-3,
        "both runs must still touch the CLAMPED name — the right one across \
         its 1 px of slack: {flats:?}"
    );
    assert!(
        flats[1].1 > win_w,
        "the visible ship still runs off the right edge: {flats:?}"
    );
}

#[test]
fn every_wheel_is_full_size_and_the_steered_box_is_marked_in_its_name() {
    // Items 3 + 5: the wheel no longer says which box you steer — every
    // one is the full logo size, so the layout reserves exactly what is
    // drawn and switching ships still cannot reflow the strip. The BOX
    // NAME's ink says it instead, keyed off `active_host` — the box the
    // keystrokes reach — not off an index into `workspace_slugs`, which
    // falls back to 0 (and so to the WRONG ship) whenever the steered
    // session's row has just been filtered out.
    let cell_w = 7.7; // the measured monospace advance at scale 1
    // The draw site's own asset geometry (`logo_dims`): the wheel is the
    // decoded PNG's aspect ratio taken at the row's height.
    let aspect = 1.0; // logo-dark is square
    let logo_h = (BASE_CELL_H - 2.0).max(1.0);
    let logo_w = logo_h * aspect;
    let slugs: Vec<WsKey> = vec![
        ("alpha".to_string(), "one".to_string()),
        ("beta".to_string(), "two".to_string()),
    ];
    let items = strip_items(&slugs, |h| h.clone());
    let label_widths = vec![30.0, 30.0];
    let item_widths = strip_item_widths(&items, &label_widths, logo_w);
    let item_positions = strip_cursor_positions(&item_widths, |i| strip_gap_before(&items[i], cell_w));
    let marks = ship_marks(
        &items,
        &item_positions,
        &item_widths,
        &"beta".to_string(),
        |_| true,
        logo_w,
        cell_w,
        3.0,
        0.0,
        4000.0,
    );
    let wheels: Vec<f32> = marks
        .iter()
        .filter(|m| m.kind == StripMarkKind::Wheel)
        .map(|m| m.w)
        .collect();
    assert_eq!(
        wheels,
        vec![logo_w, logo_w],
        "both bows carry the full-size wheel: {marks:?}"
    );
    let names: Vec<(&str, bool)> = marks
        .iter()
        .filter_map(|m| match &m.kind {
            StripMarkKind::BoxName { name, steered } => Some((name.as_str(), *steered)),
            _ => None,
        })
        .collect();
    assert_eq!(
        names,
        vec![("alpha", false), ("beta", true)],
        "the second ship is the steered one, and its NAME is what says so"
    );
    // The wheel fits inside ONE row at every scale — which is what lets it
    // be centred in the session-name row (`ship_vertical`'s `wheel_y`)
    // instead of in the hull row, where a line and a box name now live. A
    // fact about the ASSET geometry the draw site computes, not this
    // fixture.
    for scale in [0.5_f32, 1.0, 2.0, 4.0] {
        let cell_h = BASE_CELL_H * scale;
        let lh = (cell_h - 2.0).max(1.0);
        let top = (cell_h - lh) / 2.0;
        assert!(
            top >= 0.0 && top + lh <= cell_h,
            "scale {scale}: the full-size wheel must fit inside one text row"
        );
    }
}

#[test]
fn the_hull_is_a_flat_bar_a_stepped_bow_and_a_vertical_stern() {
    let cell_h = 18.0_f32;
    let names_y = 482.0;
    let ship_y = names_y + cell_h;
    let logo_h = (cell_h - 2.0).max(1.0);
    let (y, h) = hull_band(ship_y, cell_h);
    let vert = ship_vertical(names_y, ship_y, cell_h, logo_h);
    // The drop is 11 — the locked rasteriser's own number, low in the row
    // so the line runs under the box name's feet.
    assert!((y - (ship_y + 11.0)).abs() < 1e-3, "waterline drop: {y}");
    assert!((h - 3.0).abs() < 1e-3, "hull thickness: {h}");
    // The flat bar, and nothing when the name ate every column.
    let bar = hull_bar_rect(100.0, 300.0, y, h).expect("a bar with columns");
    assert!((bar.x - 100.0).abs() < 1e-3 && (bar.w - 200.0).abs() < 1e-3);
    assert!(
        bar.y >= ship_y && bar.y + bar.h <= ship_y + cell_h,
        "the bar stays inside the hull row: {bar:?}"
    );
    assert!(hull_bar_rect(300.0, 100.0, y, h).is_none());
    assert!(hull_bar_rect(100.0, 300.0, y, 0.0).is_none());
    // The bow `\`: one bar per pixel of rise, stepping up-LEFT, foot on the
    // waterline at `left + run`, each bar carrying the LINE's own thickness
    // in both axes — a 1 px rake vanished at true size.
    let run = HULL_BOW_RUN_CELLS * 7.7;
    let rise = vert.rake_rise;
    let bow = hull_bow_rects(100.0, run, y, h, rise);
    assert_eq!(
        bow.len(),
        rise as usize,
        "one step per pixel of rise: {bow:?}"
    );
    assert!(
        bow.iter().all(|r| (r.h - h).abs() < 1e-3),
        "every step carries the waterline's thickness: {bow:?}"
    );
    assert!(
        bow.iter().all(|r| r.w > h),
        "and carries it horizontally too, or a steep rake thins to nothing: {bow:?}"
    );
    assert!(
        (bow[0].x + bow[0].w - (100.0 + run + h)).abs() < 1e-3
            && (bow[0].y - y).abs() < 1e-3,
        "the foot sits on the waterline at the run's right end: {bow:?}"
    );
    for step in bow.windows(2) {
        assert!(
            step[1].x < step[0].x && step[1].y < step[0].y,
            "every step goes up and to the left: {bow:?}"
        );
    }
    let top = bow.last().expect("the rake has steps");
    // The rake rises a whole row higher than it used to: its apex is INSIDE
    // the session-name row, two px below the band's top, which is what makes
    // the `\` read as a bow instead of a notch in the line. It must still
    // never leave the band, or the reservation would have to grow for it.
    assert!(
        top.y >= names_y && top.y < names_y + cell_h,
        "the apex is inside the session-name row: {top:?}"
    );
    assert!(
        (top.y - (names_y + 3.0)).abs() < 1e-3,
        "and two px below the band's top, one px of it spent by the step \
         the staircase starts on: {top:?}"
    );
    assert!((top.x - 100.0).abs() < 1e-3, "and it lands on `left`: {top:?}");
    assert!(hull_bow_rects(100.0, 0.0, y, h, rise).is_empty());
    assert!(hull_bow_rects(100.0, run, y, h, 0.0).is_empty());
    // The stern `|`: vertical, bar-width, waterline bottom up to the hull
    // row's own glyph top — a SHORT riser, where the ASCII's `|` sits.
    let stern = hull_stern_rect(300.0, y, h, ship_y).expect("a stern");
    assert!((stern.w - h).abs() < 1e-3, "bar-width wide: {stern:?}");
    assert!(
        (stern.x + stern.w - 300.0).abs() < 1e-3,
        "at the ship's right edge: {stern:?}"
    );
    assert!(
        (stern.y + stern.h - (y + h)).abs() < 1e-3,
        "it stands on the waterline's bottom: {stern:?}"
    );
    assert!(
        (stern.y - ship_y).abs() < 1e-3 && stern.h <= cell_h,
        "and rises one text row, no further: {stern:?}"
    );
    // The two ends of a ship are NOT symmetric any more: the bow rakes up
    // into the names row, the stern stops at the line's own row. A stern as
    // tall as that bow read as a box drawn round the session names.
    let bow_h = (y + h) - top.y;
    assert!(
        stern.h < bow_h,
        "the stern must be shorter than the bow: stern {stern:?} vs bow top {top:?}"
    );
    assert!(
        stern.y + stern.h <= ship_y + cell_h,
        "and stays inside the hull row: {stern:?}"
    );
    assert!(hull_stern_rect(300.0, y, 0.0, ship_y).is_none());
    assert!(
        hull_stern_rect(300.0, y, h, y + h).is_none(),
        "nothing to rise through is no stern at all"
    );
    // Minimum-thickness floor: a small cell can't thin the hull to a
    // sub-pixel rect (the border quads carry the same `.max(1.0)`).
    assert!((hull_band(0.0, 4.0).1 - 1.0).abs() < 1e-3);
    // Both ratios scale with the row: double the cell, double the drop.
    assert!((hull_band(0.0, 36.0).0 - 22.0).abs() < 1e-3);
    assert!((hull_band(0.0, 36.0).1 - 6.0).abs() < 1e-3);
}

#[test]
fn the_bands_vertical_geometry_is_the_locked_one() {
    // The approved band, at real pixels: two text rows, the wheel INLINE
    // WITH THE SESSION NAMES on the upper one, the hull a line low in the
    // lower one with the box name set into it, and the rake reaching from
    // the line up into the wheel's own row. Every number is a fraction of
    // `BASE_CELL_H`, so the whole shape scales as one.
    //
    // The hull is placed by PIXEL offset, not by text row: at
    // `HULL_ROW_OFFSET_ROWS` it rides up into the dead descender space of
    // the names' own glyph box, which is what lets the whole band fit one
    // reserved row. The rake's rise is measured from the BAND's top, so a
    // shorter offset makes the bow shallower rather than moving its apex.
    let cell_h = BASE_CELL_H;
    // 600.0 is a grid bottom, not a window height: the band's internal
    // proportions are the same wherever the grid ends.
    let (names_y, hull_y) = strip_row_tops(600.0, cell_h);
    assert!(
        (hull_y - (names_y + cell_h * HULL_ROW_OFFSET_ROWS)).abs() < 1e-3,
        "the hull sits `HULL_ROW_OFFSET_ROWS` below the names: \
         {names_y} {hull_y}"
    );
    let logo_h = (cell_h - 2.0).max(1.0);
    let vert = ship_vertical(names_y, hull_y, cell_h, logo_h);
    // The wheel is centred in the NAMES row — its centre is that row's
    // centre, and it is nowhere near the hull row.
    assert!(
        (vert.wheel_y + logo_h / 2.0 - (names_y + cell_h / 2.0)).abs() < 1e-3,
        "the wheel's centre is the session-name row's centre: {}",
        vert.wheel_y
    );
    // Not "out of the hull row" — there is no hull ROW any more, the hull
    // is a pixel offset that deliberately rides up into the names' glyph
    // box. What the disc must clear is the drawn WATERLINE, which is the
    // thing the eye sees pass under the bow.
    let (disc_water_y, _) = hull_band(hull_y, cell_h);
    assert!(
        vert.wheel_y >= names_y && vert.wheel_y + logo_h <= disc_water_y,
        "and the whole disc stays above the waterline: {}",
        vert.wheel_y
    );
    // The box name drops 1 px into the hull row: at +0 the line rode high
    // across the letters instead of along their feet.
    assert!(
        (vert.name_y - (hull_y + 1.0)).abs() < 1e-3,
        "box-name glyph top: {}",
        vert.name_y
    );
    // The rake rises from the waterline to 2 px below the BAND's top. It is
    // stated from the band's top on purpose: while the rows were adjacent
    // that was the same arithmetic as "the waterline's drop plus a whole
    // row", and the two forms only part company once the hull row drops,
    // which is exactly when the distinction starts to matter. 23 px is
    // `HULL_ROW_OFFSET_ROWS` plus `HULL_DROP_ROWS` less that 2 px of air:
    // a shorter offset gives a shallower bow, not a lower apex.
    let (water_y, _) = hull_band(hull_y, cell_h);
    assert!(
        (vert.rake_rise - (water_y - (names_y + 2.0))).abs() < 1e-3
            && (vert.rake_rise - 23.0).abs() < 1e-3,
        "rake rise: {}",
        vert.rake_rise
    );
    assert!(
        (water_y - vert.rake_rise - (names_y + 2.0)).abs() < 1e-3,
        "and the apex lands inside the names row: {}",
        water_y - vert.rake_rise
    );
    // One shape at every scale: double the row and every length doubles.
    let double = ship_vertical(0.0, cell_h * 2.0, cell_h * 2.0, logo_h * 2.0);
    let single = ship_vertical(0.0, cell_h, cell_h, logo_h);
    assert!(
        (double.rake_rise - 2.0 * single.rake_rise).abs() < 1e-3
            && (double.name_y - 2.0 * single.name_y).abs() < 1e-3
            && (double.wheel_y - 2.0 * single.wheel_y).abs() < 1e-3,
        "the band must scale as one shape: {single:?} {double:?}",
        single = (single.wheel_y, single.name_y, single.rake_rise),
        double = (double.wheel_y, double.name_y, double.rake_rise),
    );
}

#[test]
fn ships_sit_five_cells_apart_and_sessions_three() {
    // A host boundary equal to one word space did no grouping work at all.
    // The fleet's own space is five cells of CLEAR WATER — measured where it
    // shows, between one ship's stern and the next ship's rake — while the
    // names inside a ship keep their three.
    let cell_w = 10.0;
    let logo_w = 16.0;
    let slugs: Vec<WsKey> = vec![
        ("alpha".to_string(), "one".to_string()),
        ("alpha".to_string(), "two".to_string()),
        ("beta".to_string(), "three".to_string()),
    ];
    let items = strip_items(&slugs, |h| h.clone());
    let label_widths = vec![40.0, 40.0, 40.0];
    let item_widths = strip_item_widths(&items, &label_widths, logo_w);
    let item_positions =
        strip_cursor_positions(&item_widths, |i| strip_gap_before(&items[i], cell_w));
    // Two adjacent sessions of one ship: one word space apart.
    assert!(
        (item_positions[2] - (item_positions[1] + item_widths[1]) - STRIP_GAP_CELLS * cell_w)
            .abs()
            < 1e-3,
        "sessions inside a ship: {item_positions:?}"
    );
    let marks = ship_marks(
        &items,
        &item_positions,
        &item_widths,
        &"alpha".to_string(),
        |_| true,
        logo_w,
        cell_w,
        3.0,
        0.0,
        4000.0,
    );
    let stern = marks
        .iter()
        .find(|m| m.kind == StripMarkKind::Stern)
        .expect("the first ship's stern");
    let rake = marks
        .iter()
        .filter(|m| m.kind == StripMarkKind::BowRake)
        .nth(1)
        .expect("the second ship's rake");
    assert!(
        (rake.left - (stern.left + stern.w) - STRIP_SHIP_GAP_CELLS * cell_w).abs() < 1e-3,
        "five cells of clear water from stern to the next rake's tip: \
         {stern:?} {rake:?}"
    );
}

#[test]
fn a_box_names_ink_is_water_and_never_the_active_sessions_cream() {
    // A tier of its own. The steered box takes the lighter water, so which
    // box your keystrokes reach is one step along one hue — and neither
    // water is the cream a session name takes, which is what made a host
    // read as a sixth session.
    let plain = box_name_rgb(false, false);
    let steered = box_name_rgb(true, false);
    assert_ne!(plain, steered, "the steered box must be told apart");
    assert_ne!(steered, (250, 250, 215), "never the active session's cream");
    assert_ne!(plain, (250, 250, 215));
    assert_eq!(steered, BOX_NAME_STEERED_RGB);
    assert_eq!(plain, BOX_NAME_RGB);
    // The `--contrast-mode dim` lever fades a plain box name and never the
    // steered one — that row carries one fact, and dimming it would blur it.
    assert_ne!(box_name_rgb(false, true), plain);
    assert_eq!(box_name_rgb(true, true), steered);
}

#[test]
fn a_ships_waterline_touches_its_box_name_on_both_sides() {
    // Item 4: `\ 0__-name-`. The name is inline ON the waterline, so the
    // two runs it leaves are measured off the name itself — it is never
    // left floating in a cell of air. One host, which is a ship too.
    let cell_w = 10.0;
    let logo_w = 16.0;
    let slugs: Vec<WsKey> = vec![
        ("alpha".to_string(), "one".to_string()),
        ("alpha".to_string(), "two".to_string()),
    ];
    let items = strip_items(&slugs, |h| h.clone());
    let label_widths = vec![40.0, 40.0];
    let item_widths = strip_item_widths(&items, &label_widths, logo_w);
    let item_positions = strip_cursor_positions(&item_widths, |i| strip_gap_before(&items[i], cell_w));
    let marks = ship_marks(
        &items,
        &item_positions,
        &item_widths,
        &"alpha".to_string(),
        |_| true,
        logo_w,
        cell_w,
        3.0,
        0.0,
        4000.0,
    );
    let (name_l, name_w) = marks
        .iter()
        .find_map(|m| match &m.kind {
            StripMarkKind::BoxName { .. } => Some((m.left, m.w)),
            _ => None,
        })
        .expect("the lone host's ship is drawn, bow and all");
    let flats: Vec<(f32, f32)> = marks
        .iter()
        .filter(|m| m.kind == StripMarkKind::Hull)
        .map(|m| (m.left, m.left + m.w))
        .collect();
    assert_eq!(
        flats.len(),
        2,
        "bow → name, then name → stern: {marks:?}"
    );
    assert!(
        (flats[0].1 - name_l).abs() < 1e-3,
        "the waterline must touch the name's LEFT edge: {flats:?}"
    );
    // The break clears the name's ADVANCE by a pixel on the right: a
    // trailing descender overhangs its own cell and its ink blended into
    // the line without it.
    assert!(
        (flats[1].0 - (name_l + name_w + HULL_NAME_SLACK_CELLS * cell_w)).abs() < 1e-3,
        "and its RIGHT edge, across 1 px of slack: {flats:?}"
    );
    // The first run starts at the BOW, not past the wheel: the wheel is a
    // row up, and the line runs under it — a run starting at its right edge
    // left a wheel-wide hole where the rake's foot lands.
    assert!(
        (flats[0].0 - (name_l - logo_w - BOW_AIR_CELLS * cell_w)).abs() < 1e-3,
        "the line runs from the rake's foot, under the wheel, to the name: {flats:?}"
    );
    // The rake's foot lands on the wheel's own left edge, and its run is
    // shorter than the water a ship boundary opens, so it can never reach
    // the ship ahead; the waterline runs on to the stern, which stands its
    // own clearance past the group's last session.
    let rake = marks
        .iter()
        .find(|m| m.kind == StripMarkKind::BowRake)
        .expect("a bow rake");
    let wheel = marks
        .iter()
        .find(|m| m.kind == StripMarkKind::Wheel)
        .expect("a wheel");
    assert!(
        (rake.left + rake.w - wheel.left).abs() < 1e-3,
        "the rake's foot is the wheel's left edge: {rake:?} {wheel:?}"
    );
    assert!(
        (rake.w - HULL_BOW_RUN_CELLS * cell_w).abs() < 1e-3
            && rake.w < STRIP_SHIP_GAP_CELLS * cell_w,
        "and its run fits inside a ship boundary's water: {rake:?}"
    );
    let stern = marks
        .iter()
        .find(|m| m.kind == StripMarkKind::Stern)
        .expect("a stern");
    let last_r = strip_screen_left(item_positions[2] + item_widths[2], 0.0, 4000.0)
        + STRIP_STERN_CLEAR_CELLS * cell_w;
    assert!(
        (stern.left + stern.w - last_r).abs() < 1e-3,
        "the stern closes the hull two cells past the group's last session: {stern:?}"
    );
    assert!(
        (flats[1].1 - last_r).abs() < 1e-3,
        "and the waterline runs all the way to it: {flats:?}"
    );
}

#[test]
fn an_unreachable_boxs_ship_is_gone_but_never_the_steered_boxs() {
    // Item 9: the cull is STRIP-LOCAL. `workspace_slugs` keeps every row
    // (three other consumers read it — the revert in `39def8d8`), the
    // layout keeps the culled bow's slot, and only its marks go. The box
    // you are steering keeps its ship whatever its link is doing: that is
    // the invariant which makes this cull safe where the filter was not,
    // because the bright name is what says where the keystrokes land.
    let cell_w = 10.0;
    let logo_w = 16.0;
    let slugs: Vec<WsKey> = vec![
        ("alpha".to_string(), "one".to_string()),
        ("beta".to_string(), "two".to_string()),
        ("gamma".to_string(), "three".to_string()),
    ];
    let items = strip_items(&slugs, |h| h.clone());
    let label_widths = vec![40.0, 40.0, 40.0];
    let item_widths = strip_item_widths(&items, &label_widths, logo_w);
    let item_positions = strip_cursor_positions(&item_widths, |i| strip_gap_before(&items[i], cell_w));
    let ships = |steered: &str, down: &'static str| -> Vec<StripMark> {
        ship_marks(
            &items,
            &item_positions,
            &item_widths,
            &steered.to_string(),
            |h| h.as_str() != down,
            logo_w,
            cell_w,
            3.0,
            0.0,
            4000.0,
        )
    };
    let named = |marks: &[StripMark]| -> Vec<String> {
        marks
            .iter()
            .filter_map(|m| match &m.kind {
                StripMarkKind::BoxName { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        named(&ships("alpha", "beta")),
        vec!["alpha".to_string(), "gamma".to_string()],
        "an unreachable box's ship is simply gone"
    );
    assert_eq!(
        named(&ships("beta", "beta")),
        vec![
            "alpha".to_string(),
            "beta".to_string(),
            "gamma".to_string()
        ],
        "but never the steered box's own, whatever its link is doing"
    );
    // And nothing else moves: a dropped link cannot reflow the strip.
    let all = ships("alpha", "none");
    let without = ships("alpha", "beta");
    assert!(
        without.iter().all(|m| all.contains(m)),
        "culling one ship moved another's marks: {without:?}"
    );
    assert_eq!(
        all.len() - without.len(),
        6,
        "one ship is six marks — rake, wheel, two waterline runs, name, stern"
    );
}

/// The owner's report: the badge "makes them longer text and the last
/// session name hangs outside the ship hull". The hull is measured from
/// `labels`, so the names must be drawn from the very same text, or every
/// badge pushes every later name a cell past the slot its hull reserved.
/// Three badged sessions lead the first ship (badged rows sort first,
/// `activity_rank`) with the longest name last; a second ship follows, so
/// drift into the next ship is caught too.
#[test]
fn a_badged_name_stays_inside_its_hull() {
    let cell_w = 10.0;
    let win_w = 4000.0;
    let logo_w = 20.0;
    let bar_w = 3.0;
    let slugs: Vec<WsKey> = (0..5)
        .map(|i| {
            let host = if i < 4 { "alpha" } else { "beta" };
            (host.to_string(), format!("ws-{i}"))
        })
        .collect();
    let raw = ["aa", "bb", "cc", "the-longest-session-name-of-them-all", "dd"];
    let pendings = vec![true, true, true, false, false];
    // Exactly as the draw site builds them.
    let labels: Vec<String> = raw
        .iter()
        .zip(&pendings)
        .map(|(l, &p)| strip_label(l, p))
        .collect();
    let items = strip_items(&slugs, |h| h.clone());
    let label_widths: Vec<f32> = labels
        .iter()
        .map(|l| l.chars().count() as f32 * cell_w)
        .collect();
    let item_widths = strip_item_widths(&items, &label_widths, logo_w);
    let item_positions =
        strip_cursor_positions(&item_widths, |i| strip_gap_before(&items[i], cell_w));
    let offsets = strip_divider_offsets(&items, &item_widths, &label_widths, cell_w);
    let scroll = session_strip_target(&labels, 0, cell_w, &offsets);
    let lines = session_strip_lines(
        &labels, 0, scroll, win_w, cell_w, 100.0, &[], false, &[], &pendings, &offsets,
    );
    let marks = ship_marks(
        &items,
        &item_positions,
        &item_widths,
        &"alpha".to_string(),
        |_| true,
        logo_w,
        cell_w,
        bar_w,
        scroll,
        win_w,
    );
    let sterns: Vec<&StripMark> = marks
        .iter()
        .filter(|m| m.kind == StripMarkKind::Stern)
        .collect();
    assert_eq!(lines.len(), 5, "every name is on-screen");
    assert_eq!(sterns.len(), 2, "both ships are on-screen: {marks:?}");
    for (i, line) in lines.iter().enumerate() {
        let ship = if i < 4 { 0 } else { 1 };
        let right = line.x + line.text.chars().count() as f32 * cell_w;
        assert!(
            right <= sterns[ship].left,
            "name {i} {:?} ends at {right}, past ship {ship}'s stern at {}",
            line.text,
            sterns[ship].left
        );
    }
    // Each ship's last name keeps the clearance an unbadged ship has: the
    // badge costs the hull a cell, never the stern its gap.
    for (ship, last) in [(0usize, 3usize), (1, 4)] {
        let right = lines[last].x + lines[last].text.chars().count() as f32 * cell_w;
        let stern_r = sterns[ship].left + sterns[ship].w;
        assert!(
            (stern_r - right - STRIP_STERN_CLEAR_CELLS * cell_w).abs() < 1e-3,
            "ship {ship}: {} px from its last name to its stern, want {}",
            stern_r - right,
            STRIP_STERN_CLEAR_CELLS * cell_w
        );
    }
}
