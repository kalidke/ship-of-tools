use super::*;

#[test]
fn strip_target_centers_active() {
    let labels = vec!["aa".to_string(), "bbbb".to_string(), "cc".to_string()];
    let cell_w = 10.0;
    let gap = STRIP_GAP_CELLS * cell_w;
    // item0: w=20 center=10; gap=30 → cursor=50
    // item1: w=40 center=70
    assert!((session_strip_target(&labels, 0, cell_w, &[]) - 10.0).abs() < 1e-3);
    assert!((session_strip_target(&labels, 1, cell_w, &[]) - (50.0 + 20.0)).abs() < 1e-3);
    let _ = gap;
}

#[test]
fn strip_target_active_centring_accounts_for_a_preceding_bow() {
    // The active item (item1) must be pushed over by exactly the bow's
    // own width plus the gap that bow opens (a ship boundary, wider than a
    // word space — `strip_gap_before`), less the word space the label-only
    // walk already spends there; its centring relative to item0 is
    // otherwise unaffected.
    let labels = vec!["aa".to_string(), "bbbb".to_string(), "cc".to_string()];
    let cell_w = 10.0;
    let wheel_w = 25.0;
    let slugs: Vec<WsKey> = vec![
        ("alpha".to_string(), "aa".to_string()),
        ("beta".to_string(), "bbbb".to_string()),
        ("beta".to_string(), "cc".to_string()),
    ];
    let items = vec![
        StripItem::Session(slugs[0].clone()),
        StripItem::Bow {
            host: "beta".to_string(),
            name: "beta".to_string(),
        },
        StripItem::Session(slugs[1].clone()),
        StripItem::Session(slugs[2].clone()),
    ];
    let label_widths: Vec<f32> = labels
        .iter()
        .map(|l| l.chars().count() as f32 * cell_w)
        .collect();
    let item_widths = strip_item_widths(&items, &label_widths, wheel_w);
    // The bow measures its wheel alone, and it opens a ship boundary.
    let bow_w = wheel_w;
    let shift = bow_w + strip_gap_before(&items[1], cell_w);
    let offsets = strip_divider_offsets(&items, &item_widths, &label_widths, cell_w);
    // No bow precedes item0.
    assert!((offsets[0]).abs() < 1e-3);
    // One bow — its wheel, plus the water its boundary opens — precedes
    // item1/2. The word space before item1 itself is spent by BOTH walks,
    // so it cancels and only the bow's own contribution is left.
    assert!((offsets[1] - shift).abs() < 1e-3, "{offsets:?}");
    assert!((offsets[2] - shift).abs() < 1e-3, "{offsets:?}");
    let no_bow_target = session_strip_target(&labels, 1, cell_w, &[]);
    let with_bow_target = session_strip_target(&labels, 1, cell_w, &offsets);
    assert!(
        (with_bow_target - (no_bow_target + shift)).abs() < 1e-3,
        "the active session's centring must shift by exactly the bow's width \
         and the water it opens"
    );
}

#[test]
fn a_reserved_grid_still_lays_out_every_pane() {
    // The other end of the reservation's chain: the sweep above stops at
    // `cell_grid_for`'s output, so nothing pinned that a grid which CAME
    // from it still lays out, nor that the pane rects stay inside it.
    // Windows short enough to reach `compute`'s degenerate guard (and the
    // `rows.max(1)` clamp under it) are out of scope here, as in the sweep.
    let preset = crate::settings::LayoutPreset::default_laptop();
    for &(h, scale) in &[
        (768.0_f32, 1.0_f32),
        (1058.0, 1.0),
        (1080.0, 1.0),
        (1440.0, 1.5),
        (2160.0, 2.0),
    ] {
        let cell_h = BASE_CELL_H * scale;
        let cell_w = 7.7 * scale;
        let (cols, rows) = cell_grid_for(
            1920,
            h as u32,
            cell_w,
            cell_h,
            BASE_CHROME_ORIGIN_X * scale,
            BASE_CHROME_ORIGIN_Y * scale,
        );
        let area = ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: cols,
            height: rows,
        };
        let geom = crate::layout::compute(area, &preset, true, None);
        let llm = geom.llm.expect("the Llm column must survive the reservation");
        let repl = geom.repl.expect("the drawer must survive the reservation");
        assert!(
            llm.height >= 1 && repl.height >= 3,
            "h {h} scale {scale}: a pane collapsed: {geom:?}"
        );
        assert!(
            llm.y + llm.height <= rows && repl.y + repl.height <= rows,
            "h {h} scale {scale}: a pane rect runs past the {rows}-row grid: {geom:?}"
        );
    }
}

