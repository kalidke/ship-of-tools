//! Up, Down, PageUp and PageDown in edit mode move by the row on screen. A winit key event cannot be built outside the
//! event loop, so the tests press through `edit_buffer_key`, which `Editor::key` calls for every key the buffer takes.

use super::*;
use cosmic_text::FontSystem;
use std::time::{Duration, Instant};

/// An editor over `content`, shaped at `cells` monospace cells of width.
struct Harness {
    fs: FontSystem,
    width: f32,
    editor: Editor,
}

impl Harness {
    fn new(content: &str, cells: usize) -> Self {
        let mut fs = FontSystem::new();
        let one = MarkdownPreview::new_plain_spans(&mut fs, &[("m".into(), false)], 1000.0, 1.0);
        let cell = one.buffer.layout_runs().next().unwrap().glyphs[0].w;
        let mut editor = Editor::default();
        editor.arm_file("files:x".into());
        editor.open_file("files:x", true, content.to_string(), "v".into());
        Self { fs, width: cell * cells as f32 + 0.5, editor }
    }

    fn edit(&mut self) -> &mut EditState {
        self.editor.edit.as_mut().unwrap()
    }

    fn cursor(&mut self) -> usize {
        self.edit().buf.cursor()
    }

    /// Place the cursor as a click would: no goal column carries over.
    fn put(&mut self, at: usize) {
        self.edit().buf.set_cursor(at);
        self.edit().goal_x = None;
    }

    fn press(&mut self, logical: Key, action: Option<Action>, shift: bool, page_rows: usize) {
        let (fs, width) = (&mut self.fs, self.width);
        let pressed = Pressed { logical: &logical, action, ctrl: false, alt: false, shift, super_: false };
        let edit = self.editor.edit.as_mut().unwrap();
        edit_buffer_key(edit, pressed, page_rows, |spans| MarkdownPreview::new_plain_spans(fs, spans, width, 1.0));
    }

    fn act(&mut self, action: Action) {
        self.press(Key::Named(NamedKey::ArrowDown), Some(action), false, 5);
    }

    fn type_char(&mut self, c: &str) {
        self.press(Key::Character(c.into()), None, false, 5);
    }

    /// The number of rows the body shapes to on the line `line`.
    fn rows_of_line(&mut self, line: usize) -> usize {
        let body = format!("{}\n", self.edit().buf.body());
        let shaped = MarkdownPreview::new_plain_spans(&mut self.fs, &[(body, false)], self.width, 1.0);
        shaped.buffer.layout_runs().filter(|r| r.line_i == line).count()
    }
}

const PARAGRAPH: &str = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau";

#[test]
fn a_long_texts_shaped_rows_reach_its_last_line() {
    let mut fs = FontSystem::new();
    let body = (0..1000).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
    let shaped = MarkdownPreview::new_plain_spans(&mut fs, &[(body, false)], 400.0, 1.0);
    assert_eq!(shaped.buffer.layout_runs().map(|r| r.line_i).max(), Some(999));
}

#[test]
fn up_and_down_move_one_row_and_stay_in_a_wrapped_paragraph() {
    let content = format!("before\n\n{PARAGRAPH}\n\nafter");
    let mut h = Harness::new(&content, 24);
    let (start, end) = (8, 8 + PARAGRAPH.len());
    let rows = h.rows_of_line(2);
    assert!(rows >= 4, "the paragraph must wrap over several rows, got {rows}");
    h.put(start);
    let mut last = start;
    for _ in 0..rows - 1 {
        h.act(Action::EditDown);
        let at = h.cursor();
        assert!(at > last && at < end, "a Down inside the paragraph stays in it: {last} -> {at}");
        last = at;
    }
    h.act(Action::EditDown);
    assert_eq!(h.cursor(), end + 1, "from the last row, Down goes to the blank line below");
    h.act(Action::EditUp);
    assert!(h.cursor() > start && h.cursor() < end, "Up from the blank line enters the paragraph's last row");
    for _ in 0..rows - 1 {
        h.act(Action::EditUp);
    }
    assert!(h.cursor() >= start && h.cursor() < start + 24, "Up reaches the paragraph's first row");
    h.act(Action::EditUp);
    assert_eq!(h.cursor(), 7, "from the first row, Up goes to the blank line above");
}

