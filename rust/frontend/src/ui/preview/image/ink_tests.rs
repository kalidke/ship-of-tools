use super::*;
use sot_protocol::ink::raster_dims;

const R: ScreenRect = ScreenRect { x: 100.0, y: 50.0, w: 400.0, h: 200.0 };
const DIMS: (u32, u32) = (400, 200);

fn fig(n: &str) -> InkBase {
    InkBase::Figure { node_id: format!("files:{n}") }
}

fn on() -> InkLayer {
    let mut l = InkLayer::default();
    l.set_on(true);
    l
}

/// The change one edit makes in the revision.
fn rev(l: &mut InkLayer, f: impl FnOnce(&mut InkLayer)) -> u64 {
    let r = l.revision();
    f(l);
    l.revision() - r
}

fn pt(l: &mut InkLayer, b: &InkBase, p: (f32, f32)) {
    l.press(Some(b), p, &R, &R, DIMS);
    assert_eq!(rev(l, |l| l.release()), 1);
}

/// A right press at `x` on `R`.
fn hit(l: &mut InkLayer, b: &InkBase, x: f32) {
    l.erase(Some(b), (x, 100.0), &R, &R);
}

fn xs(l: &InkLayer) -> Vec<u16> {
    l.strokes().iter().map(|s| s[0][0]).collect()
}

#[test]
fn a_point_lands_on_the_base_where_it_was_drawn_at_any_zoom_and_pan() {
    let b = fig("a.png");
    let image = ScreenRect { x: 0.0, y: 0.0, w: 1000.0, h: 800.0 };
    for (zoom, pan) in [(1.0, (0.0, 0.0)), (2.5, (-130.0, 40.0)), (0.5, (300.0, 200.0))] {
        let canvas = ScreenRect { x: 100.0 + pan.0, y: 50.0 + pan.1, w: 400.0 * zoom, h: 200.0 * zoom };
        for f in [(0.0f32, 0.0f32), (0.25, 0.75), (1.0, 1.0), (0.3333, 0.1)] {
            let p = (canvas.x + f.0 * canvas.w, canvas.y + f.1 * canvas.h);
            if !inside(&image, p) {
                continue;
            }
            let mut l = on();
            l.press(Some(&b), p, &canvas, &image, DIMS);
            let want = [(f.0 * 65535.0).round() as u16, (f.1 * 65535.0).round() as u16];
            assert_eq!(l.live(), Some(&[want][..]), "zoom {zoom} at {f:?}");
        }
    }
    // outside the image rectangle: nothing is recorded
    let (mut l, small) = (on(), ScreenRect { x: 200.0, y: 100.0, w: 100.0, h: 50.0 });
    l.press(Some(&b), (150.0, 120.0), &R, &small, DIMS);
    assert!(l.live().is_none());
    // inside the image on x only, or inside the image but off the canvas: nothing is recorded
    l.press(Some(&b), (300.0, 10.0), &R, &R, DIMS);
    l.press(Some(&b), (300.0, 150.0), &ScreenRect { x: 0.0, y: 0.0, w: 50.0, h: 50.0 }, &R, DIMS);
    assert!(l.live().is_none());
    // a canvas of no size starts no stroke and replaces nothing
    let mut z = on();
    pt(&mut z, &b, (200.0, 100.0));
    assert_eq!(z.press(Some(&fig("z.png")), (200.0, 100.0), &ScreenRect { x: 200.0, y: 100.0, w: 0.0, h: 0.0 }, &R, DIMS), None);
    assert!(z.live().is_none() && z.base() == Some(&b) && z.strokes().len() == 1);
    // a drag past the edge clamps to it
    l.press(Some(&b), (300.0, 150.0), &R, &R, DIMS);
    l.drag((9000.0, -9000.0), &R);
    assert_eq!(l.live().unwrap().last(), Some(&[65535, 0]));
}