#[test]
fn strip_band_never_touches_the_grids_last_row() {
    // BLOCKER 1: the strip used to float off `config.height` while the
    // chrome floats off the grid, so the grid's last row — the bottom
    // border line, carrying the FE/BE version stamp — sat under the
    // strip's names row at EVERY window height (worst at
    // `R = (h - 2*oy) mod cell_h == 8*scale`, where the two share a
    // baseline). `cell_grid_for` reserves the band once, and the band is
    // now placed off the grid it reserved from, so the two cannot drift.
    // The sweep used to exist because the reservation was 3 rows at or
    // below ~0.224, a scale-invariant bottom pad (since deleted) being the
    // one non-scaling length among scaling ones. It is now
    // `STRIP_TOP_AIR_ROWS`, asserted below as a literal BECAUSE it is
    // now an invariant — though one non-scaling length remains, the
    // `.max(1.0)` floor in `border_thickness_px`, which pins the stroke's
    // half-thickness below a ~5.5 px cell. The
    // sweep stays: it is what would catch a future term that forgets to
    // scale, which is the defect class this constant belonged to, and it
    // is where both ends of the air's range are pinned — the lifted active
    // name must clear the grid, and the hull row must stay in the window.
    //
    // Two limits of this sweep, both deliberate. It starts at
    // `240 * scale`, so it never reaches `cell_grid_for`'s `rows.max(1)`
    // clamp (above `h = 19.2` at scale 0.2); and under that clamp the band
    // genuinely does overlap — at scale 1.0 every window height at or
    // below 71 px — which is benign (no pane lays out at that size at all)
    // and is not a reason to widen the range.
    for &scale in &[0.2_f32, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0] {
        let cell_h = BASE_CELL_H * scale;
        let cell_w = 7.7 * scale; // the measured monospace advance
        let oy = BASE_CHROME_ORIGIN_Y * scale;
        let ox = BASE_CHROME_ORIGIN_X * scale;
        // Not the literal 2 (see `strip_reserved_rows`' doc): what holds
        // at every scale is that the reservation is the SMALLEST one that
        // clears the band.
        let k = strip_reserved_rows(cell_h, oy) as f32;
        let need = cell_h
            * (STRIP_TOP_AIR_ROWS
                + HULL_ROW_OFFSET_ROWS
                + HULL_DROP_ROWS
                + HULL_THICKNESS_ROWS
                + STRIP_SEA_ROWS);
        assert_eq!(
            k, 1.0,
            "scale {scale}: the band must never cost more than one row"
        );
        // Same tolerance as `strip_reserved_rows` itself, and for the
        // same reason: the band meets its budget exactly, so an
        // exact `>=` here would fail on f32 rounding rather than on
        // geometry. Well below a pixel, so a real overrun still fails.
        let fit_slack = cell_h * BAND_FIT_EPS_ROWS;
        assert!(
            k * cell_h + oy >= need - fit_slack,
            "scale {scale}: {k} rows do not clear the band's {need} px"
        );
        assert!(
            (k - 1.0) * cell_h + oy < need,
            "scale {scale}: {k} rows over-reserve"
        );
        let mut h = (240.0 * scale) as u32;
        while h <= (2160.0 * scale) as u32 {
            let (_, rows) = cell_grid_for(1920, h, cell_w, cell_h, ox, oy);
            let grid_bottom = oy + rows as f32 * cell_h;
            let (names_y, hull_y) = strip_row_tops(grid_bottom, cell_h);
            // The air's LOWER bound. The band's top now sits INSIDE the
            // grid's last row, in the dead space below the border stroke —
            // that space is exactly where the reclaimed pane row came from.
            // What the lifted name must never touch is the drawn line
            // itself, at every scale and every row remainder.
            let band_top = names_y - STRIP_ACTIVE_LIFT_CELLS * cell_w;
            let stroke_bottom =
                grid_bottom - cell_h * 0.5 + border_thickness_px(cell_h) * 0.5;
            assert!(
                band_top >= stroke_bottom,
                "scale {scale}, h {h}: the strip band (lifted top {band_top}) \
                 touches the border stroke, whose underside is at {stroke_bottom}"
            );
            // The air's UPPER bound: it comes out of `oy` plus whatever
            // the grid discarded, so an air above `oy` runs the band off
            // the bottom edge at the heights that discard nothing.
            assert!(
                hull_y + cell_h * (HULL_DROP_ROWS + HULL_THICKNESS_ROWS)
                    <= h as f32 + fit_slack,
                "scale {scale}, h {h}: the waterline runs off the window bottom"
            );
            // The hull is `HULL_ROW_OFFSET_ROWS` below the names — one
            // named distance, so only a change to the ARITHMETIC fails here.
            let apart = cell_h * HULL_ROW_OFFSET_ROWS;
            assert!(
                (hull_y - names_y - apart).abs() < 1e-3,
                "scale {scale}, h {h}: the hull is {} px below the names, but \
                 the offset is {apart} px",
                hull_y - names_y
            );
            h += 1;
        }
    }
}