#[test]
fn the_column_is_kept_across_a_shorter_row_until_another_key() {
    let (a, b, c) = ("a".repeat(60), "b".repeat(10), "c".repeat(60));
    let mut h = Harness::new(&format!("{a}\n{b}\n{c}"), 100);
    h.put(40);
    h.act(Action::EditDown);
    assert_eq!(h.cursor(), 61 + 10, "the short row is entered at its end");
    h.act(Action::EditDown);
    assert_eq!(h.cursor(), 72 + 40, "the column comes back on the third row");
    h.act(Action::EditUp);
    assert_eq!(h.cursor(), 71);
    h.act(Action::EditUp);
    assert_eq!(h.cursor(), 40);

    // Any other motion resets the goal.
    h.act(Action::EditDown);
    h.act(Action::EditLeft);
    assert_eq!(h.cursor(), 70);
    h.act(Action::EditDown);
    assert_eq!(h.cursor(), 72 + 9);

    // So does an edit.
    h.put(40);
    h.act(Action::EditDown);
    h.type_char("x");
    assert_eq!(h.cursor(), 72);
    h.act(Action::EditDown);
    assert_eq!(h.cursor(), 73 + 11);
}

#[test]
fn shift_extends_the_selection_by_rows() {
    let (a, b) = ("a".repeat(60), "b".repeat(10));
    let mut h = Harness::new(&format!("{a}\n{b}\n{a}"), 100);
    h.put(40);
    h.press(Key::Named(NamedKey::ArrowDown), Some(Action::EditDown), true, 5);
    assert_eq!(h.edit().buf.selection_range(), Some((40, 71)));
    h.press(Key::Named(NamedKey::ArrowDown), Some(Action::EditDown), true, 5);
    assert_eq!(h.edit().buf.selection_range(), Some((40, 112)));
    h.act(Action::EditUp);
    assert_eq!(h.edit().buf.selection_range(), None, "a plain motion drops the selection");
}

#[test]
fn up_on_the_first_row_goes_to_the_start_and_down_on_the_last_to_the_end() {
    let mut h = Harness::new("one\ntwo\nthree", 40);
    h.put(2);
    h.act(Action::EditUp);
    assert_eq!(h.cursor(), 0);
    h.put(9);
    h.act(Action::EditDown);
    assert_eq!(h.cursor(), 13);

    // The read-only header above the body is no row to move into.
    let mut editor = Editor::default();
    editor.enter_concept("files/x".into(), None, "---\nkind: note\n---\nbody one\nbody two");
    h.editor = editor;
    h.put(3);
    h.act(Action::EditUp);
    assert_eq!(h.cursor(), 0);
}

#[test]
fn a_wrapped_paragraph_under_a_header_is_walked_by_rows() {
    let mut editor = Editor::default();
    editor.enter_concept("files/x".into(), None, &format!("---\nkind: note\n---\n{PARAGRAPH}\nnext"));
    let mut h = Harness::new("", 24);
    h.editor = editor;
    assert!(h.rows_of_line(0) >= 4);
    h.act(Action::EditDown);
    let at = h.cursor();
    assert!(at > 0 && at < PARAGRAPH.len(), "the first Down stays in the paragraph, got {at}");
}

#[test]
fn the_cursor_mark_does_not_move_the_column() {
    // One 50-letter word on 20-cell rows: the row below starts at 20, and the cursor's own cell must not shift it.
    let mut h = Harness::new(&"x".repeat(50), 20);
    h.put(3);
    h.act(Action::EditDown);
    assert_eq!(h.cursor(), 23);
    h.act(Action::EditDown);
    assert_eq!(h.cursor(), 43);
    h.act(Action::EditUp);
    assert_eq!(h.cursor(), 23);
}

