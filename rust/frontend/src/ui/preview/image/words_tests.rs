use super::*;
use sot_protocol::ops::PageWord;

/// A box side as the wire carries it.
fn at(f: f32) -> u16 {
    (f * 65535.0).round() as u16
}

fn word(l: f32, t: f32, r: f32, b: f32, text: &str) -> PageWord {
    PageWord { left: at(l), top: at(t), right: at(r), bottom: at(b), text: text.into() }
}

/// Two lines of two words on the page of file `a.pdf`: "alpha beta" high and "gamma delta" lower, at index 0 to 3.
fn page() -> PageText {
    PageText::new(
        PageWords {
            page: 2,
            cut: false,
            lines: vec![
                vec![word(0.1, 0.1, 0.3, 0.2, "alpha"), word(0.5, 0.1, 0.7, 0.2, "beta")],
                vec![word(0.1, 0.5, 0.3, 0.6, "gamma"), word(0.5, 0.5, 0.7, 0.6, "delta")],
            ],
        },
    )
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> ScreenRect {
    ScreenRect { x, y, w, h }
}

fn near(r: &ScreenRect, want: (f32, f32, f32, f32)) -> bool {
    [(r.x, want.0), (r.y, want.1), (r.w, want.2), (r.h, want.3)].iter().all(|(a, b)| (a - b).abs() < 0.1)
}

/// The word at a point is the first whose box holds it, else the first nearest by the distance to the box, in units of the
/// page's height; a point past the page's edge has a word too, and a page with no words has none.
#[test]
fn the_word_at_a_point_is_the_one_under_it_else_the_nearest() {
    let p = page();
    assert_eq!(p.word_at((0.2, 0.15), 1.0), Some(0));
    assert_eq!(p.word_at((0.6, 0.55), 1.0), Some(3));
    assert_eq!(p.word_at((0.1, 0.1), 1.0), Some(0), "a corner is on the box");
    // Between two words of a line: the nearer of the two sides.
    assert_eq!(p.word_at((0.38, 0.15), 1.0), Some(0));
    assert_eq!(p.word_at((0.42, 0.15), 1.0), Some(1));
    // Past the page's edges.
    assert_eq!(p.word_at((-3.0, -3.0), 1.0), Some(0));
    assert_eq!(p.word_at((9.0, 9.0), 1.0), Some(3));
    // Between the lines, beside the upper one: the nearer line.
    assert_eq!(p.word_at((0.2, 0.3), 1.0), Some(0));
    assert_eq!(p.word_at((0.2, 0.4), 1.0), Some(2));
    // A wide page counts a sideways step as more distance: from (0.3, 0.45), "side" is 0.1 away sideways and "under" 0.13
    // away downward, so "side" is nearer on a square page and "under" on one twice as wide.
    let two = PageText::new(PageWords { page: 1, cut: false, lines: vec![vec![word(0.4, 0.4, 0.5, 0.5, "side")], vec![word(0.2, 0.58, 0.4, 0.7, "under")]] });
    assert_eq!(two.word_at((0.3, 0.45), 1.0), Some(0));
    assert_eq!(two.word_at((0.3, 0.45), 2.0), Some(1));
    // Overlapping boxes: the first in the document's order.
    let overlap = PageText::new(PageWords { page: 1, cut: false, lines: vec![vec![word(0.1, 0.1, 0.5, 0.2, "a"), word(0.3, 0.1, 0.7, 0.2, "b")]] });
    assert_eq!(overlap.word_at((0.4, 0.15), 1.0), Some(0));
    // The box that holds the point wins over a small one nearer its middle that does not.
    let wide = PageText::new(PageWords { page: 1, cut: false, lines: vec![vec![word(0.5, 0.1, 0.52, 0.2, "small"), word(0.1, 0.1, 0.9, 0.2, "wide")]] });
    assert_eq!(wide.word_at((0.6, 0.15), 1.0), Some(1));
    // No words, and an aspect that is no number.
    let empty = PageText::new(PageWords { page: 1, cut: false, lines: vec![] });
    assert_eq!(empty.word_at((0.5, 0.5), 1.0), None);
    // An aspect that is no number, or no width, is read as 1: the nearest word is the one a square page gives, which a
    // comparison with a NaN distance would not find (the first word would win).
    for odd in [f32::NAN, 0.0, -2.0, f32::INFINITY] {
        assert_eq!(p.word_at((0.2, 0.4), odd), Some(2), "aspect {odd}");
    }
    // A word that holds the point wins by that fact: a tiny aspect squares every sideways gap to nothing, and the first
    // word would win the tie of zeros over the one the point is in.
    assert_eq!(p.word_at((0.6, 0.15), 1e-30), Some(1));
    assert_eq!(p.select((0.6, 0.15), (0.6, 0.55), 1e-30), Some((1, 3)));
}

/// A drag selects every word from the one at its start to the one at its end, in the document's order, whichever way it
/// runs; one word when both ends are in it, and the nearest words when an end is off every box.
#[test]
fn a_drag_selects_the_run_between_its_ends_in_the_documents_order_either_way() {
    let p = page();
    let (a, b) = ((0.6, 0.15), (0.2, 0.55));
    assert_eq!(p.select(a, b, 1.0), Some((1, 2)));
    assert_eq!(p.select(b, a, 1.0), Some((1, 2)), "the same run dragged back");
    assert_eq!(p.select((0.2, 0.15), (0.6, 0.55), 1.0), Some((0, 3)));
    assert_eq!(p.select((0.2, 0.12), (0.25, 0.18), 1.0), Some((0, 0)), "both ends in one word");
    assert_eq!(p.select((0.0, 0.0), (1.0, 1.0), 1.0), Some((0, 3)), "ends off every box");
    let empty = PageText::new(PageWords { page: 1, cut: false, lines: vec![] });
    assert_eq!(empty.select(a, b, 1.0), None);
}

/// The text joins a line's words by one space and the lines by LF, and a range off the page is none.
#[test]
fn the_text_joins_a_line_by_spaces_and_lines_by_lf() {
    let p = page();
    assert_eq!(p.text((0, 3)).as_deref(), Some("alpha beta\ngamma delta"));
    assert_eq!(p.text((1, 2)).as_deref(), Some("beta\ngamma"));
    assert_eq!(p.text((2, 2)).as_deref(), Some("gamma"));
    assert_eq!(p.text((0, 4)), None);
    assert_eq!(p.text((2, 1)), None);
}

/// The bands are one a line, from the first selected word's left to the last's right and as high as the tallest, mapped
/// onto the canvas rectangle and clipped to the image; what the clip leaves nothing of is left out.
#[test]
fn the_bands_are_one_a_line_clipped_to_the_image() {
    let p = page();
    // The page drawn 1000 by 2000 at (100, 50), the whole of it in view.
    let canvas = rect(100.0, 50.0, 1000.0, 2000.0);
    let view = rect(0.0, 0.0, 5000.0, 5000.0);
    let two = p.bands((1, 2), canvas, view);
    assert_eq!(two.len(), 2, "one band a line");
    assert!(near(&two[0], (600.0, 250.0, 200.0, 200.0)), "beta alone on its line: {:?}", two[0]);
    assert!(near(&two[1], (200.0, 1050.0, 200.0, 200.0)), "gamma alone on its line: {:?}", two[1]);
    let all = p.bands((0, 3), canvas, view);
    assert!(near(&all[0], (200.0, 250.0, 600.0, 200.0)) && near(&all[1], (200.0, 1050.0, 600.0, 200.0)), "{all:?}");
    // A line's band is as high as its tallest word.
    let tall = PageText::new(PageWords { page: 1, cut: false, lines: vec![vec![word(0.1, 0.1, 0.2, 0.2, "a"), word(0.3, 0.05, 0.4, 0.3, "b")]] });
    assert!(near(&tall.bands((0, 1), canvas, view)[0], (200.0, 150.0, 300.0, 500.0)));
    // The clip cuts a band to the image rectangle on every side and drops one it leaves nothing of.
    let cut = p.bands((0, 3), canvas, rect(300.0, 300.0, 200.0, 1000.0));
    assert_eq!(cut.len(), 2);
    assert!(near(&cut[0], (300.0, 300.0, 200.0, 150.0)), "{:?}", cut[0]);
    assert!(near(&cut[1], (300.0, 1050.0, 200.0, 200.0)), "{:?}", cut[1]);
    assert_eq!(p.bands((0, 3), canvas, rect(300.0, 500.0, 200.0, 300.0)).len(), 0, "between the lines");
    assert_eq!(p.bands((0, 1), canvas, rect(900.0, 0.0, 100.0, 5000.0)).len(), 0, "right of every word");
    // A line read right to left (its first word at the right) has its band, from its least left to its greatest right.
    let rtl = PageText::new(PageWords { page: 1, cut: false, lines: vec![vec![word(0.7, 0.1, 0.9, 0.2, "b"), word(0.1, 0.1, 0.3, 0.2, "a")]] });
    let band = rtl.bands((0, 1), canvas, view);
    assert_eq!(band.len(), 1);
    assert!(near(&band[0], (200.0, 250.0, 800.0, 200.0)), "{:?}", band[0]);
    // A range off the page marks nothing.
    assert!(p.bands((4, 5), canvas, view).is_empty());
}