#[test]
fn the_air_above_the_session_names_is_one_named_length_at_every_window_height() {
    // The distance the owner tunes is the one between the panes' bottom
    // border — the grid's last row — and the session names, and it must be
    // the SAME distance at every window height, or a spacing picked from
    // rendered samples is only reproducible at the height they were
    // captured at. `cell_grid_for` floors the grid to whole rows; the
    // remainder it discards, `u = (h - 2*oy) mod cell_h`, is a full row of
    // range (18*scale px) and it used to land in exactly this gap.
    let cell_h = BASE_CELL_H;
    let cell_w = 7.7; // the measured monospace advance, at scale 1
    let ox = BASE_CHROME_ORIGIN_X;
    let oy = BASE_CHROME_ORIGIN_Y;
    // Four heights whose discarded remainders are 6, 0, 8 and 12 px. The
    // default window (1050 logical) is the `u == 0` case — the tightest
    // one, and the only one the old geometry rendered as intended.
    let mut air: Vec<(u32, f32, f32)> = Vec::new();
    for &h in &[768_u32, 1050, 1058, 1080] {
        let u = (h as f32 - 2.0 * oy) % cell_h;
        let (_, rows) = cell_grid_for(1920, h, cell_w, cell_h, ox, oy);
        let grid_bottom = oy + rows as f32 * cell_h;
        let (names_y, _) = strip_row_tops(grid_bottom, cell_h);
        air.push((h, u, names_y - grid_bottom));
    }
    // The remainders really do differ, or the sweep proves nothing.
    assert!(
        air.iter().any(|&(_, u, _)| (u - air[0].1).abs() > 1e-3),
        "these heights share a remainder: {air:?}"
    );
    for &(h, u, a) in &air {
        assert!(
            (a - air[0].2).abs() < 1e-3,
            "h {h} (remainder {u}) leaves {a} px above the session names, \
             while h {} leaves {}",
            air[0].0,
            air[0].2
        );
        // And it is the named length, not merely a constant one — this is
        // the distance the owner sets, so the constant is what he moves.
        assert!(
            (a - cell_h * STRIP_TOP_AIR_ROWS).abs() < 1e-3,
            "h {h}: {a} px of air, but `STRIP_TOP_AIR_ROWS` asks for {}",
            cell_h * STRIP_TOP_AIR_ROWS
        );
    }
}

#[test]
fn strip_lines_active_is_bold_and_centered() {
    let labels = vec!["aa".to_string(), "bbbb".to_string(), "cc".to_string()];
    let cell_w = 10.0;
    let win_w = 800.0;
    let active = 1;
    // Scroll exactly at the active center → active is screen-centered.
    let scroll = session_strip_target(&labels, active, cell_w, &[]);
    let lines = session_strip_lines(
        &labels,
        active,
        scroll,
        win_w,
        cell_w,
        500.0,
        &[],
        false,
        &[],
        &[],
        &[],
    );
    // All three on-screen here.
    assert_eq!(lines.len(), 3);
    let act = &lines[1];
    assert!(act.bold && !act.dim, "active must be bold, not dim");
    // Active center == win_w/2: left + w/2 == 400.
    let w = 4.0 * cell_w;
    assert!(((act.x + w / 2.0) - win_w / 2.0).abs() < 1e-3);
    // Neighbours dim, not bold.
    assert!(lines[0].dim && !lines[0].bold);
    assert!(lines[2].dim && !lines[2].bold);
    // Ordering left→right: item0 left of item1 left of item2.
    assert!(lines[0].x < lines[1].x && lines[1].x < lines[2].x);
}

