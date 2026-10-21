use super::*;

// ADR 0022: source-pixel ROI mapping.
#[test]
fn roi_fit_is_full_image() {
    // Canvas exactly fills the pane (zoom 1, pane-aspect image): whole image.
    let r = visible_roi_px(0.0, 0.0, 800.0, 600.0, 0.0, 0.0, 800.0, 600.0, 800, 600);
    assert_eq!(r, Some((0, 0, 800, 600)));
}

#[test]
fn roi_centered_zoom_is_center_quarter() {
    // 2× canvas centered on the pane → the central half in each axis is
    // visible = source [200..600) × [150..450) for an 800×600 image.
    let (cw, ch) = (1600.0, 1200.0);
    let (px, py, pw, ph) = (0.0, 0.0, 800.0, 600.0);
    let (cx, cy) = (px + pw * 0.5 - cw * 0.5, py + ph * 0.5 - ch * 0.5); // centered
    let r = visible_roi_px(cx, cy, cw, ch, px, py, pw, ph, 800, 600).unwrap();
    assert_eq!(r, (200, 150, 400, 300));
}

#[test]
fn roi_pan_to_top_left_corner() {
    // 2× canvas panned so its top-left aligns with the pane origin → the
    // top-left quarter of the source is visible.
    let (cw, ch) = (1600.0, 1200.0);
    let r = visible_roi_px(0.0, 0.0, cw, ch, 0.0, 0.0, 800.0, 600.0, 800, 600).unwrap();
    assert_eq!(r, (0, 0, 400, 300));
}

#[test]
fn roi_none_when_offscreen_or_degenerate() {
    // Canvas entirely left of the pane → nothing visible.
    assert_eq!(
        visible_roi_px(-2000.0, 0.0, 800.0, 600.0, 0.0, 0.0, 800.0, 600.0, 800, 600),
        None
    );
    // Zero source dims → None.
    assert_eq!(
        visible_roi_px(0.0, 0.0, 800.0, 600.0, 0.0, 0.0, 800.0, 600.0, 0, 600),
        None
    );
}

// ADR 0025 `preview --roi`: the zoom/pan solve (inverse of visible_roi_px).
#[test]
fn solve_roi_full_image_is_fit() {
    // Square image in a square pane (letterbox == pane), whole image
    // requested → zoom 1, centred.
    let (z, pan) = solve_roi_view(
        800.0,
        800.0,
        800.0,
        800.0,
        16.0,
        800,
        800,
        RoiRect {
            x: 0,
            y: 0,
            w: 800,
            h: 800,
        },
    )
    .unwrap();
    assert!((z - 1.0).abs() < 1e-4);
    assert!(pan.0.abs() < 1e-2 && pan.1.abs() < 1e-2);
}

#[test]
fn solve_roi_centred_quarter_zooms_2x_no_pan() {
    let (z, pan) = solve_roi_view(
        800.0,
        800.0,
        800.0,
        800.0,
        16.0,
        800,
        800,
        RoiRect {
            x: 200,
            y: 200,
            w: 400,
            h: 400,
        },
    )
    .unwrap();
    assert!((z - 2.0).abs() < 1e-4);
    assert!(pan.0.abs() < 1e-2 && pan.1.abs() < 1e-2);
}

#[test]
fn solve_roi_top_left_quarter_pans_positive() {
    // Rect centre at source fraction 0.25 → pan = (0.5 - 0.25) × the
    // 1600 px canvas = +400 px each axis (canvas slides right/down so the
    // top-left content lands mid-pane).
    let (z, pan) = solve_roi_view(
        800.0,
        800.0,
        800.0,
        800.0,
        16.0,
        800,
        800,
        RoiRect {
            x: 0,
            y: 0,
            w: 400,
            h: 400,
        },
    )
    .unwrap();
    assert!((z - 2.0).abs() < 1e-4);
    assert!((pan.0 - 400.0).abs() < 1e-1);
    assert!((pan.1 - 400.0).abs() < 1e-1);
}