#[test]
fn a_stroke_ends_at_its_release_a_new_press_or_a_lost_focus_and_is_dropped_with_its_base() {
    let b = fig("a.png");
    // 2: no stroke when drawing is off, nothing is shown, or the press is outside the image
    let mut l = InkLayer::default();
    l.press(Some(&b), (200.0, 100.0), &R, &R, DIMS);
    l.set_on(true);
    l.press(None, (200.0, 100.0), &R, &R, DIMS);
    l.press(Some(&b), (0.0, 0.0), &R, &R, DIMS);
    assert!(l.live().is_none() && l.revision() == 0);
    // the canvas draws with drawing off
    let mut c = InkLayer::default();
    c.press(Some(&InkBase::Canvas), (200.0, 100.0), &R, &R, DIMS);
    assert!(c.live().is_some());
    // 1, 3, 4: press begins, motion extends (spacing: one raster pixel), release commits
    l.press(Some(&b), (200.0, 100.0), &R, &R, DIMS);
    assert_eq!(l.live().unwrap().len(), 1);
    l.drag((200.4, 100.0), &R);
    assert_eq!(l.live().unwrap().len(), 1);
    l.drag((260.0, 100.0), &R);
    assert_eq!(l.live().unwrap().len(), 2);
    assert_eq!(rev(&mut l, |l| l.release()), 1);
    assert!(l.live().is_none() && l.strokes().len() == 1 && l.revision() == 1);
    // 5: a press with a stroke live commits it, then begins a new one
    l.press(Some(&b), (300.0, 100.0), &R, &R, DIMS);
    assert_eq!(rev(&mut l, |l| drop(l.press(Some(&b), (310.0, 110.0), &R, &R, DIMS))), 1);
    assert_eq!(l.strokes().len(), 2);
    assert_eq!(l.live().unwrap().len(), 1);
    // 6: a lost focus commits
    assert_eq!(rev(&mut l, |l| l.release()), 1);
    assert_eq!(l.strokes().len(), 3);
    // 7: a later point maps through the new rectangle, earlier ones stay
    l.press(Some(&b), (200.0, 100.0), &R, &R, DIMS);
    let zoomed = ScreenRect { x: 100.0, y: 50.0, w: 800.0, h: 400.0 };
    l.drag((300.0, 250.0), &zoomed);
    assert_eq!(l.live().unwrap(), &[[16384, 16384], [16384, 32768]][..]);
    // 8: the base stops being shown, the live stroke is dropped
    l.base_hidden();
    assert!(l.live().is_none() && l.strokes().len() == 3);
    // 9: undo or clear during a stroke drops it and does nothing else
    l.press(Some(&b), (200.0, 100.0), &R, &R, DIMS);
    l.undo(Some(&b));
    assert!(l.live().is_none() && l.strokes().len() == 3);
    l.press(Some(&b), (200.0, 100.0), &R, &R, DIMS);
    l.clear(Some(&b));
    assert!(l.live().is_none() && l.strokes().len() == 3);
    // 10: a press on another base replaces the drawing and says so; the name is the node id's last part
    let mut note = None;
    assert_eq!(rev(&mut l, |l| note = l.press(Some(&fig("b.png")), (200.0, 100.0), &R, &R, DIMS)), 1);
    assert_eq!(note.as_deref(), Some("the drawing on a.png was replaced"));
    assert!(l.strokes().is_empty() && l.base() == Some(&fig("b.png")));
    // 11: the same base again keeps the drawing
    l.release();
    assert_eq!(l.press(Some(&fig("b.png")), (210.0, 100.0), &R, &R, DIMS), None);
    l.release();
    assert_eq!(l.strokes().len(), 2);
    let deep = InkBase::Figure { node_id: "files:dir/c.png".into() };
    assert_eq!(l.press(Some(&deep), (200.0, 100.0), &R, &R, DIMS).as_deref(), Some("the drawing on b.png was replaced"));
    l.release();
    assert_eq!(l.press(Some(&fig("b.png")), (200.0, 100.0), &R, &R, DIMS).as_deref(), Some("the drawing on c.png was replaced"));
    // the replaced drawing's history goes with it: undo takes back the new stroke and nothing more
    l.release();
    l.undo(Some(&fig("b.png")));
    assert_eq!(rev(&mut l, |l| l.undo(Some(&fig("b.png")))), 0);
}

