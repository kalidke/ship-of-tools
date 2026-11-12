//! The shared owner against a fixture pane that is not the preview: it shapes wrapped Unicode text itself, and must
//! copy the ranges and clip the tint exactly as the preview does for the same text.

use super::*;
use crate::ui::preview::markdown::{highlight::HighlightService, MarkdownPreview};
use crate::ui::preview::shaped::ShapedPreview;
use crate::ui::render::text::{set_buffer_text, EXTRA_TOP_PAD_PX};
use cosmic_text::{Attrs, Family, FontSystem, Shaping};

const TEXT: &str = "alpha beta gamma delta epsilon zeta eta theta\nbravé ñandú charlie\nomega";
const RECT: ScreenRect = ScreenRect { x: 37.0, y: 61.0, w: 145.0, h: 400.0 };

/// A pane of the simplest kind: a buffer, a rect and a scroll in pixels.
struct Fixture { buffer: Buffer, rect: ScreenRect, scroll_px: f32, reserved: Option<char> }

impl Fixture {
    fn new(fs: &mut FontSystem, metrics: cosmic_text::Metrics, text: &str, reserved: Option<char>) -> Self {
        let mut buffer = Buffer::new(fs, metrics);
        buffer.set_size(fs, Some(RECT.w), None);
        let attrs = Attrs::new().family(Family::Monospace).metrics(metrics);
        set_buffer_text(&mut buffer, fs, text, attrs, Shaping::Advanced);
        buffer.shape_until_scroll(fs, false);
        Self { buffer, rect: RECT, scroll_px: 0.0, reserved }
    }

    fn lent(&self) -> FlowedText<'_> {
        let rect = self.rect;
        let origin_y = rect.y + EXTRA_TOP_PAD_PX - self.scroll_px;
        let clip_bottom = (rect.y + EXTRA_TOP_PAD_PX
            + whole_line_bottom(&self.buffer, self.scroll_px, rect.h - EXTRA_TOP_PAD_PX)).min(rect.y + rect.h);
        FlowedText { buffer: &self.buffer, rect, origin_y, clip_bottom, reserved: self.reserved, shown: None }
    }
}

/// The bottom of the last whole visual line that fits, as the renderer clips.
fn whole_line_bottom(buffer: &Buffer, scroll_px: f32, height: f32) -> f32 {
    let fit = buffer.layout_runs().map(|r| r.line_top + r.line_height - scroll_px)
        .take_while(|&bot| bot <= height + 0.5).fold(0.0_f32, f32::max);
    if fit > 0.0 { fit } else { height }
}

fn glyph_point(text: &FlowedText, line: usize, byte: usize) -> (f32, f32) {
    for run in text.buffer.layout_runs() {
        if run.line_i != line { continue; }
        if let Some(g) = run.glyphs.iter().find(|g| g.start == byte) {
            return (text.rect.x + g.x + g.w * 0.15, text.origin_y + run.line_top + run.line_height * 0.5);
        }
    }
    panic!("no glyph at ({line}, {byte})");
}

fn owner_drag(owner: &mut FlowedSelection, text: &FlowedText, at: (f32, f32)) -> bool {
    owner.extend_drag(FlowedSelection::hit(text, at, false))
}

fn near(a: &[ScreenRect], b: &[ScreenRect]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(p, q)|
        (p.x - q.x).abs() < 0.01 && (p.y - q.y).abs() < 0.01 && (p.w - q.w).abs() < 0.01 && (p.h - q.h).abs() < 0.01)
}

#[test]
fn a_fixture_pane_copies_and_clips_as_the_preview_does() {
    let mut fs = FontSystem::new();
    let md = MarkdownPreview::new_plain(&mut fs, TEXT, RECT.w, 1.0);
    let metrics = md.buffer.metrics();
    let mut preview = ShapedPreview::new(md, HighlightService::new().unwrap(), RECT);
    let fixture = Fixture::new(&mut fs, metrics, TEXT, None);
    assert!(fixture.buffer.layout_runs().filter(|r| r.line_i == 0).count() > 1, "line 0 must wrap");
    let text = fixture.lent();

    // The same drag on both: from line 0 to line 2, then back up to line 1.
    let mut owner = FlowedSelection::default();
    let start = glyph_point(&text, 0, 6);
    owner.press(FlowedSelection::hit(&text, start, true));
    preview.press(preview.hit(start, true));
    for (line, byte) in [(2, 3), (1, 7)] {
        let at = glyph_point(&text, line, byte);
        assert_eq!(owner_drag(&mut owner, &text, at), preview.extend_drag(at));
        assert_eq!(owner.text(&text), preview.selected_text());
        assert!(owner.text(&text).is_some());
    }
    assert_eq!(owner.text(&text).as_deref(), Some("beta gamma delta epsilon zeta eta theta\nbravé "),
        "a soft wrap adds no newline");
    assert!(near(&owner.rects(&text), &preview.selection_rects()));
    assert!(owner.rects(&text).len() > 2);

    // A drag far outside clamps to the visible text.
    let far = (RECT.x + RECT.w + 100.0, RECT.y + 5000.0);
    assert_eq!(owner_drag(&mut owner, &text, far), preview.extend_drag(far));
    assert_eq!(owner.text(&text), preview.selected_text());
    assert!(near(&owner.rects(&text), &preview.selection_rects()));

    // A release ends the drag; an outside press clears the range.
    owner.release();
    assert!(!owner_drag(&mut owner, &text, glyph_point(&text, 0, 0)));
    assert!(FlowedSelection::hit(&text, (RECT.x - 1.0, RECT.y), true).is_none());
    assert!(owner.press(None));
    assert!(owner.text(&text).is_none() && owner.rects(&text).is_empty());
}