#[test]
fn solve_roi_round_trips_through_visible_roi_px() {
    // Applying the solved zoom/pan through the render pass's canvas
    // placement + slack clamp and mapping back with visible_roi_px must
    // recover a superset of the requested rect (non-pane-aspect image, so
    // letterbox ≠ pane and one axis shows extra context).
    let (pane_w, pane_h) = (1000.0_f32, 500.0_f32);
    let (src_w, src_h) = (800_u32, 600_u32);
    let fit = (pane_w / src_w as f32).min(pane_h / src_h as f32);
    let (lw, lh) = (src_w as f32 * fit, src_h as f32 * fit);
    let req = RoiRect {
        x: 100,
        y: 150,
        w: 200,
        h: 120,
    };
    let zoom_max = png_zoom_max(pane_w, pane_h, (src_w, src_h));
    let (z, (px, py)) =
        solve_roi_view(pane_w, pane_h, lw, lh, zoom_max, src_w, src_h, req).unwrap();
    let (cw, ch) = (lw * z, lh * z);
    let slack_x = (cw - pane_w).max(0.0);
    let slack_y = (ch - pane_h).max(0.0);
    let px = px.clamp(-slack_x * 0.5, slack_x * 0.5);
    let py = py.clamp(-slack_y * 0.5, slack_y * 0.5);
    let canvas_x = pane_w * 0.5 - cw * 0.5 + px; // pane at origin
    let canvas_y = pane_h * 0.5 - ch * 0.5 + py;
    let (ex, ey, ew, eh) = visible_roi_px(
        canvas_x, canvas_y, cw, ch, 0.0, 0.0, pane_w, pane_h, src_w, src_h,
    )
    .unwrap();
    assert!(ex <= req.x + 2 && ey <= req.y + 2, "({ex},{ey})");
    assert!(
        ex + ew + 2 >= req.x + req.w && ey + eh + 2 >= req.y + req.h,
        "({ex},{ey},{ew},{eh})"
    );
}

#[test]
fn caption_band_and_image_rect_tile_the_pane_exactly() {
    let pane = ScreenRect {
        x: 40.0,
        y: 10.0,
        w: 1000.0,
        h: 500.0,
    };
    // No caption → the image gets the whole pane (the pre-caption behaviour
    // must be bit-identical, since most previews have no caption).
    let full = image_rect_for_caption(pane, 0.0);
    assert_eq!(
        (full.x, full.y, full.w, full.h),
        (pane.x, pane.y, pane.w, pane.h)
    );
    // With a caption the image shrinks by EXACTLY the band, and only in h —
    // a drifting x/w would shift the letterbox and skew the ROI mapping.
    let band_h = 64.0;
    let img = image_rect_for_caption(pane, band_h);
    assert_eq!((img.x, img.y, img.w), (pane.x, pane.y, pane.w));
    assert_eq!(img.h, pane.h - band_h);
    // The band starts exactly where the image ends: no overlap (the caption
    // would cover the figure) and no gap (a dead strip).
    let band_top = pane.y + pane.h - band_h;
    assert_eq!(img.y + img.h, band_top);
    assert_eq!(band_top + band_h, pane.y + pane.h);
    // Degenerate guard: a band taller than the pane can't invert the rect.
    assert!(image_rect_for_caption(pane, 10_000.0).h >= 1.0);
}

#[test]
fn keyboard_pane_derivation_subtracts_the_caption_band() {
    // THE regression guard for the PR #72 review finding. It must exercise
    // the function the keyboard handler actually calls — an earlier version
    // of this test called `png_zoom_max`/`image_rect_for_caption` directly
    // and would have passed green with the handler reverted to the raw pane,
    // pinning the bug's premise instead of its fix.
    let cells = (150_u16, 42_u16);
    let (cw, ch) = (8.0_f32, 17.0_f32);
    let band = 38.0_f32;
    let full = preview_image_pane_px(cells, cw, ch, 0.0);
    let reduced = preview_image_pane_px(cells, cw, ch, band);
    // Delete the band subtraction and THIS fails.
    assert_eq!(reduced.h, full.h - band);
    assert_eq!(reduced.w, full.w, "the band never touches width");
    // The consequence the user feels: on a height-constrained image the
    // reachable zoom ceiling must rise, not stay pinned to the full pane.
    let tall = (700_u32, 1900_u32);
    assert!(
        png_zoom_max(reduced.w, reduced.h, tall) > png_zoom_max(full.w, full.h, tall),
        "reduced pane must yield MORE zoom headroom on a tall image"
    );
    // A width-constrained image is unaffected — pins the scope of the fix.
    let wide = (1600_u32, 400_u32);
    assert_eq!(
        png_zoom_max(reduced.w, reduced.h, wide),
        png_zoom_max(full.w, full.h, wide),
    );
    // And the keyboard derivation must agree with the render path's, which
    // starts from an ORIGIN-BEARING pane rect: same extent, same ceiling.
    let render = image_rect_for_caption(
        ScreenRect {
            x: 31.0,
            y: 17.0,
            w: cells.0 as f32 * cw,
            h: cells.1 as f32 * ch,
        },
        band,
    );
    assert_eq!((reduced.w, reduced.h), (render.w, render.h));
    assert_eq!(
        png_zoom_max(reduced.w, reduced.h, tall),
        png_zoom_max(render.w, render.h, tall),
    );
}