#[test]
fn undo_clear_and_restore_keep_their_order_and_a_new_stroke_drops_the_cleared_copy() {
    let (b, mut l) = (fig("a.png"), on());
    for x in [150.0, 250.0, 350.0] {
        pt(&mut l, &b, (x, 100.0));
    }
    assert_eq!(rev(&mut l, |l| l.undo(Some(&b))), 1);
    assert_eq!(l.strokes().len(), 2);
    assert_eq!(rev(&mut l, |l| l.clear(Some(&b))), 1);
    assert!(l.strokes().is_empty());
    assert_eq!(rev(&mut l, |l| l.undo(Some(&b))), 1);
    assert_eq!(l.strokes().len(), 2);
    assert_eq!(rev(&mut l, |l| l.undo(Some(&b))), 1);
    assert_eq!(l.strokes().len(), 1);
    l.clear(Some(&b));
    pt(&mut l, &b, (400.0, 100.0));
    l.undo(Some(&b));
    l.undo(Some(&b));
    assert!(l.strokes().is_empty(), "the new stroke dropped the cleared copy");
    let r = l.revision();
    l.undo(Some(&b));
    l.clear(Some(&b));
    assert_eq!(l.revision(), r, "nothing to change");
}

#[test]
fn the_cleared_copy_goes_with_a_commit_or_a_base_change_not_a_press() {
    let (b, mut l) = (fig("a.png"), on());
    pt(&mut l, &b, (150.0, 100.0));
    l.clear(Some(&b));
    l.press(Some(&b), (200.0, 100.0), &R, &R, DIMS);
    l.base_hidden();
    l.undo(Some(&b));
    assert_eq!(xs(&l).len(), 1, "a stroke dropped before its commit leaves the cleared copy");
    l.clear(Some(&b));
    l.press(Some(&b), (200.0, 100.0), &R, &R, DIMS);
    l.undo(Some(&b));
    l.undo(Some(&b));
    assert_eq!(xs(&l).len(), 1);
    l.clear(Some(&b));
    l.press(Some(&fig("b.png")), (200.0, 100.0), &R, &R, DIMS);
    l.base_hidden();
    assert_eq!(rev(&mut l, |l| l.undo(Some(&fig("b.png")))), 0);
    assert!(l.strokes().is_empty(), "the old base's strokes never come back on the new one");
}

#[test]
fn the_right_button_erases_in_one_undo_step_and_never_a_live_stroke() {
    let (b, mut l) = (fig("a.png"), on());
    for x in [150.0, 250.0, 350.0] {
        pt(&mut l, &b, (x, 100.0));
    }
    assert_eq!(rev(&mut l, |l| l.erase(Some(&b), (150.0, 160.0), &R, &R)), 0);
    l.erase_end();
    assert_eq!(l.strokes().len(), 3, "a press with no stroke under it changes nothing");
    l.undo(Some(&b));
    assert_eq!(l.strokes().len(), 2, "an erase that removed nothing is no undo step");
    pt(&mut l, &b, (350.0, 100.0));
    let r = l.revision();
    hit(&mut l, &b, 150.0);
    hit(&mut l, &b, 250.0);
    l.erase_end();
    assert!(l.strokes().len() == 1 && l.revision() == r + 2, "one raise per erase call that removes");
    assert_eq!(rev(&mut l, |l| l.erase(Some(&b), (150.0, 100.0), &R, &R)), 0);
    l.erase_end();
    // erase, a new stroke, undo: the new stroke goes first
    pt(&mut l, &b, (300.0, 200.0));
    l.undo(Some(&b));
    assert_eq!(l.strokes().len(), 1);
    assert_eq!(rev(&mut l, |l| l.undo(Some(&b))), 1);
    assert_eq!(l.strokes().len(), 3, "one undo put the erase drag's strokes back");
    // a right press during a left drag is ignored, as is one on another base
    l.press(Some(&b), (200.0, 200.0), &R, &R, DIMS);
    hit(&mut l, &b, 350.0);
    l.erase_end();
    assert_eq!(l.strokes().len(), 3);
    l.release();
    l.erase(Some(&fig("other.png")), (350.0, 100.0), &R, &R);
    assert_eq!(l.strokes().len(), 4);
    // erasing every stroke empties the drawing as a clear does
    for x in [150.0, 200.0, 250.0, 350.0] {
        l.erase(Some(&b), (x, if x == 200.0 { 200.0 } else { 100.0 }), &R, &R);
    }
    l.erase_end();
    assert!(l.strokes().is_empty());
    l.undo(Some(&b));
    assert_eq!(l.strokes().len(), 4);
}

