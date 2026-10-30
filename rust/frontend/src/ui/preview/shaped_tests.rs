//! Behavior of selection in the actual flowed preview buffer, without a window or GPU.

use super::*;
use crate::ui::render::text::EXTRA_TOP_PAD_PX;

const TEXT: &str = "alpha beta gamma delta epsilon zeta eta theta\nbravé charlie\nomega";

fn preview(text: &str, width: f32, height: f32) -> (ShapedPreview, glyphon::FontSystem) {
    let mut fs = glyphon::FontSystem::new();
    let md = MarkdownPreview::new_plain(&mut fs, text, width, 1.0);
    (ShapedPreview::new(md, crate::ui::preview::markdown::highlight::HighlightService::new().unwrap(),
        ScreenRect { x: 37.0, y: 61.0, w: width, h: height }), fs)
}

/// A point on the leading quarter of the glyph at this byte index. Positions come from real shaping, so font
/// differences do not turn the test into a pixel fixture. All callers use left-to-right text.
fn at(p: &ShapedPreview, line: usize, byte: usize) -> (f32, f32) {
    for run in p.md().buffer.layout_runs() {
        if run.line_i != line { continue; }
        if let Some(glyph) = run.glyphs.iter().find(|g| g.start == byte) {
            return (p.rect().x + glyph.x + glyph.w * 0.15,
                p.rect().y + EXTRA_TOP_PAD_PX + run.line_top + run.line_height * 0.5
                    - p.scroll() as f32 * p.md().line_height());
        }
    }
    panic!("no shaped glyph at ({line}, {byte})");
}

fn select(p: &mut ShapedPreview) {
    let start = p.hit(at(p, 0, 6), true).unwrap();
    p.press(Some(start));
    assert!(p.extend_drag(at(p, 1, 7)));
    p.release();
}

#[test]
fn preview_selection_spans_wraps_and_buffer_lines_in_both_directions() {
    let (mut p, _fs) = preview(TEXT, 145.0, 400.0);
    assert!(p.md().buffer.layout_runs().filter(|r| r.line_i == 0).count() > 1);
    select(&mut p);
    let expected = "beta gamma delta epsilon zeta eta theta\nbravé ";
    assert_eq!(p.selected_text().as_deref(), Some(expected));
    assert!(!p.extend_drag(at(&p, 2, 0)), "release ends the drag");
    let end = p.hit(at(&p, 1, 7), true).unwrap();
    p.press(Some(end));
    assert!(p.extend_drag(at(&p, 0, 6)));
    p.release();
    assert_eq!(p.selected_text().as_deref(), Some(expected));
}

#[test]
fn preview_click_without_motion_and_outside_press_clear_selection() {
    let (mut p, _fs) = preview(TEXT, 145.0, 400.0);
    select(&mut p);
    let cursor = p.hit(at(&p, 0, 6), true).unwrap();
    p.press(Some(cursor));
    p.release();
    assert!(p.selected_text().is_none());
    assert!(p.selection_rects().is_empty());
    select(&mut p);
    assert!(p.hit((p.rect().x - 1.0, p.rect().y), true).is_none());
    assert!(p.press(None));
    assert!(p.selected_text().is_none());
    assert!(!p.extend_drag(at(&p, 2, 0)));
}

#[test]
fn preview_install_clears_selection_and_drag() {
    let (mut p, mut fs) = preview(TEXT, 145.0, 400.0);
    let start = p.hit(at(&p, 0, 6), true).unwrap();
    p.press(Some(start));
    p.extend_drag(at(&p, 1, 7));
    assert!(p.selected_text().is_some());
    let md = MarkdownPreview::new_plain(&mut fs, "replacement", 145.0, 1.0);
    p.install(md);
    assert!(p.selected_text().is_none());
    assert!(p.selection_rects().is_empty());
    assert!(!p.extend_drag((40.0, 70.0)));
}

#[test]
fn preview_clear_selection_ends_the_range_and_active_drag() {
    let (mut p, _fs) = preview(TEXT, 145.0, 400.0);
    let start = p.hit(at(&p, 0, 6), true).unwrap();
    p.press(Some(start));
    p.extend_drag(at(&p, 1, 7));
    assert!(p.selected_text().is_some());
    p.clear_selection();
    assert!(p.selected_text().is_none());
    assert!(p.selection_rects().is_empty());
    assert!(!p.extend_drag(at(&p, 2, 0)));
}