#[test]
fn solve_roi_round_trips_through_a_caption_reduced_rect() {
    // The reserve-space chain's real hazard: if any consumer kept the FULL
    // pane rect while the image was letterboxed into the reduced one, the
    // --roi round-trip would skew silently. Same round-trip as
    // `solve_roi_round_trips_through_visible_roi_px`, but every step is fed
    // the caption-reduced rect — it must still recover the requested rect.
    let pane = ScreenRect {
        x: 0.0,
        y: 0.0,
        w: 1000.0,
        h: 500.0,
    };
    let img = image_rect_for_caption(pane, 80.0);
    let (src_w, src_h) = (800_u32, 600_u32);
    let fit = (img.w / src_w as f32).min(img.h / src_h as f32);
    let (lw, lh) = (src_w as f32 * fit, src_h as f32 * fit);
    let req = RoiRect {
        x: 100,
        y: 150,
        w: 200,
        h: 120,
    };
    let zoom_max = png_zoom_max(img.w, img.h, (src_w, src_h));
    let (z, (px, py)) =
        solve_roi_view(img.w, img.h, lw, lh, zoom_max, src_w, src_h, req).unwrap();
    let (cw, ch) = (lw * z, lh * z);
    let slack_x = (cw - img.w).max(0.0);
    let slack_y = (ch - img.h).max(0.0);
    let px = px.clamp(-slack_x * 0.5, slack_x * 0.5);
    let py = py.clamp(-slack_y * 0.5, slack_y * 0.5);
    let canvas_x = img.x + img.w * 0.5 - cw * 0.5 + px;
    let canvas_y = img.y + img.h * 0.5 - ch * 0.5 + py;
    let (ex, ey, ew, eh) = visible_roi_px(
        canvas_x, canvas_y, cw, ch, img.x, img.y, img.w, img.h, src_w, src_h,
    )
    .unwrap();
    assert!(ex <= req.x + 2 && ey <= req.y + 2, "({ex},{ey})");
    assert!(
        ex + ew + 2 >= req.x + req.w && ey + eh + 2 >= req.y + req.h,
        "({ex},{ey},{ew},{eh})"
    );
}

#[test]
fn solve_roi_zoom_ceiling_clamps() {
    // A 4×4 px aim in an 800 px image wants zoom 200; the ceiling (16
    // px/src-px at fit 1) caps it — same ceiling interactive zoom obeys.
    let zoom_max = png_zoom_max(800.0, 800.0, (800, 800));
    let (z, _) = solve_roi_view(
        800.0,
        800.0,
        800.0,
        800.0,
        zoom_max,
        800,
        800,
        RoiRect {
            x: 0,
            y: 0,
            w: 4,
            h: 4,
        },
    )
    .unwrap();
    assert_eq!(z, zoom_max);
}

#[test]
fn solve_roi_degenerate_is_none() {
    let r = RoiRect {
        x: 0,
        y: 0,
        w: 10,
        h: 10,
    };
    assert!(solve_roi_view(0.0, 800.0, 800.0, 800.0, 16.0, 800, 800, r).is_none());
    assert!(solve_roi_view(800.0, 800.0, 800.0, 800.0, 16.0, 0, 800, r).is_none());
    assert!(solve_roi_view(
        800.0,
        800.0,
        800.0,
        800.0,
        16.0,
        800,
        800,
        RoiRect {
            x: 0,
            y: 0,
            w: 0,
            h: 10
        }
    )
    .is_none());
}

