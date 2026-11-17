use super::scissor_px;
use crate::ui::render::quad::ScreenRect;

fn rect(x: f32, y: f32, w: f32, h: f32) -> ScreenRect {
    ScreenRect { x, y, w, h }
}

#[test]
fn a_scissor_rounds_every_edge_and_is_cut_to_the_surface() {
    let s = (640, 480);
    let cases = [
        // Whole and half pixels: the edges as they are, the half rounded up.
        (rect(10.0, 20.0, 100.0, 50.0), Some((10, 20, 100, 50))),
        (rect(10.5, 20.5, 100.0, 50.0), Some((11, 21, 100, 50))),
        // The body edges at the fractional text scales a window meets (0.8, 0.9 with float noise, 1.1): the top rounds to the
        // pixel its fills start on, never the one above, and the foot to the one it ends on.
        (rect(16.3, 268.8, 604.8, 115.2), Some((16, 269, 605, 115))),
        (rect(16.3, 269.9999, 604.8, 100.0), Some((16, 270, 605, 100))),
        (rect(16.3, 270.6, 604.8, 100.4), Some((16, 271, 605, 100))),
        (rect(0.0, 0.0, 639.6, 479.6), Some((0, 0, 640, 480))),
        // Negative origins, and rectangles past the surface, are cut to it.
        (rect(-30.0, -10.4, 100.0, 50.0), Some((0, 0, 70, 40))),
        (rect(600.0, 450.0, 100.0, 100.0), Some((600, 450, 40, 30))),
        (rect(-50.0, -50.0, 2000.0, 2000.0), Some((0, 0, 640, 480))),
        // Nothing left: empty, wholly off the surface, or less than a pixel wide.
        (rect(10.0, 10.0, 0.0, 50.0), None),
        (rect(10.0, 10.0, 50.0, 0.0), None),
        (rect(10.6, 10.0, 0.3, 50.0), None),
        (rect(700.0, 10.0, 50.0, 50.0), None),
        (rect(-100.0, 10.0, 50.0, 50.0), None),
        (rect(f32::NAN, 0.0, 10.0, 10.0), None),
    ];
    for (r, want) in cases {
        assert_eq!(scissor_px(r, s), want, "{r:?}");
    }
}