#[test]
fn page_keys_take_the_same_step_by_the_pane_rows() {
    let content = format!("{PARAGRAPH} {PARAGRAPH}\ntail");
    let mut h = Harness::new(&content, 16);
    assert!(h.rows_of_line(0) >= 8);
    h.put(5);
    for _ in 0..3 {
        h.act(Action::EditDown);
    }
    let by_rows = h.cursor();
    assert!(by_rows > 5 && by_rows < PARAGRAPH.len(), "three rows down is still inside the paragraph: {by_rows}");
    h.put(5);
    h.press(Key::Named(NamedKey::PageDown), Some(Action::EditPageDown), false, 3);
    assert_eq!(h.cursor(), by_rows);
    h.press(Key::Named(NamedKey::PageUp), Some(Action::EditPageUp), false, 3);
    assert!(h.cursor() < 16, "three rows up from the fourth row is the first: {}", h.cursor());
    h.press(Key::Named(NamedKey::PageUp), Some(Action::EditPageUp), false, 3);
    assert_eq!(h.cursor(), 0, "a page past the top stops at the start");
}

#[test]
fn text_the_rows_cannot_be_matched_to_steps_by_the_files_lines() {
    for sep in ["\r", "\u{2028}", "\u{85}", "\u{2029}"] {
        let first = format!("aaaa{sep}bbbb");
        let mut h = Harness::new(&format!("{first}\ncccc"), 40);
        h.put(2);
        h.act(Action::EditDown);
        assert_eq!(h.cursor(), first.len() + 1 + 2, "separator {sep:?}: the file's next line, same column");
        h.act(Action::EditUp);
        assert_eq!(h.cursor(), 2);
    }
}

#[test]
fn a_body_ending_in_a_newline_is_walked_by_rows() {
    let mut h = Harness::new(&format!("{PARAGRAPH}\n"), 24);
    h.act(Action::EditDown);
    assert!(h.cursor() > 0 && h.cursor() < PARAGRAPH.len(), "stays in the paragraph, got {}", h.cursor());
    for _ in 0..h.rows_of_line(0) {
        h.act(Action::EditDown);
    }
    assert_eq!(h.cursor(), PARAGRAPH.len() + 1, "the empty line after the final newline is a row");
}

/// A pane over the editor: a preview whose line is taller than the editor's (prose, 22 px against 19), `visible` px
/// of text high, which each cursor move scrolls as the window's key path does and then lays out as a frame does.
struct Pane {
    view: ShapedPreview,
    visible: f32,
}

impl Pane {
    fn new(h: &mut Harness, visible: f32) -> Self {
        let highlight = crate::ui::preview::markdown::highlight::HighlightService::new().unwrap();
        let md = MarkdownPreview::new(&mut h.fs, "text", 400.0, visible, 1.0, &Default::default(), &Default::default(),
            &highlight, &Default::default(), false);
        let rect = ScreenRect { x: 0.0, y: 0.0, w: 400.0, h: visible };
        assert!(md.line_height() > 19.0, "the preview's line must be taller than the editor's");
        Self { view: ShapedPreview::new(md, highlight, rect), visible }
    }

    /// The key path's own steps after a key: re-shape, follow, then the frame's clamp.
    fn after_key(&mut self, h: &mut Harness) {
        let (fs, width) = (&mut h.fs, h.width);
        h.editor.reshape(&KeyBindings::defaults(), |spans| MarkdownPreview::new_plain_spans(fs, spans, width, 1.0));
        h.editor.follow_cursor(&mut self.view, self.visible, Instant::now(), Duration::ZERO);
        self.frame(h);
    }

    fn frame(&mut self, h: &Harness) {
        let line_h = self.view.md().line_height();
        let max = crate::ui::preview::pane::preview_max_scroll(line_h, self.visible, h.editor.shaped().unwrap());
        self.view.advance(Instant::now(), max, self.visible / line_h, true);
    }

