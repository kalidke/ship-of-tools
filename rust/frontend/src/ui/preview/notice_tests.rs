use super::*;
use crate::ui::input::layout_help::PEEK_FADE;
use std::time::Duration;

fn shown(text: &str, at: Instant) -> PreviewNotice {
    let mut n = PreviewNotice::default();
    n.show(text.to_string(), at);
    n
}

const PANE: Rect = Rect { x: 40, y: 3, width: 70, height: 30 };

#[test]
fn a_notice_is_held_then_fades_then_is_gone() {
    let t = Instant::now();
    let n = shown("hello", t);
    assert_eq!(PreviewNotice::default().opacity(t), 0.0);
    assert_eq!(n.opacity(t), 1.0);
    assert_eq!(n.opacity(t + PEEK_HOLD), 1.0);
    assert!(!n.fading(t + PEEK_HOLD - Duration::from_millis(1)) && n.fading(t + PEEK_HOLD));
    assert_eq!(n.deadline(), Some(t + PEEK_HOLD));
    let half = n.opacity(t + PEEK_HOLD + PEEK_FADE / 2);
    assert!((half - 0.5).abs() < 0.01, "{half}");
    assert_eq!(n.opacity(t + PEEK_HOLD + PEEK_FADE), 0.0);
    // The same rule as the help peek's.
    assert_eq!(n.opacity(t + PEEK_HOLD + PEEK_FADE / 4), hold_then_fade(t, t + PEEK_HOLD + PEEK_FADE / 4));
}

#[test]
fn a_fade_that_is_over_is_dropped_once_and_draws_no_box() {
    let t = Instant::now();
    let mut n = shown("hello", t);
    assert!(n.boxed(PANE, t + PEEK_HOLD).is_some());
    assert!(n.boxed(PANE, t + PEEK_HOLD + PEEK_FADE).is_none());
    assert!(!n.expire(t + PEEK_HOLD) && n.text().is_some());
    assert!(n.expire(t + PEEK_HOLD + PEEK_FADE) && n.text().is_none());
    assert!(!n.expire(t + PEEK_HOLD + PEEK_FADE), "the drop is reported once");
    assert_eq!(n.deadline(), None);
}

#[test]
fn a_newer_notice_replaces_the_shown_one_and_restarts_the_hold() {
    let t = Instant::now();
    let mut n = shown("first", t);
    let later = t + PEEK_HOLD + PEEK_FADE / 2;
    assert!(n.opacity(later) < 1.0);
    n.show("second".to_string(), later);
    assert_eq!(n.text(), Some("second"));
    assert_eq!(n.opacity(later), 1.0);
    assert_eq!(n.deadline(), Some(later + PEEK_HOLD));
    assert_eq!(n.boxed(PANE, later).expect("shown").lines, ["second"]);
}

#[test]
fn a_cleared_notice_shows_nothing() {
    let t = Instant::now();
    let mut n = shown("about the row left", t);
    n.clear();
    assert!(n.text().is_none() && n.boxed(PANE, t).is_none() && n.deadline().is_none() && !n.expire(t));
}

#[test]
fn the_box_sits_at_the_panes_foot_inside_it_and_wraps_to_it() {
    let t = Instant::now();
    let text = "Can't draw on this SVG. Drawing (d) marks up an image file (PNG, JPEG, TIFF and the like) for this row's agent, \
        which sees your marks with your next prompt. To draw anyway, open a PNG of it, or press Shift+D for a blank canvas.";
    let b = shown(text, t).boxed(PANE, t).expect("a box");
    assert!(b.lines.len() > 1 && b.lines.len() <= MAX_ROWS, "{:?}", b.lines);
    assert!(b.lines.iter().all(|l| l.chars().count() <= PANE.width as usize - 4), "{:?}", b.lines);
    assert_eq!(b.rect.height as usize, b.lines.len() + 2);
    assert_eq!((b.rect.x, b.rect.width), (PANE.x, PANE.width));
    assert_eq!(b.rect.y + b.rect.height, PANE.y + PANE.height, "flush with the pane's foot, so the picture's top is clear");
    assert_eq!(b.opacity, 1.0);
}