#[test]
fn strip_lines_culls_offscreen() {
    // Many items, narrow window → far items culled.
    let labels: Vec<String> = (0..50).map(|i| format!("ws{i}")).collect();
    let cell_w = 10.0;
    let win_w = 300.0;
    let active = 25;
    let scroll = session_strip_target(&labels, active, cell_w, &[]);
    let lines = session_strip_lines(
        &labels,
        active,
        scroll,
        win_w,
        cell_w,
        500.0,
        &[],
        false,
        &[],
        &[],
        &[],
    );
    assert!(
        lines.len() < labels.len(),
        "off-screen items must be culled"
    );
    // The active one is always present and centered.
    assert!(lines.iter().any(|l| l.bold));
}

#[test]
fn strip_lines_color_by_work_state() {
    // Three sessions, item0 active+idle, item1 working, item2 blocked.
    let labels = vec!["aa".to_string(), "bb".to_string(), "cc".to_string()];
    let cell_w = 10.0;
    let win_w = 800.0;
    let active = 0;
    let scroll = session_strip_target(&labels, active, cell_w, &[]);
    let tones = vec![
        Some((AgentTone::Idle, false)),
        Some((AgentTone::Working, false)),
        Some((AgentTone::Blocked, false)),
    ];
    let lines = session_strip_lines(
        &labels,
        active,
        scroll,
        win_w,
        cell_w,
        500.0,
        &tones,
        false,
        &[],
        &[],
        &[],
    );
    assert_eq!(lines.len(), 3);
    // Active idle keeps the bright default (idle = current colour), bold.
    assert_eq!(lines[0].color, Some((250, 250, 215)));
    assert!(lines[0].bold && !lines[0].dim);
    // Non-active working is green and NOT dim — visible off the active slot.
    assert_eq!(lines[1].color, AgentTone::Working.rgb());
    assert!(!lines[1].dim && !lines[1].bold);
    // Non-active blocked is red — the "needs you" signal pops.
    assert_eq!(lines[2].color, AgentTone::Blocked.rgb());
    assert!(!lines[2].dim);
}

#[test]
fn strip_active_coloured_row_is_distinct_without_bold() {
    // Maintainer report: two COLOURED (working) sessions, one active — they
    // collapsed to identical-looking rows because active-vs-non-active
    // differed ONLY by bold, and the monospace face renders no real bold.
    // The contrast lever fixes it by giving the active coloured row a
    // BRIGHTNESS cue independent of Weight::BOLD. Earlier strip tests only
    // covered active-IDLE (which stands out via the bright default path),
    // never active-COLOURED — this is that missing case.
    let labels = vec!["aa".to_string(), "bb".to_string()];
    let cell_w = 10.0;
    let win_w = 800.0;
    let scroll = session_strip_target(&labels, 0, cell_w, &[]);
    let tones = vec![
        Some((AgentTone::Working, false)),
        Some((AgentTone::Working, false)),
    ];
    let base = AgentTone::Working.rgb();
    // Bright lever: active (item0) scaled up, non-active (item1) at base.
    let bright = session_strip_lines(
        &labels,
        0,
        scroll,
        win_w,
        cell_w,
        500.0,
        &tones,
        false,
        &[],
        &[],
        &[],
    );
    assert_eq!(
        bright[0].color,
        base.map(|c| scale_rgb(c, CONTRAST_BRIGHT_FACTOR))
    );
    assert_eq!(bright[1].color, base);
    assert_ne!(
        bright[0].color, bright[1].color,
        "active coloured row must be distinct from a non-active same-tone row"
    );
    // Dim lever: active (item0) at base, non-active (item1) scaled down.
    let dim = session_strip_lines(
        &labels,
        0,
        scroll,
        win_w,
        cell_w,
        500.0,
        &tones,
        true,
        &[],
        &[],
        &[],
    );
    assert_eq!(dim[0].color, base);
    assert_eq!(
        dim[1].color,
        base.map(|c| scale_rgb(c, CONTRAST_DIM_FACTOR))
    );
    assert_ne!(dim[0].color, dim[1].color);
}