#[test]
fn no_event_between_an_erase_hit_and_the_drag_end_splits_the_strokes_from_the_history() {
    let (b, mut l) = (fig("a.png"), on());
    let two = |l: &mut InkLayer| {
        *l = on();
        pt(l, &b, (150.0, 100.0));
        pt(l, &b, (350.0, 100.0));
        hit(l, &b, 350.0);
    };
    // undo takes the erase back; the drag goes on and hits again as a step of its own
    two(&mut l);
    l.undo(Some(&b));
    hit(&mut l, &b, 350.0);
    l.erase_end();
    l.undo(Some(&b));
    assert_eq!(xs(&l), [8192, 40959]);
    // clear, then undo brings the clear back; the drag goes on and hits again
    two(&mut l);
    l.clear(Some(&b));
    l.undo(Some(&b));
    hit(&mut l, &b, 150.0);
    l.erase_end();
    l.undo(Some(&b));
    assert_eq!(xs(&l), [8192]);
    // release or a hidden base ends the step: a later hit is another step
    for end in [InkLayer::release, InkLayer::base_hidden] {
        two(&mut l);
        end(&mut l);
        hit(&mut l, &b, 150.0);
        l.erase_end();
        l.undo(Some(&b));
        assert_eq!(xs(&l), [8192], "the undo took back the second step only");
    }
    // a press (a new stroke between two hits of one right drag) ends it too
    two(&mut l);
    l.press(Some(&b), (300.0, 200.0), &R, &R, DIMS);
    l.release();
    hit(&mut l, &b, 150.0);
    l.erase_end();
    l.undo(Some(&b));
    assert_eq!(xs(&l), [8192, 32768]);
    l.undo(Some(&b));
    l.undo(Some(&b));
    assert_eq!(xs(&l), [8192, 40959]);
}

#[test]
fn the_eraser_reaches_the_painted_ink_along_the_path_and_only_inside_the_image() {
    let (b, mut l) = (fig("a.png"), on());
    // a stroke of two points on a raster five times finer than the screen: the ink is 5 raster px, the reach 6, 1.2 screen px
    let fine = (2000, 1000);
    let line = |l: &mut InkLayer| {
        *l = on();
        l.press(Some(&b), (150.0, 100.0), &R, &R, fine);
        l.drag((350.0, 100.0), &R);
        l.release();
    };
    for (dy, gone) in [(1.1, true), (1.3, false), (-1.1, true), (-1.3, false)] {
        line(&mut l);
        l.erase(Some(&b), (250.0, 100.0 + dy), &R, &R);
        assert_eq!(l.strokes().is_empty(), gone, "{dy} screen px off the line");
    }
    // past an end of a segment the distance is to the end, not the line
    line(&mut l);
    l.erase(Some(&b), (360.0, 100.0), &R, &R);
    assert_eq!(l.strokes().len(), 1);
    l.erase(Some(&b), (351.0, 100.0), &R, &R);
    assert!(l.strokes().is_empty());
    // two motion samples either side of a stroke: the path between them crosses it
    let mut v = on();
    v.press(Some(&b), (300.0, 60.0), &R, &R, DIMS);
    v.drag((300.0, 240.0), &R);
    v.release();
    v.erase(Some(&b), (250.0, 150.0), &R, &R);
    assert_eq!(v.strokes().len(), 1);
    v.erase(Some(&b), (350.0, 150.0), &R, &R);
    assert!(v.strokes().is_empty());
    // only points inside the image erase, however the drag began or was lost
    let small = ScreenRect { x: 100.0, y: 50.0, w: 100.0, h: 150.0 };
    let mut w = on();
    pt(&mut w, &b, (350.0, 100.0));
    w.erase(Some(&b), (150.0, 100.0), &R, &small);
    w.erase(Some(&b), (350.0, 100.0), &R, &small);
    assert_eq!(w.strokes().len(), 1, "a drag that left the image erases nothing outside it");
    w.release();
    w.erase(Some(&b), (350.0, 100.0), &R, &small);
    assert_eq!(w.strokes().len(), 1, "a lost release ended the drag");
}

