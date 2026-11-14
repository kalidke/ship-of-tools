//! Behavior of selection in the actual flowed preview buffer, without a window or GPU.

use super::*;
use crate::ui::render::text::EXTRA_TOP_PAD_PX;
use std::time::{Duration, Instant};

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
                    - p.scroll_px());
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
    p.scroll_by(1, std::time::Instant::now(), std::time::Duration::ZERO);
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
    p.scroll_by(1, std::time::Instant::now(), std::time::Duration::ZERO);
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
    p.scroll_by(1, std::time::Instant::now(), std::time::Duration::ZERO);
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
        &Default::default(), &Default::default(), &highlight, &Default::default(), false);
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
        &Default::default(), &Default::default(), &highlight, &Default::default(), false);
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

#[test]
fn preview_literal_placeholder_without_media_is_copied_and_tinted() {
    let (mut p, _fs) = preview("a\u{FFFC}b\n\u{FFFC}\nc", 200.0, 400.0);
    let start = p.hit(at(&p, 0, 0), true).unwrap();
    p.press(Some(start));
    p.extend_drag(at(&p, 2, 0));
    assert_eq!(p.selected_text().as_deref(), Some("a\u{FFFC}b\n\u{FFFC}\n"));
    assert_eq!(p.selection_rects().len(), 2);
}

/// A markdown preview of `source` in a pane of `width`, shaped as the preview pane shapes a project file.
fn markdown(source: &str, width: f32, maps_source: bool) -> (ShapedPreview, glyphon::FontSystem) {
    let mut fs = glyphon::FontSystem::new();
    let highlight = crate::ui::preview::markdown::highlight::HighlightService::new().unwrap();
    let md = MarkdownPreview::new(&mut fs, source, width, 4000.0, 1.0,
        &Default::default(), &Default::default(), &highlight, &Default::default(), maps_source);
    (ShapedPreview::new(md, highlight, ScreenRect { x: 0.0, y: 0.0, w: width, h: 4000.0 }), fs)
}

/// The point on the trailing part of the last glyph of buffer line `line`.
fn at_end(p: &ShapedPreview, line: usize) -> (f32, f32) {
    let run = p.md().buffer.layout_runs().filter(|r| r.line_i == line).last().expect("a run");
    let glyph = run.glyphs.last().expect("a glyph");
    (p.rect().x + glyph.x + glyph.w * 0.85,
        p.rect().y + EXTRA_TOP_PAD_PX + run.line_top + run.line_height * 0.5)
}

fn line_starting(p: &ShapedPreview, prefix: &str) -> usize {
    p.md().buffer.lines.iter().position(|l| l.text().trim_start().starts_with(prefix)).expect("a buffer line")
}

fn release_between(p: &mut ShapedPreview, from: (f32, f32), to: (f32, f32)) {
    let start = p.hit(from, true).unwrap();
    p.press(Some(start));
    assert!(p.extend_drag(to));
    p.release();
}

const PROBE: &str = include_str!("markdown/testdata/selection_probe.md");

#[test]
fn a_rendered_buffer_with_no_map_shares_nothing() {
    // The buffer positions of `World` happen to be the file's (2, 0..5): a rendered buffer must not answer them.
    let (mut p, _fs) = markdown("Hello\n\nWorld\n", 400.0, false);
    let line = line_starting(&p, "World");
    assert_eq!(line, 2);
    let (from, to) = (at(&p, line, 0), at_end(&p, line));
    release_between(&mut p, from, to);
    assert_eq!(p.selected_text().as_deref(), Some("World"));
    assert_eq!(p.released_selection(), None);
}