#[test]
fn strip_working_wilts_when_stale() {
    // A stale "working" still colours green but dims (the strip wilt).
    let labels = vec!["aa".to_string()];
    let cell_w = 10.0;
    let scroll = session_strip_target(&labels, 0, cell_w, &[]);
    let tones = vec![Some((AgentTone::Working, true))];
    let lines = session_strip_lines(
        &labels,
        9,
        scroll,
        800.0,
        cell_w,
        500.0,
        &tones,
        false,
        &[],
        &[],
        &[],
    );
    assert_eq!(lines[0].color, AgentTone::Working.rgb());
    assert!(lines[0].dim, "a wilted working name dims on the strip too");
}

#[test]
fn contrast_dim_strip_fades_non_active_plain_name() {
    // Strip "dim" lever: a non-active idle/no-agent name is baked to an
    // explicitly-dimmed colour (not the default DIM flag).
    let labels = vec!["aa".to_string(), "bb".to_string()];
    let cell_w = 10.0;
    let scroll = session_strip_target(&labels, 0, cell_w, &[]);
    let tones: Vec<Option<(AgentTone, bool)>> = vec![None, None];
    let lines = session_strip_lines(
        &labels,
        0,
        scroll,
        800.0,
        cell_w,
        500.0,
        &tones,
        true,
        &[],
        &[],
        &[],
    );
    // Active plain name stays bright + bold.
    assert_eq!(lines[0].color, Some((250, 250, 215)));
    assert!(lines[0].bold);
    // Non-active plain name carries an explicit dimmed colour, dim flag
    // off (the dim is baked into the colour).
    assert_eq!(
        lines[1].color,
        Some(scale_rgb((204, 204, 204), CONTRAST_DIM_FACTOR))
    );
    assert!(!lines[1].dim && !lines[1].bold);
}

#[test]
fn flash_brightens_strip_name_toward_white() {
    // A full flash (f=1.0) lerps a working-green name all the way to
    // white; a finished flash (f=0.0) leaves the tone untouched. Use a
    // *non-active* name (active=1) so the contrast lever doesn't also
    // scale it — isolating the flash.
    let labels = vec!["aa".to_string(), "bb".to_string()];
    let cell_w = 10.0;
    let scroll = session_strip_target(&labels, 1, cell_w, &[]);
    let tones = vec![
        Some((AgentTone::Working, false)),
        Some((AgentTone::Idle, false)),
    ];
    let lit = session_strip_lines(
        &labels,
        1,
        scroll,
        800.0,
        cell_w,
        500.0,
        &tones,
        false,
        &[1.0, 0.0],
        &[],
        &[],
    );
    assert_eq!(lit[0].color, Some((255, 255, 255)));
    let cold = session_strip_lines(
        &labels,
        1,
        scroll,
        800.0,
        cell_w,
        500.0,
        &tones,
        false,
        &[0.0, 0.0],
        &[],
        &[],
    );
    assert_eq!(cold[0].color, AgentTone::Working.rgb());
}

#[test]
fn strip_pending_badges_name_with_sigil_and_accent() {
    // Badge floor (ADR 0025 §1): a workspace flagged pending gets a leading
    // `●` sigil and bright white + bold on the bottom strip, overriding
    // whatever work-state colour it would otherwise carry — so it reads as
    // "result waiting here", distinct from working/idle/blocked. The view is
    // untouched; only the rendering changes.
    // bb is "working" (green) but ALSO pending — pending must win.
    let tones = vec![None, Some((AgentTone::Working, false))];
    let pendings = vec![false, true];
    // The sigil is part of the label (`strip_label`), as the draw site builds it.
    let labels: Vec<String> = ["aa", "bb"]
        .iter()
        .zip(&pendings)
        .map(|(l, &p)| strip_label(l, p))
        .collect();
    let cell_w = 10.0;
    let win_w = 800.0;
    let scroll = session_strip_target(&labels, 0, cell_w, &[]);
    let lines = session_strip_lines(
        &labels,
        0,
        scroll,
        win_w,
        cell_w,
        500.0,
        &tones,
        false,
        &[],
        &pendings,
        &[],
    );
    assert_eq!(lines.len(), 2);
    // Non-pending name keeps its plain text, no sigil.
    assert!(!lines[0].text.starts_with('●'));
    // Pending name carries the sigil + bright white, overriding the
    // green working tone, and is bold so it pops.
    assert!(
        lines[1].text.starts_with('●'),
        "pending name gets the ● sigil"
    );
    assert_eq!(
        lines[1].color,
        Some((255, 255, 255)),
        "pending name uses bright white, not the working green"
    );
    assert!(lines[1].bold, "pending name is bold so it stands out");
    assert!(!lines[1].dim);
}