#[test]
fn the_drawing_is_bounded_in_points() {
    let (b, mut l) = (fig("a.png"), on());
    assert!(l.is_on());
    l.press(Some(&b), (100.0, 100.0), &R, &R, DIMS);
    let mut said = None;
    for i in 0..MAX_INK_POINTS + 10 {
        said = l.drag((if i % 2 == 0 { 300.0 } else { 100.0 }, 100.0), &R).or(said);
    }
    assert_eq!(l.live().unwrap().len(), MAX_INK_POINTS);
    assert_eq!(said, Some("the drawing is full: undo or clear"));
    l.release();
    let note = l.press(Some(&b), (200.0, 100.0), &R, &R, DIMS);
    assert!(l.live().is_none(), "a press on a full drawing starts no stroke");
    assert_eq!(note.as_deref(), Some("the drawing is full: undo or clear"));
    assert_eq!(l.strokes().iter().map(Vec::len).sum::<usize>(), MAX_INK_POINTS);
}

fn vertical(l: &mut InkLayer, b: &InkBase) {
    l.press(Some(b), (300.0, 100.0), &R, &R, DIMS);
    l.drag((300.0, 200.0), &R);
    l.release();
    assert_eq!(l.strokes().len(), 1);
}

#[test]
fn a_drag_that_leaves_the_image_and_comes_back_joins_nothing_across_the_stretch_outside() {
    let (b, mut l) = (fig("a.png"), on());
    vertical(&mut l, &b);
    for p in [(150.0, 60.0), (150.0, 40.0), (90.0, 40.0), (90.0, 260.0), (450.0, 260.0), (450.0, 240.0)] {
        l.erase(Some(&b), p, &R, &R);
    }
    l.erase_end();
    assert_eq!(l.strokes().len(), 1, "the chord from the exit to the re-entry erased the stroke");
    // drawing turned off and on during the drag breaks the path the same way
    l.erase(Some(&b), (150.0, 60.0), &R, &R);
    l.set_on(false);
    l.erase(Some(&b), (150.0, 240.0), &R, &R);
    l.set_on(true);
    l.erase(Some(&b), (450.0, 240.0), &R, &R);
    l.erase_end();
    assert_eq!(l.strokes().len(), 1, "the chord across the stretch drawing was off erased the stroke");
}

#[test]
fn a_later_segment_of_a_stroke_erases_it() {
    let (b, mut l) = (fig("a.png"), on());
    l.press(Some(&b), (150.0, 100.0), &R, &R, DIMS);
    l.drag((250.0, 100.0), &R);
    l.drag((250.0, 200.0), &R);
    l.release();
    l.erase(Some(&b), (251.0, 180.0), &R, &R);
    assert!(l.strokes().is_empty());
}

#[test]
fn a_sweep_beside_a_dot_erases_it() {
    let (b, mut l) = (fig("a.png"), on());
    l.press(Some(&b), (250.0, 101.0), &R, &R, DIMS);
    l.release();
    l.erase(Some(&b), (150.0, 100.0), &R, &R);
    l.erase(Some(&b), (350.0, 100.0), &R, &R);
    assert!(l.strokes().is_empty());
}

#[test]
fn the_path_follows_each_sample_not_the_drags_first_point() {
    let (b, mut l) = (fig("a.png"), on());
    vertical(&mut l, &b);
    for p in [(150.0, 60.0), (450.0, 60.0), (450.0, 240.0)] {
        l.erase(Some(&b), p, &R, &R);
    }
    assert_eq!(l.strokes().len(), 1);
}

#[test]
fn a_miss_between_two_hits_keeps_one_undo_step() {
    let (b, mut l) = (fig("a.png"), on());
    for x in [150.0, 450.0] {
        pt(&mut l, &b, (x, 100.0));
    }
    for p in [(150.0, 100.0), (150.0, 200.0), (450.0, 200.0), (450.0, 100.0)] {
        l.erase(Some(&b), p, &R, &R);
    }
    l.erase_end();
    assert!(l.strokes().is_empty());
    l.undo(Some(&b));
    assert_eq!(l.strokes().len(), 2);
}

#[test]
fn erase_end_ends_the_step_and_the_path() {
    let (b, mut l) = (fig("a.png"), on());
    vertical(&mut l, &b);
    pt(&mut l, &b, (150.0, 220.0));
    l.erase(Some(&b), (150.0, 220.0), &R, &R);
    l.erase_end();
    l.erase(Some(&b), (450.0, 80.0), &R, &R);
    l.erase_end();
    assert_eq!(l.strokes().len(), 1, "the second drag's press joined the first drag's last point");
    pt(&mut l, &b, (200.0, 100.0));
    l.erase(Some(&b), (200.0, 100.0), &R, &R);
    l.erase_end();
    l.undo(Some(&b));
    assert_eq!(l.strokes().len(), 2, "one undo took back the last drag only");
}

