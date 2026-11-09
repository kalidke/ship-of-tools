use super::*;

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

fn pt(l: &mut InkLayer, b: &InkBase, p: (f32, f32)) {
    l.press(Some(b), p, &R, &R);
    l.release();
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
            l.press(Some(&b), p, &canvas, &image);
            let want = [(f.0 * 65535.0).round() as u16, (f.1 * 65535.0).round() as u16];
            assert_eq!(l.live(), Some(&[want][..]), "zoom {zoom} at {f:?}");
        }
    }
    // outside the image rectangle: nothing is recorded
    let (mut l, small) = (on(), ScreenRect { x: 200.0, y: 100.0, w: 100.0, h: 50.0 });
    l.press(Some(&b), (150.0, 120.0), &R, &small);
    assert!(l.live().is_none());
    // a drag past the edge clamps to it
    l.press(Some(&b), (300.0, 150.0), &R, &R);
    l.drag((9000.0, -9000.0), &R, DIMS);
    assert_eq!(l.live().unwrap().last(), Some(&[65535, 0]));
}

#[test]
fn a_stroke_ends_at_its_release_a_new_press_or_a_lost_focus_and_is_dropped_with_its_base() {
    let b = fig("a.png");
    // 2: no stroke when drawing is off, nothing is shown, or the press is outside the image
    let mut l = InkLayer::default();
    l.press(Some(&b), (200.0, 100.0), &R, &R);
    l.set_on(true);
    l.press(None, (200.0, 100.0), &R, &R);
    l.press(Some(&b), (0.0, 0.0), &R, &R);
    assert!(l.live().is_none() && l.revision() == 0);
    // the canvas draws with drawing off
    InkLayer::default().press(Some(&InkBase::Canvas), (200.0, 100.0), &R, &R);
    // 1, 3, 4: press begins, motion extends (spacing: one raster pixel), release commits
    l.press(Some(&b), (200.0, 100.0), &R, &R);
    assert_eq!(l.live().unwrap().len(), 1);
    l.drag((200.4, 100.0), &R, DIMS);
    assert_eq!(l.live().unwrap().len(), 1);
    l.drag((260.0, 100.0), &R, DIMS);
    assert_eq!(l.live().unwrap().len(), 2);
    l.release();
    assert!(l.live().is_none() && l.strokes().len() == 1 && l.revision() == 1);
    // 5: a press with a stroke live commits it, then begins a new one
    l.press(Some(&b), (300.0, 100.0), &R, &R);
    l.press(Some(&b), (310.0, 110.0), &R, &R);
    assert_eq!(l.strokes().len(), 2);
    assert_eq!(l.live().unwrap().len(), 1);
    // 6: a lost focus commits
    l.release();
    assert_eq!(l.strokes().len(), 3);
    // 7: a later point maps through the new rectangle, earlier ones stay
    l.press(Some(&b), (200.0, 100.0), &R, &R);
    let zoomed = ScreenRect { x: 100.0, y: 50.0, w: 800.0, h: 400.0 };
    l.drag((300.0, 250.0), &zoomed, DIMS);
    assert_eq!(l.live().unwrap(), &[[16384, 16384], [16384, 32768]][..]);
    // 8: the base stops being shown, the live stroke is dropped
    l.base_hidden();
    assert!(l.live().is_none() && l.strokes().len() == 3);
    // 9: undo or clear during a stroke drops it and does nothing else
    l.press(Some(&b), (200.0, 100.0), &R, &R);
    l.undo();
    assert!(l.live().is_none() && l.strokes().len() == 3);
    l.press(Some(&b), (200.0, 100.0), &R, &R);
    l.clear();
    assert!(l.live().is_none() && l.strokes().len() == 3);
    // 10: a press on another base replaces the drawing and says so
    let note = l.press(Some(&fig("b.png")), (200.0, 100.0), &R, &R);
    assert_eq!(note.as_deref(), Some("the drawing on a.png was replaced"));
    assert!(l.strokes().is_empty() && l.base() == Some(&fig("b.png")));
    // 11: the same base again keeps the drawing
    l.release();
    assert_eq!(l.press(Some(&fig("b.png")), (210.0, 100.0), &R, &R), None);
    l.release();
    assert_eq!(l.strokes().len(), 2);
}

#[test]
fn undo_clear_and_restore_keep_their_order_and_a_new_stroke_drops_the_cleared_copy() {
    let (b, mut l) = (fig("a.png"), on());
    for x in [150.0, 250.0, 350.0] {
        pt(&mut l, &b, (x, 100.0));
    }
    l.undo();
    assert_eq!(l.strokes().len(), 2);
    l.clear();
    assert!(l.strokes().is_empty());
    l.undo();
    assert_eq!(l.strokes().len(), 2);
    l.undo();
    assert_eq!(l.strokes().len(), 1);
    l.clear();
    pt(&mut l, &b, (400.0, 100.0));
    l.undo();
    l.undo();
    assert!(l.strokes().is_empty(), "the new stroke dropped the cleared copy");
    let r = l.revision();
    l.undo();
    l.clear();
    assert_eq!(l.revision(), r, "nothing to change");
}

#[test]
fn the_right_button_erases_in_one_undo_step_and_never_a_live_stroke() {
    let (b, mut l) = (fig("a.png"), on());
    for x in [150.0, 250.0, 350.0] {
        pt(&mut l, &b, (x, 100.0));
    }
    l.erase(Some(&b), (150.0, 160.0), &R, &R, DIMS);
    l.erase_end();
    assert_eq!(l.strokes().len(), 3, "a press with no stroke under it changes nothing");
    let r = l.revision();
    l.erase(Some(&b), (150.0, 100.0), &R, &R, DIMS);
    l.erase(Some(&b), (250.0, 100.0), &R, &R, DIMS);
    l.erase_end();
    assert!(l.strokes().len() == 1 && l.revision() > r);
    // erase, a new stroke, undo: the new stroke goes first
    pt(&mut l, &b, (300.0, 200.0));
    l.undo();
    assert_eq!(l.strokes().len(), 1);
    l.undo();
    assert_eq!(l.strokes().len(), 3, "one undo put the erase drag's strokes back");
    // a right press during a left drag is ignored
    l.press(Some(&b), (200.0, 200.0), &R, &R);
    l.erase(Some(&b), (350.0, 100.0), &R, &R, DIMS);
    l.erase_end();
    assert_eq!(l.strokes().len(), 3);
    l.release();
    // erasing every stroke empties the drawing as a clear does
    for x in [150.0, 200.0, 250.0, 350.0] {
        l.erase(Some(&b), (x, if x == 200.0 { 200.0 } else { 100.0 }), &R, &R, DIMS);
    }
    l.erase_end();
    assert!(l.strokes().is_empty());
    l.undo();
    assert_eq!(l.strokes().len(), 4);
}

#[test]
fn the_drawing_is_bounded_in_points() {
    let (b, mut l) = (fig("a.png"), on());
    assert!(l.is_on());
    l.press(Some(&b), (100.0, 100.0), &R, &R);
    let mut said = None;
    for i in 0..MAX_INK_POINTS + 10 {
        said = l.drag((if i % 2 == 0 { 300.0 } else { 100.0 }, 100.0), &R, DIMS).or(said);
    }
    assert_eq!(l.live().unwrap().len(), MAX_INK_POINTS);
    assert_eq!(said, Some("the drawing is full: undo or clear"));
}