#[test]
fn the_box_is_inside_the_pane_at_text_scale_1_and_2() {
    // The pane's cells at scale 2 are about half as many each way, each cell twice as large in pixels.
    let t = Instant::now();
    let text = "word ".repeat(80);
    for (scale, pane) in [(1.0f32, PANE), (2.0, Rect { x: 20, y: 2, width: 35, height: 14 }), (2.0, Rect { x: 0, y: 0, width: 12, height: 5 })] {
        let (cw, ch, ox, oy) = (9.0 * scale, 18.0 * scale, 4.0 * scale, 6.0 * scale);
        let b = shown(&text, t).boxed(pane, t).unwrap_or_else(|| panic!("no box in {pane:?}"));
        assert!(b.rect.x >= pane.x && b.rect.y >= pane.y && b.rect.x + b.rect.width <= pane.x + pane.width
            && b.rect.y + b.rect.height <= pane.y + pane.height, "{:?} not in {pane:?}", b.rect);
        // The pixels the overlay draws, as `prepare_overlays` maps them, lie in the pane's pixels.
        let (px, py, pw, ph) = (ox + b.rect.x as f32 * cw, oy + b.rect.y as f32 * ch, b.rect.width as f32 * cw, b.rect.height as f32 * ch);
        assert!(px >= ox + pane.x as f32 * cw && py >= oy + pane.y as f32 * ch);
        assert!(px + pw <= ox + (pane.x + pane.width) as f32 * cw && py + ph <= oy + (pane.y + pane.height) as f32 * ch);
        assert!(b.lines.len() <= MAX_ROWS && b.lines.len() + 2 <= pane.height as usize);
    }
}

#[test]
fn a_text_past_the_rows_ends_in_an_ellipsis_within_the_width() {
    let t = Instant::now();
    let b = shown(&"abcdefghij ".repeat(60), t).boxed(PANE, t).expect("a box");
    assert_eq!(b.lines.len(), MAX_ROWS);
    let last = b.lines.last().unwrap();
    assert!(last.ends_with('…') && last.chars().count() <= PANE.width as usize - 4, "{last}");
    // A short pane takes fewer rows still.
    let short = shown(&"abcdefghij ".repeat(60), t).boxed(Rect { height: 5, ..PANE }, t).expect("a box");
    assert_eq!(short.lines.len(), 3);
    assert!(short.rect.height <= 5);
}

#[test]
fn a_pane_too_small_for_a_row_takes_no_box() {
    let t = Instant::now();
    let n = shown("hello", t);
    assert!(n.boxed(Rect { width: 11, ..PANE }, t).is_none());
    assert!(n.boxed(Rect { height: 2, ..PANE }, t).is_none());
    assert!(n.boxed(Rect::default(), t).is_none());
    assert!(shown("", t).boxed(PANE, t).is_none());
}

#[test]
fn the_rows_are_cell_safe_whatever_the_text() {
    let t = Instant::now();
    // A file's own bytes: a color code, a tab, a bell, a lone CR, a bidi paragraph separator and a wide glyph.
    let text = "svg not drawn: \u{1b}[31mred\u{1b}[0m\ttab\u{7}bell\rcr\u{2029}para 日本語日本語日本語日本語日本語日本語日本語日本語日本語日本語日本語日本語日本語日本語";
    let b = shown(text, t).boxed(PANE, t).expect("a box");
    for line in &b.lines {
        assert!(!line.contains(char::is_control) && !line.contains(['\u{2028}', '\u{2029}']), "{line:?}");
        assert!(unicode_width::UnicodeWidthStr::width(line.as_str()) <= PANE.width as usize - 4, "{line:?}");
    }
    assert!(b.lines.iter().any(|l| l.contains("red")) && !b.lines.iter().any(|l| l.contains("[31m")));
    assert!(b.lines.len() >= 3, "each separator ends a row: {:?}", b.lines);
}