#[test]
fn a_reserved_placeholder_is_never_copied_or_tinted() {
    let mut fs = FontSystem::new();
    let metrics = MarkdownPreview::new_plain(&mut fs, "x", RECT.w, 1.0).buffer.metrics();
    let fixture = Fixture::new(&mut fs, metrics, "before\n\u{FFFC}\nafter", Some('\u{FFFC}'));
    let text = fixture.lent();
    let mut owner = FlowedSelection::default();
    owner.press(FlowedSelection::hit(&text, glyph_point(&text, 0, 0), true));
    owner_drag(&mut owner, &text, (RECT.x + RECT.w - 1.0, RECT.y + RECT.h - 1.0));
    let copied = owner.text(&text).unwrap();
    assert!(copied.starts_with("before\n") && !copied.contains('\u{FFFC}'), "{copied:?}");
    let media_top = text.buffer.layout_runs().find(|r| r.line_i == 1).map(|r| text.origin_y + r.line_top).unwrap();
    assert!(owner.rects(&text).iter().all(|r| r.y != media_top));
    assert!(!owner.rects(&text).is_empty());
}

#[test]
fn a_scrolled_short_pane_clips_the_tint_as_the_preview_does() {
    let mut fs = FontSystem::new();
    let rect = ScreenRect { h: 52.0, ..RECT };
    let md = MarkdownPreview::new_plain(&mut fs, TEXT, rect.w, 1.0);
    let metrics = md.buffer.metrics();
    let line = md.line_height();
    let mut preview = ShapedPreview::new(md, HighlightService::new().unwrap(), rect);
    preview.scroll_down(1);
    let mut fixture = Fixture::new(&mut fs, metrics, TEXT, None);
    fixture.rect = rect;
    fixture.scroll_px = line;
    let text = fixture.lent();
    let mut owner = FlowedSelection::default();
    let start = (rect.x + 2.0, rect.y + EXTRA_TOP_PAD_PX + 2.0);
    owner.press(FlowedSelection::hit(&text, start, true));
    preview.press(preview.hit(start, true));
    let far = (rect.x + rect.w + 100.0, rect.y + 5000.0);
    assert_eq!(owner_drag(&mut owner, &text, far), preview.extend_drag(far));
    let rects = owner.rects(&text);
    assert!(!rects.is_empty() && near(&rects, &preview.selection_rects()));
    assert_eq!(owner.text(&text), preview.selected_text());
    // 52 leaves a partial line below the last whole one: the drag and the tint stop at the whole line.
    assert!(text.clip_bottom < rect.y + rect.h - 1.0);
    assert!(!owner.text(&text).unwrap().contains("brav"), "the drag stops at the last whole line");
    assert!(rects.iter().all(|r| r.w > 0.0 && r.h > 0.0 && r.x >= rect.x && r.y >= rect.y
        && r.x + r.w <= rect.x + rect.w + 0.01 && r.y + r.h <= rect.y + rect.h + 0.01));
}

#[test]
fn a_range_past_the_clip_is_tinted_only_to_the_last_whole_line() {
    let mut fs = FontSystem::new();
    let metrics = MarkdownPreview::new_plain(&mut fs, "x", RECT.w, 1.0).buffer.metrics();
    let mut fixture = Fixture::new(&mut fs, metrics, TEXT, None);
    let mut owner = FlowedSelection::default();
    let tall = fixture.lent();
    owner.press(FlowedSelection::hit(&tall, glyph_point(&tall, 0, 0), true));
    owner_drag(&mut owner, &tall, (RECT.x + RECT.w - 1.0, RECT.y + 399.0));
    fixture.rect = ScreenRect { h: 52.0, ..RECT };
    let short = fixture.lent();
    let low = owner.rects(&short).iter().map(|r| r.y + r.h).fold(0.0_f32, f32::max);
    assert!(short.clip_bottom < short.rect.y + short.rect.h - 1.0);
    assert!((low - short.clip_bottom).abs() < 0.01, "{low} vs {}", short.clip_bottom);
}

/// The share reads the ordered range only once the drag is released, whichever way it was dragged.
#[test]
fn the_released_range_is_ordered_and_absent_during_a_drag() {
    let mut fs = FontSystem::new();
    let md = MarkdownPreview::new_plain(&mut fs, TEXT, RECT.w, 1.0);
    let metrics = md.buffer.metrics();
    let mut preview = ShapedPreview::new(md, HighlightService::new().unwrap(), RECT);
    let text = Fixture::new(&mut fs, metrics, TEXT, None);
    let text = text.lent();
    let from = glyph_point(&text, 1, 7);
    preview.press(preview.hit(from, true));
    preview.extend_drag(glyph_point(&text, 0, 6));
    assert!(preview.released_selection().is_none(), "nothing during a drag");
    preview.release();
    let (range, shown) = preview.released_selection().unwrap();
    assert_eq!(range.0, (0, 6));
    assert_eq!(range.1.0, 1);
    assert_eq!(Some(shown), preview.selected_text());
}