#[test]
fn the_reach_is_the_ink_and_one_pixel_on_both_sides() {
    let b = fig("a.png");
    for (dy, gone) in [(1.1, true), (1.25, false)] {
        let mut l = on();
        l.press(Some(&b), (150.0, 100.0), &R, &R, (2000, 1000));
        l.drag((350.0, 100.0), &R);
        l.release();
        l.erase(Some(&b), (250.0, 100.0 + dy), &R, &R);
        assert_eq!(l.strokes().is_empty(), gone, "{dy}");
    }
}

#[test]
fn a_point_one_and_a_half_raster_pixels_on_is_kept() {
    let (b, mut l) = (fig("a.png"), on());
    l.press(Some(&b), (200.0, 100.0), &R, &R, DIMS);
    l.drag((201.5, 100.0), &R);
    assert_eq!(l.live().unwrap().len(), 2);
}

#[test]
fn undo_and_clear_change_nothing_while_the_drawings_base_is_not_shown() {
    let (b, mut l) = (fig("a.png"), on());
    for x in [150.0, 250.0] {
        pt(&mut l, &b, (x, 100.0));
    }
    l.base_hidden();
    let other = fig("b.png");
    for shown in [Some(&other), None] {
        assert_eq!(rev(&mut l, |l| l.undo(shown)), 0);
        assert_eq!(rev(&mut l, |l| l.clear(shown)), 0);
        assert_eq!(l.strokes().len(), 2);
    }
    assert_eq!(rev(&mut l, |l| l.undo(Some(&b))), 1);
    assert_eq!(l.strokes().len(), 1);
}

/// Whether every pixel that differs between `before` and `after` lies in the dirty rectangle.
fn covered(before: &[u8], after: &[u8], w: u32, dirty: Option<(u32, u32, u32, u32)>) -> bool {
    (0..after.len()).filter(|&i| before[i] != after[i]).all(|i| {
        let (x, y) = (i as u32 % w, i as u32 / w);
        dirty.is_some_and(|(dx, dy, dw, dh)| x >= dx && x < dx + dw && y >= dy && y < dy + dh)
    })
}