#[test]
fn view_carry_is_a_fixed_point_under_the_quantization_hysteresis() {
    // The carry loops save (`visible_roi_px`, outward floor/ceil) into
    // restore (`solve_roi_view`) on every A↔B flip. Quantization expands
    // the rect by up to a pixel per edge per round trip, so a cache that
    // rewrote every readback would zoom out by a creep — invisible per
    // flip, obvious after dozens. Pin both halves of the defense:
    // (1) a restore's readback stays within the ±1 px/edge window of the
    //     rect it restored (the hysteresis premise), and
    // (2) with the incumbent kept, repeated flips re-solve the SAME rect
    //     to bit-identical zoom/pan — a true fixed point after the first
    //     restore.
    let pane = ScreenRect {
        x: 7.0,
        y: 11.0,
        w: 1231.0,
        h: 803.0,
    };
    let img = image_rect_for_caption(pane, 57.0);
    let (src_w, src_h) = (12730_u32, 12826_u32);
    let fit = (img.w / src_w as f32).min(img.h / src_h as f32);
    let (lw, lh) = (src_w as f32 * fit, src_h as f32 * fit);
    let zoom_max = png_zoom_max(img.w, img.h, (src_w, src_h));
    // The render pass's clamp + canvas placement + ROI readback.
    let readback = |z: f32, pan: (f32, f32)| -> RoiRect {
        let z = z.clamp(1.0, zoom_max);
        let (cw, ch) = (lw * z, lh * z);
        let sx = (cw - img.w).max(0.0);
        let sy = (ch - img.h).max(0.0);
        let px = pan.0.clamp(-sx * 0.5, sx * 0.5);
        let py = pan.1.clamp(-sy * 0.5, sy * 0.5);
        let cx = img.x + img.w * 0.5 - cw * 0.5 + px;
        let cy = img.y + img.h * 0.5 - ch * 0.5 + py;
        let (x, y, w, h) =
            visible_roi_px(cx, cy, cw, ch, img.x, img.y, img.w, img.h, src_w, src_h)
                .expect("view visible");
        RoiRect { x, y, w, h }
    };
    // A user view: zoomed well in, panned off-centre — then saved.
    let saved = readback(9.7, (313.0, -211.0));
    // First restore (the B→A flip) and its write-through readback.
    let (z1, p1) = solve_roi_view(img.w, img.h, lw, lh, zoom_max, src_w, src_h, saved).unwrap();
    let echo = readback(z1, p1);
    assert!(
        roi_rects_within_quantization(saved, echo),
        "readback escaped the hysteresis window: {saved:?} -> {echo:?}"
    );
    // Hysteresis keeps `saved` as the incumbent, so every later flip
    // re-solves the identical rect: zoom/pan are exactly reproduced.
    for _ in 0..8 {
        let (z, p) =
            solve_roi_view(img.w, img.h, lw, lh, zoom_max, src_w, src_h, saved).unwrap();
        assert_eq!((z, p), (z1, p1), "flip drifted off the fixed point");
    }
}

#[test]
fn caption_flip_restores_the_same_source_region() {
    // The reported bug: same-size neighbors in one directory, one with a
    // figure caption. The screen-px carry restored an offset (rect
    // centre shifts by half the band) wrongly-magnified (fit changes)
    // view. The ROI carry must land the SAME source region under either
    // geometry — centred, and never showing less than what was saved
    // (min-axis zoom may show more context on the shorter rect).
    let pane = ScreenRect {
        x: 0.0,
        y: 0.0,
        w: 1200.0,
        h: 780.0,
    };
    let plain = image_rect_for_caption(pane, 0.0);
    let banded = image_rect_for_caption(pane, 96.0); // 3-line caption
    let (src_w, src_h) = (4096_u32, 4096_u32);
    let saved = RoiRect {
        x: 1500,
        y: 2200,
        w: 600,
        h: 390,
    };
    for img in [plain, banded] {
        let fit = (img.w / src_w as f32).min(img.h / src_h as f32);
        let (lw, lh) = (src_w as f32 * fit, src_h as f32 * fit);
        let zoom_max = png_zoom_max(img.w, img.h, (src_w, src_h));
        let (z, pan) =
            solve_roi_view(img.w, img.h, lw, lh, zoom_max, src_w, src_h, saved).unwrap();
        let (cw, ch) = (lw * z, lh * z);
        let sx = (cw - img.w).max(0.0);
        let sy = (ch - img.h).max(0.0);
        let px = pan.0.clamp(-sx * 0.5, sx * 0.5);
        let py = pan.1.clamp(-sy * 0.5, sy * 0.5);
        let cx = img.x + img.w * 0.5 - cw * 0.5 + px;
        let cy = img.y + img.h * 0.5 - ch * 0.5 + py;
        let (ex, ey, ew, eh) =
            visible_roi_px(cx, cy, cw, ch, img.x, img.y, img.w, img.h, src_w, src_h).unwrap();
        assert!(
            ex <= saved.x + 1 && ey <= saved.y + 1,
            "lost the region start: ({ex},{ey})"
        );
        assert!(
            ex + ew + 1 >= saved.x + saved.w && ey + eh + 1 >= saved.y + saved.h,
            "lost the region end: ({ex},{ey},{ew},{eh})"
        );
        let (scx, scy) = (
            saved.x as f32 + saved.w as f32 / 2.0,
            saved.y as f32 + saved.h as f32 / 2.0,
        );
        let (ecx, ecy) = (ex as f32 + ew as f32 / 2.0, ey as f32 + eh as f32 / 2.0);
        assert!(
            (ecx - scx).abs() <= 2.0 && (ecy - scy).abs() <= 2.0,
            "restored centre drifted: ({ecx},{ecy}) vs ({scx},{scy})"
        );
    }
}