    /// Whether the cursor's row is wholly inside what the pane draws.
    fn shows_cursor(&self, h: &Harness) -> bool {
        let (top, bottom) = h.editor.cursor_row().expect("the cursor's row is in the layout");
        let from = self.view.scroll_px();
        top >= from && bottom <= from + self.visible
    }
}

fn long_text() -> String {
    (0..200).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n")
}

#[test]
fn the_view_scrolls_just_far_enough_to_show_the_cursor_row() {
    let mut h = Harness::new(&long_text(), 40);
    let mut pane = Pane::new(&mut h, 200.0);
    pane.after_key(&mut h);
    for _ in 0..60 {
        h.act(Action::EditDown);
        pane.after_key(&mut h);
        assert!(pane.shows_cursor(&h), "the cursor is out of view at {}", h.cursor());
    }
    let (_, bottom) = h.editor.cursor_row().unwrap();
    let slack = pane.view.scroll_px() + pane.visible - bottom;
    assert!(pane.view.scroll() > 0.0 && slack < pane.view.md().line_height(), "scrolled past what the row needed: {slack}");
    // A jump to the end, then to the start, brings each into view; the start is the top.
    h.act(Action::EditFinish);
    pane.after_key(&mut h);
    assert!(pane.shows_cursor(&h) && pane.view.scroll() > 100.0);
    h.act(Action::EditStart);
    pane.after_key(&mut h);
    assert!(pane.shows_cursor(&h));
    assert_eq!(pane.view.scroll(), 0.0);
}

#[test]
fn a_motion_that_keeps_the_cursor_in_view_does_not_scroll() {
    let mut h = Harness::new(&long_text(), 40);
    let mut pane = Pane::new(&mut h, 200.0);
    pane.after_key(&mut h);
    for action in [Action::EditDown, Action::EditDown, Action::EditRight, Action::EditUp] {
        h.act(action);
        pane.after_key(&mut h);
        assert_eq!(pane.view.scroll(), 0.0, "{action:?} scrolled a cursor that stayed in view");
    }
    // Scrolled partway, a step that stays inside the view moves nothing either.
    for _ in 0..30 {
        h.act(Action::EditDown);
        pane.after_key(&mut h);
    }
    let at = pane.view.scroll();
    assert!(at > 0.0);
    h.act(Action::EditUp);
    pane.after_key(&mut h);
    assert_eq!(pane.view.scroll(), at);
}

#[test]
fn the_wheel_scrolls_away_and_the_next_key_brings_the_view_back() {
    let mut h = Harness::new(&long_text(), 40);
    let mut pane = Pane::new(&mut h, 200.0);
    pane.after_key(&mut h);
    h.act(Action::EditDown);
    pane.after_key(&mut h);
    pane.view.scroll_by(40, Instant::now(), Duration::ZERO);
    pane.frame(&h);
    assert_eq!(pane.view.scroll(), 40.0, "the wheel moves the edit view");
    assert!(!pane.shows_cursor(&h), "the cursor is now out of view");
    h.act(Action::EditRight);
    pane.after_key(&mut h);
    assert!(pane.shows_cursor(&h), "the next motion brings the view back");
    assert_eq!(pane.view.scroll(), 0.0);
}

#[test]
fn the_cursors_row_is_found_under_a_header_and_not_in_a_stale_buffer() {
    let mut editor = Editor::default();
    editor.enter_concept("files/x".into(), None, "---\nkind: note\n---\none\ntwo\nthree");
    let mut h = Harness::new("", 40);
    h.editor = editor;
    h.edit().buf.set_cursor(5);
    let (fs, width) = (&mut h.fs, h.width);
    h.editor.reshape(&KeyBindings::defaults(), |spans| MarkdownPreview::new_plain_spans(fs, spans, width, 1.0));
    let (top, bottom) = h.editor.cursor_row().unwrap();
    assert!(top > 0.0 && bottom > top, "below the header's lines: {top}..{bottom}");
    // Text typed since the shaping: the buffer is not the edit's, so there is no row to follow.
    h.type_char("x");
    assert_eq!(h.editor.cursor_row(), None);
}