#[test]
fn the_overlay_is_the_drawings_raster_after_every_edit() {
    let mut cur = fig("a.png");
    let mut src = (800, 300);
    let mut l = on();
    let mut seed = 7u32;
    let mut next = move |n: u32| {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        (seed >> 8) % n
    };
    l.press(Some(&cur), (300.0, 100.0), &R, &R, src);
    l.release();
    let (mut hits, mut live_drops, mut base_changes, mut size_changes) = (0, 0, 0, 0);
    for step in 0..120 {
        let (x, y) = (100.0 + next(401) as f32, 50.0 + next(201) as f32);
        let (was, was_dims, was_strokes) = (l.mask.clone(), l.dims, l.strokes().len());
        l.dirty = None;
        let stroke = |l: &mut InkLayer, cur: &InkBase, src, next: &mut dyn FnMut(u32) -> u32| {
            l.press(Some(cur), (x, y), &R, &R, src);
            for _ in 0..next(5) {
                l.drag((80.0 + next(441) as f32, 30.0 + next(241) as f32), &R);
            }
        };
        match next(9) {
            0..=2 => {
                stroke(&mut l, &cur, src, &mut next);
                // a live stroke is in the mask already: the wire's mask of the strokes and the live one
                let live = l.live().map(|s| s.to_vec()).into_iter();
                let all: Vec<_> = l.strokes().iter().cloned().chain(live).collect();
                assert_eq!(l.mask, sot_protocol::ink::mask(&all, raster_dims(src.0, src.1)), "live, step {step}");
                l.release();
            }
            3 | 4 => l.undo(Some(&cur)),
            5 => {
                if next(2) == 0 {
                    l.clear(Some(&cur));
                } else if let Some(p) = l.strokes().first().and_then(|s| s.first().copied()) {
                    // right on a committed stroke's own point
                    l.erase(Some(&cur), (R.x + p[0] as f32 / 65535.0 * R.w, R.y + p[1] as f32 / 65535.0 * R.h), &R, &R);
                    l.erase_end();
                    hits += (l.strokes().len() < was_strokes) as u32;
                }
            }
            6 => {
                stroke(&mut l, &cur, src, &mut next);
                assert!(l.live().is_some());
                match next(3) {
                    0 => l.base_hidden(),
                    1 => l.undo(Some(&cur)),
                    _ => l.clear(Some(&cur)),
                }
                live_drops += 1;
            }
            7 => {
                // another base at the same raster size
                cur = fig(if next(2) == 0 { "b.png" } else { "a.png" });
                l.press(Some(&cur), (x, y), &R, &R, src);
                l.release();
                base_changes += 1;
            }
            _ => {
                // the same base at another size
                src = if src == (800, 300) { (600, 360) } else { (800, 300) };
                l.press(Some(&cur), (x, y), &R, &R, src);
                l.release();
                size_changes += 1;
            }
        }
        let dims = raster_dims(src.0, src.1);
        assert_eq!(l.dims, dims);
        if let Some((dx, dy, dw, dh)) = l.dirty() {
            assert!(dw > 0 && dh > 0 && dx + dw <= dims.0 && dy + dh <= dims.1, "dirty inside the raster, step {step}");
        }
        assert_eq!(l.mask, sot_protocol::ink::mask(l.strokes(), dims), "step {step}");
        if was_dims == dims && !was.is_empty() {
            assert!(covered(&was, &l.mask, dims.0, l.dirty()), "dirty rectangle, step {step}");
        }
        if let Some((dx, dy, dw, dh)) = l.dirty() {
            // the bytes the texture gets: the mask's rectangle in the ink color
            let want: Vec<u8> = (dy..dy + dh)
                .flat_map(|r| (dx..dx + dw).map(move |c| (r * dims.0 + c) as usize))
                .flat_map(|i| [INK_RGB[0], INK_RGB[1], INK_RGB[2], l.mask[i]])
                .collect();
            assert_eq!(l.rgba((dx, dy, dw, dh)), want, "uploaded bytes, step {step}");
            if was_dims != dims {
                assert_eq!((dx, dy, dw, dh), (0, 0, dims.0, dims.1), "a new size is all dirty, step {step}");
            }
        }
    }
    assert!(hits > 0 && live_drops > 0 && base_changes > 0 && size_changes > 0, "{hits} {live_drops} {base_changes} {size_changes}");
    // a large source: the raster is the served size, all of it dirty, and the mask is the wire's
    l.press(Some(&cur), (300.0, 100.0), &R, &R, (4000, 2000));
    l.release();
    assert_eq!(l.dims, raster_dims(4000, 2000));
    assert_eq!(l.dirty(), Some((0, 0, l.dims.0, l.dims.1)));
    assert_eq!(l.mask, sot_protocol::ink::mask(l.strokes(), l.dims));
}

#[test]
fn the_shown_base_is_the_raster_node_only_for_an_image_outside_edit_and_the_refused_overlay() {
    let shown = |node, editing, refused| InkLayer::shown_base(node, editing, refused);
    assert_eq!(shown(Some("files:a.png"), false, false), Some(fig("a.png")));
    // a hidden raster has no node; PDF pages and plugin previews are no base
    assert_eq!(shown(None, false, false), None);
    assert_eq!(shown(Some("files:doc.pdf"), false, false), None);
    assert_eq!(shown(Some("files:src/lib.jl"), false, false), None);
    assert_eq!(shown(Some("files:a.png"), true, false), None);
    assert_eq!(shown(Some("files:a.png"), false, true), None);
}

#[test]
fn a_base_no_longer_shown_drops_the_live_stroke_and_the_same_base_stays() {
    let (mut l, b) = (on(), fig("a.png"));
    pt(&mut l, &b, (150.0, 100.0));
    l.press(Some(&b), (200.0, 100.0), &R, &R, DIMS);
    l.sync(Some(&b), Some(DIMS));
    assert!(l.live().is_some(), "the same base shown keeps the live stroke");
    for other in [None, Some(fig("b.png"))] {
        l.press(Some(&b), (200.0, 100.0), &R, &R, DIMS);
        assert!(l.live().is_some());
        let committed = l.strokes().len();
        l.sync(other.as_ref(), Some(DIMS));
        assert!(l.live().is_none(), "the base stopped being shown: the stroke is dropped, not committed");
        assert_eq!(l.strokes().len(), committed);
    }
}