#[test]
fn a_rendered_markdown_selection_is_shared_as_the_files_own_lines() {
    let paragraph = "First line of a paragraph\nwith `a code\nspan` and https://bare.example/url\nlast line of it.";
    for source in [PROBE.to_string(), PROBE.replace('\n', "\r\n")] {
        let (mut p, _fs) = markdown(&source, 200.0, true);
        let line = line_starting(&p, "First line");
        assert!(p.md().buffer.layout_runs().filter(|r| r.line_i == line).count() > 1, "the pane wraps the paragraph");
        let (from, to) = (at(&p, line, 0), at_end(&p, line));
        release_between(&mut p, from, to);
        assert!(p.selected_text().unwrap().starts_with("First line of a paragraph with a code span"));
        assert_eq!(p.released_selection(), Some((((6, 0), (9, 16)), paragraph.to_string())));
        let bare = p.md().buffer.lines[line].text().find("bare").unwrap();
        let (from, to) = (at(&p, line, bare), at(&p, line, bare + 4));
        release_between(&mut p, from, to);
        assert_eq!(p.selected_text().as_deref(), Some("bare"));
        assert_eq!(p.released_selection(), Some((((6, 0), (9, 16)), paragraph.to_string())));
        let code = line_starting(&p, "beta");
        let (from, to) = (at(&p, code, 1), at_end(&p, code));
        release_between(&mut p, from, to);
        assert_eq!(p.selected_text().as_deref(), Some("beta = 2"));
        assert_eq!(p.released_selection(), Some((((29, 0), (29, 8)), "beta = 2".to_string())));
    }
}

const LENGTH: Duration = Duration::from_millis(150);