#[test]
fn preview_scroll_and_resize_keep_text_positions_and_hit_the_drawn_text() {
    let (mut p, mut fs) = preview(TEXT, 145.0, 400.0);
    select(&mut p);
    let expected = p.selected_text();
    p.scroll_down(1);
    assert_eq!(p.selected_text(), expected);
    let pos = at(&p, 1, 7);
    let cursor = p.hit(pos, true).unwrap();
    assert_eq!((cursor.line, cursor.index), (1, 7));
    p.layout(ScreenRect { w: 110.0, ..p.rect() }, &mut fs);
    assert_eq!(p.selected_text(), expected);
    assert_eq!(p.hit(at(&p, 1, 7), true).map(|c| (c.line, c.index)), Some((1, 7)));
}

#[test]
fn preview_drag_outside_clamps_to_visible_lines() {
    let (mut p, _fs) = preview("alpha\nbravo\ncharlie\ndelta\necho", 200.0, 42.0);
    p.scroll_down(1);
    let start = p.hit(at(&p, 1, 0), true).unwrap();
    p.press(Some(start));
    assert!(p.extend_drag((p.rect().x + p.rect().w + 100.0, p.rect().y + 1000.0)));
    assert_eq!(p.selected_text().as_deref(), Some("bravo\ncharlie"));
    assert!(p.extend_drag((p.rect().x - 100.0, p.rect().y - 1000.0)));
    assert_eq!(p.selected_text().as_deref(), Some("alpha\n"));
}

#[test]
fn preview_tint_has_one_clipped_rectangle_per_selected_visual_line() {
    let (mut p, mut fs) = preview(TEXT, 145.0, 400.0);
    select(&mut p);
    let selected_lines = p.md().buffer.layout_runs().filter(|r| r.line_i == 0 || r.line_i == 1).count();
    let rects = p.selection_rects();
    assert_eq!(rects.len(), selected_lines);
    assert!((rects[0].x - at(&p, 0, 6).0).abs() < 5.0);
    assert!((rects[0].y - p.rect().y - EXTRA_TOP_PAD_PX).abs() < 0.01);
    p.scroll_down(1);
    p.layout(ScreenRect { h: 42.0, ..p.rect() }, &mut fs);
    let clipped = p.selection_rects();
    assert!(!clipped.is_empty());
    assert_eq!(clipped[0].y, p.rect().y, "the partly scrolled first line is clipped at the top");
    for r in clipped {
        assert!(r.w > 0.0 && r.h > 0.0);
        assert!(r.x >= p.rect().x && r.y >= p.rect().y);
        assert!(r.x + r.w <= p.rect().x + p.rect().w + 0.01);
        assert!(r.y + r.h <= p.rect().y + p.rect().h + 0.01);
    }
}

#[test]
fn preview_copy_uses_rendered_markdown_text() {
    let mut fs = glyphon::FontSystem::new();
    let highlight = crate::ui::preview::markdown::highlight::HighlightService::new().unwrap();
    let md = MarkdownPreview::new(&mut fs, "**hello** world", 400.0, 100.0, 1.0,
        &Default::default(), &Default::default(), &highlight, &Default::default());
    let mut p = ShapedPreview::new(md, highlight, ScreenRect { x: 0.0, y: 0.0, w: 400.0, h: 100.0 });
    let start = p.hit(at(&p, 0, 0), true).unwrap();
    p.press(Some(start));
    p.extend_drag(at(&p, 0, 5));
    p.release();
    assert_eq!(p.selected_text().as_deref(), Some("hello"));
}

#[test]
fn preview_selection_skips_separate_media_reservations() {
    let mut fs = glyphon::FontSystem::new();
    let highlight = crate::ui::preview::markdown::highlight::HighlightService::new().unwrap();
    let md = MarkdownPreview::new(&mut fs, "before\n\n$$x$$\n\nafter", 400.0, 600.0, 1.0,
        &Default::default(), &Default::default(), &highlight, &Default::default());
    assert!(!md.media_blocks.is_empty());
    let mut p = ShapedPreview::new(md, highlight, ScreenRect { x: 0.0, y: 0.0, w: 400.0, h: 600.0 });
    let media_top = p.md().buffer.layout_runs().find(|r| r.text.trim() == "\u{FFFC}")
        .map(|r| r.line_top + EXTRA_TOP_PAD_PX).expect("display math reservation");
    let start = p.hit(at(&p, 0, 0), true).unwrap();
    p.press(Some(start));
    p.extend_drag((399.0, 599.0));
    p.release();
    let text = p.selected_text().unwrap();
    assert!(text.contains("before") && text.contains("after"));
    assert!(!text.contains('\u{FFFC}'));
    assert!(p.selection_rects().iter().all(|r| r.y != media_top));
}