#[test]
fn the_same_node_at_another_size_rebuilds_the_raster_at_once() {
    let (mut l, b) = (on(), fig("a.png"));
    pt(&mut l, &b, (150.0, 100.0));
    let _ = l.dirty.take();
    l.sync(Some(&b), Some(DIMS));
    assert!(l.dirty.is_none(), "the same size repaints nothing");
    l.sync(Some(&b), Some((800, 400)));
    assert_eq!(l.dims, raster_dims(800, 400));
    assert_eq!(l.dirty(), Some((0, 0, l.dims.0, l.dims.1)));
    assert_eq!(l.mask.len(), (l.dims.0 * l.dims.1) as usize);
    assert_eq!(l.mask, mask(l.strokes(), l.dims));
}

#[test]
fn a_size_change_keeps_the_live_stroke_and_paints_it() {
    let (mut l, b) = (on(), fig("a.png"));
    pt(&mut l, &b, (150.0, 100.0));
    l.press(Some(&b), (200.0, 100.0), &R, &R, DIMS);
    l.drag((400.0, 150.0), &R);
    let live = l.live().unwrap().to_vec();
    assert!(live.len() > 1);
    l.sync(Some(&b), Some((800, 400)));
    assert_eq!(l.live(), Some(&live[..]));
    let all = [l.strokes().to_vec(), vec![live]].concat();
    assert_eq!(l.mask, mask(&all, l.dims));
}

#[test]
fn the_window_routes_a_left_press_motion_and_the_right_button_through_the_layer() {
    let (mut l, b) = (on(), fig("a.png"));
    let inside = (200.0, 100.0);
    // no frame, no source size, drawing off or a point outside: the press is not taken
    assert_eq!(l.left_press(Some(&b), inside, Some(DIMS)), None);
    l.set_frame(R, R);
    assert_eq!(l.left_press(Some(&b), inside, None), None);
    assert_eq!(l.left_press(Some(&b), (10.0, 10.0), Some(DIMS)), None);
    assert!(l.live().is_none() && !l.busy());
    l.set_on(false);
    assert_eq!(l.left_press(Some(&b), inside, Some(DIMS)), None);
    l.set_on(true);
    assert_eq!(l.left_press(None, inside, Some(DIMS)), None);
    // taken: a stroke begins, and a replaced drawing's note comes back
    assert_eq!(l.left_press(Some(&b), inside, Some(DIMS)), Some(None));
    assert!(l.busy());
    // motion extends the live stroke, and the eraser is not touched
    l.right(true, Some(&b), (10.0, 10.0));
    l.moved(Some(&b), (300.0, 100.0));
    assert_eq!(l.live().unwrap().len(), 2);
    l.release();
    l.right(false, Some(&b), (10.0, 10.0));
    assert!(!l.busy());
    assert_eq!(l.left_press(Some(&fig("b.png")), inside, Some(DIMS)), Some(Some("the drawing on a.png was replaced".to_string())));
    l.release();
    // a right press erases at once, its drag erases, its release ends the drag and motion then erases nothing
    let (mut l, b) = (on(), fig("a.png"));
    l.set_frame(R, R);
    pt(&mut l, &b, (150.0, 100.0));
    pt(&mut l, &b, (250.0, 100.0));
    pt(&mut l, &b, (450.0, 100.0));
    l.right(true, Some(&b), (150.0, 100.0));
    assert!(l.busy() && l.strokes().len() == 2);
    l.moved(Some(&b), (250.0, 100.0));
    assert_eq!(l.strokes().len(), 1);
    l.right(false, Some(&b), (250.0, 100.0));
    l.moved(Some(&b), (450.0, 100.0));
    assert_eq!(l.strokes().len(), 1, "no button held: motion erases nothing");
    // a lost focus commits a live stroke and ends a held erase
    l.right(true, Some(&b), (10.0, 10.0));
    l.left_press(Some(&b), (300.0, 150.0), Some(DIMS));
    l.focus_lost();
    assert!(!l.busy() && l.strokes().len() == 2);
}