fn long() -> String { (0..40).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n") }

fn frame(t0: Instant, ms: u64) -> Instant { t0 + Duration::from_millis(ms) }

/// The screen y of a buffer line's top edge at the preview's drawn offset.
fn line_top(p: &ShapedPreview, line: usize) -> f32 {
    let top = p.md().buffer.layout_runs().find(|r| r.line_i == line).unwrap().line_top;
    p.rect().y + EXTRA_TOP_PAD_PX + top - p.scroll_px()
}

#[test]
fn a_wheel_moves_the_preview_over_frames_and_its_hit_test_follows() {
    let (mut p, _fs) = preview(&long(), 300.0, 200.0);
    let t0 = Instant::now();
    p.scroll_by(3, t0, LENGTH);
    assert_eq!(p.scroll(), 0.0, "nothing moves at the event itself");
    assert!(p.advance(frame(t0, 16), 30, 5.0, false), "frames keep coming while it moves");
    let (lines, px) = (p.scroll(), p.scroll_px());
    assert!(lines > 0.0 && lines < 3.0 && lines.fract() != 0.0, "{lines}");
    assert_eq!(px, px.round(), "the drawn offset is a whole pixel");
    // The hit test reads the same offset: a point a hair inside line 3's top edge is line 3, a hair above it line 2.
    let (x, y) = (p.rect().x + 2.0, line_top(&p, 3));
    assert_eq!(p.hit((x, y + 0.1), true).map(|c| c.line), Some(3));
    assert_eq!(p.hit((x, y - 0.1), true).map(|c| c.line), Some(2));
    assert!(p.advance(frame(t0, 100), 30, 5.0, false));
    assert!(!p.advance(frame(t0, 150), 30, 5.0, false), "the last frame rests it");
    assert_eq!(p.scroll(), 3.0, "on the asked line, exactly");
    assert_eq!(p.scroll_px(), 3.0 * p.md().line_height());
    let (x, y) = (p.rect().x + 2.0, line_top(&p, 3));
    assert_eq!(p.hit((x, y + 0.1), true).map(|c| c.line), Some(3));
}

#[test]
fn a_zero_length_scroll_lands_at_once_and_an_edge_drops_what_is_owed() {
    let (mut p, _fs) = preview(&long(), 300.0, 200.0);
    let t0 = Instant::now();
    p.scroll_by(4, t0, Duration::ZERO);
    assert_eq!(p.scroll(), 4.0);
    assert!(!p.advance(frame(t0, 16), 30, 5.0, false));
    p.scroll_by(-5, t0, LENGTH);
    assert!(p.advance(frame(t0, 16), 30, 5.0, false));
    // The top is met before the owed distance is spent: it rests there, and nothing owed keeps frames alive.
    assert!(!p.advance(frame(t0, 150), 30, 5.0, false));
    assert_eq!(p.scroll(), 0.0);
    p.scroll_by(40, t0, LENGTH);
    assert!(!p.advance(frame(t0, 150), 30, 5.0, false));
    assert_eq!(p.scroll(), 30.0, "the end clamps what was asked past it");
    assert!(!p.advance(frame(t0, 300), 30, 5.0, false));
}

#[test]
fn home_and_end_ask_their_whole_distance_resolved_at_the_layouts_end() {
    let (mut p, _fs) = preview(&long(), 300.0, 200.0);
    let t0 = Instant::now();
    p.scroll_to_end(t0, LENGTH);
    assert_eq!(p.scroll(), 0.0);
    // The layout knows the end: 30 lines, five visible. Only the last pane of the travel is eased.
    assert!(p.advance(frame(t0, 16), 30, 5.0, false));
    assert!(p.scroll() > 25.0 && p.scroll() < 30.0, "{}", p.scroll());
    assert!(!p.advance(frame(t0, 150), 30, 5.0, false));
    assert_eq!(p.scroll(), 30.0);
    p.scroll_to_top(t0, LENGTH);
    assert!(p.advance(frame(t0, 16), 30, 5.0, false));
    assert!(p.scroll() < 5.0 && p.scroll() > 0.0, "{}", p.scroll());
    assert!(!p.advance(frame(t0, 150), 30, 5.0, false));
    assert_eq!(p.scroll(), 0.0);
    // A zero length is today's End: the position the layout clamps.
    p.scroll_to_end(t0, Duration::ZERO);
    assert!(!p.advance(frame(t0, 300), 30, 5.0, false));
    assert_eq!(p.scroll(), 30.0);
}

#[test]
fn a_new_file_and_a_change_of_buffer_end_what_is_owed() {
    let (mut p, mut fs) = preview(&long(), 300.0, 200.0);
    let t0 = Instant::now();
    p.scroll_by(6, t0, LENGTH);
    p.advance(frame(t0, 16), 30, 5.0, false);
    p.install(MarkdownPreview::new_plain(&mut fs, &long(), 300.0, 1.0));
    assert_eq!(p.scroll(), 0.0);
    assert!(!p.advance(frame(t0, 32), 30, 5.0, false), "a new file is at the top and owes nothing");
    assert_eq!(p.scroll(), 0.0);
    p.scroll_by(6, t0, LENGTH);
    p.advance(frame(t0, 16), 30, 5.0, false);
    let kept = p.scroll();
    assert!(!p.advance(frame(t0, 32), 30, 5.0, true), "the editor opening stops it where it stood");
    assert_eq!(p.scroll(), kept.round());
    assert!(!p.advance(frame(t0, 300), 30, 5.0, true));
    assert_eq!(p.scroll(), kept.round());
}

#[test]
fn a_reanchor_inside_the_file_eases_and_a_reply_anchor_lands_at_once() {
    let src = "module M\n\nexport foo\n\n\"\"\"\nfoo docstring\n\"\"\"\nfunction foo()\nend\n";
    let (mut p, _fs) = preview(src, 2000.0, 400.0);
    let t0 = Instant::now();
    assert!(p.reanchor(Some(8), true, t0, LENGTH));
    assert_eq!(p.scroll(), 0.0);
    assert!(p.advance(frame(t0, 16), 40, 9.0, false));
    assert!(p.scroll() > 0.0 && p.scroll() < 4.0);
    assert!(!p.advance(frame(t0, 150), 40, 9.0, false));
    assert_eq!(p.scroll(), 4.0);
    p.scroll_by(5, t0, LENGTH);
    p.arm_anchor(Some(8));
    p.settle_anchor(true);
    assert_eq!(p.scroll(), 4.0);
    assert!(!p.advance(frame(t0, 32), 40, 9.0, false), "a reply's anchor owes nothing");
}
