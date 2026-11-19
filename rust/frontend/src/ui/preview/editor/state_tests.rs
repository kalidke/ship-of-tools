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
        let mut h = Self { fs, width: cell * cells as f32 + 0.5, editor };
        h.reshape();
        h
    }

    /// The window's re-shape after a key: the edit's spans shaped at the pane's width.
    fn reshape(&mut self) {
        let (fs, width) = (&mut self.fs, self.width);
        self.editor.reshape(&KeyBindings::defaults(), |spans| MarkdownPreview::new_plain_spans(fs, spans, width, 1.0));
    }

    /// The cursor's place as the window draws it, from a fresh re-shape.
    fn place(&mut self) -> rows::Place {
        self.reshape();
        self.editor.cursor_place().expect("the cursor has a place")
    }

    /// A concept edit: the frontmatter is the read-only header above the body.
    fn concept(content: &str, cells: usize) -> Self {
        let mut h = Self::new("", cells);
        h.editor = Editor::default();
        h.editor.enter_concept("files/x".into(), None, content);
        h.reshape();
        h
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
        self.edit().goal = None;
    }

    fn press(&mut self, logical: Key, action: Option<Action>, shift: bool, page_rows: usize) {
        let pressed = Pressed { logical: &logical, action, ctrl: false, alt: false, shift, super_: false };
        let edit = self.editor.edit.as_mut().unwrap();
        edit_buffer_key(edit, pressed, page_rows, self.editor.shaped.as_ref());
        self.reshape();
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
    h.reshape();
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
    h.reshape();
    assert!(h.rows_of_line(0) >= 4);
    h.act(Action::EditDown);
    let at = h.cursor();
    assert!(at > 0 && at < PARAGRAPH.len(), "the first Down stays in the paragraph, got {at}");
}

#[test]
fn a_word_without_spaces_wraps_by_glyph_and_down_keeps_its_column() {
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
fn a_lone_cr_or_separator_ends_a_shaped_row_and_is_walked_by_rows() {
    for sep in ["\r", "\u{2028}", "\u{85}", "\u{2029}"] {
        let first = format!("aaaa{sep}bbbb");
        let mut h = Harness::new(&format!("{first}\ncccc"), 40);
        h.put(2);
        h.act(Action::EditDown);
        assert_eq!(h.cursor(), 4 + sep.len() + 2, "separator {sep:?}: the shaper's next row, same column");
        h.act(Action::EditDown);
        assert_eq!(h.cursor(), first.len() + 1 + 2, "separator {sep:?}: the file's next line");
        h.act(Action::EditUp);
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
        let place = h.editor.cursor_place().expect("the cursor's row is in the layout");
        let from = self.view.scroll_px();
        place.top >= from && place.top + place.height <= from + self.visible
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
    let place = h.editor.cursor_place().unwrap();
    let bottom = place.top + place.height;
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
    h.reshape();
    h.edit().buf.set_cursor(5);
    let place = h.editor.cursor_place().unwrap();
    assert!(place.top > 0.0 && place.height > 0.0, "below the header's lines: {place:?}");
    // Text typed since the shaping: the buffer is not the edit's, so there is no row to follow.
    h.edit().buf.insert_char('x');
    assert_eq!(h.editor.cursor_place(), None);
}

#[test]
fn the_edit_text_holds_no_cursor_mark_so_its_rows_are_the_bodys_own() {
    let mut h = Harness::new("one\ntwo words here", 40);
    h.put(5);
    h.reshape();
    let shaped = h.editor.shaped().unwrap();
    let lines: Vec<&str> = shaped.buffer.lines.iter().map(|l| l.text()).collect();
    assert_eq!(&lines[..2], ["one", "two words here"]);
    assert!(!lines.iter().any(|l| l.contains('\u{2588}')), "{lines:?}");
}

#[test]
fn the_cursor_is_drawn_at_the_end_of_a_row_of_the_text_and_on_an_empty_line_and_a_wide_or_joined_character() {
    // The end of the text, and a byte inside a joined character's cluster (e and a combining accent).
    let mut h = Harness::new("ab\ne\u{301}x", 40);
    let cell = h.place().x; // at 0
    assert_eq!(cell, 0.0);
    h.put(2);
    let (end_of_ab, top_ab) = (h.place().x, h.place().top);
    assert!(end_of_ab > 0.0);
    h.put(3); // start of the second line
    let second = h.place();
    assert!(second.top > top_ab && second.x == 0.0);
    h.put(4); // inside the cluster: at its left edge
    assert_eq!(h.place(), second);
    h.put(6); // the end of the text
    assert!(h.place().x > 0.0 && h.place().top == second.top);
    // An empty line in the middle has a row of its own, and a wide character takes more than one cell.
    let mut h = Harness::new("a\n\nb\n日本", 40);
    h.put(2);
    let empty = h.place();
    assert_eq!(empty.x, 0.0);
    h.put(0);
    assert!(empty.top > h.place().top);
    h.put(5);
    let (wide, after) = (h.place(), { h.put(8); h.place() });
    assert!(after.x > wide.x * 1.5 + 1.0 && after.top == wide.top, "{wide:?} {after:?}");
}

#[test]
fn the_cursor_at_a_wrap_stands_at_the_end_of_the_row_before_the_dropped_space_and_at_the_start_of_the_next() {
    let mut h = Harness::new(PARAGRAPH, 24);
    h.reshape();
    let runs: Vec<_> = h.editor.shaped().unwrap().buffer.layout_runs().take(2).map(|r| (r.line_top, r.glyphs.last().map(|g| (g.end, g.x + g.w)), r.glyphs.first().map(|g| g.start))).collect();
    let (end0, x_end) = runs[0].1.unwrap();
    let start1 = runs[1].2.unwrap();
    assert!(start1 > end0, "the wrap drops a space: row one ends at {end0}, row two starts at {start1}");
    h.put(end0);
    let at_end = h.place();
    assert_eq!((at_end.top, at_end.x), (runs[0].0, x_end));
    h.put(start1);
    let at_start = h.place();
    assert_eq!((at_start.top, at_start.x), (runs[1].0, 0.0));
    h.put(end0 + 1); // the dropped space itself: the cursor after it is the next row's start
    assert_eq!(h.place(), at_start);
}

#[test]
fn a_crlf_line_and_a_lone_cr_have_places_and_the_next_line_is_found_past_them() {
    let mut h = Harness::new("ab\r\ncd\r\nef", 40);
    h.put(0);
    let first = h.place();
    h.put(2);
    assert!(h.place().x > 0.0 && h.place().top == first.top, "before the CR is the line's end");
    h.put(4);
    let second = h.place();
    h.put(8);
    let third = h.place();
    assert!(second.top > first.top && third.top > second.top && second.x == 0.0);
    // A lone CR makes the shaper end a line there: the byte after it is on the next shaped row.
    let mut h = Harness::new("ab\rcd\nef", 40);
    h.put(0);
    let first = h.place();
    h.put(3);
    assert!(h.place().top > first.top && h.place().x == 0.0);
    h.put(6);
    assert!(h.place().top > first.top);
}

#[test]
fn the_drawn_cursor_moves_one_row_for_each_down_from_every_start_column() {
    let para = format!("{PARAGRAPH} {PARAGRAPH} {PARAGRAPH} {PARAGRAPH}");
    for cells in [40usize, 23] {
        for col in 0..cells {
            let mut h = Harness::new(&para, cells);
            h.put(col);
            let mut last = h.place();
            while h.cursor() < para.len() {
                h.act(Action::EditDown);
                let now = h.place();
                let rows = ((now.top - last.top) / last.height).round() as i32;
                assert!(rows == 1 || h.cursor() == para.len(), "{cells} cells, start {col}: the drawn cursor moved {rows} rows to byte {}", h.cursor());
                last = now;
            }
        }
    }
}

#[test]
fn right_to_the_end_of_a_wrapped_row_then_down_and_up_stay_in_the_paragraph() {
    let content = format!("before\n\n{PARAGRAPH}\n\nafter");
    let mut h = Harness::new(&content, 24);
    let start = 8;
    h.put(start);
    h.reshape();
    let rows: Vec<_> = h.editor.shaped().unwrap().buffer.layout_runs().filter(|r| r.line_i == 2)
        .map(|r| (r.line_top, r.glyphs.first().unwrap().start, r.glyphs.last().unwrap().end)).collect();
    let (end0, start1) = (start + rows[0].2, start + rows[1].1);
    assert!(start1 > end0, "a space is dropped where the row wraps");
    // Right along the row to its end: the byte before the dropped space.
    while h.cursor() < end0 {
        h.act(Action::EditRight);
    }
    assert_eq!(h.place().top, rows[0].0);
    h.act(Action::EditDown);
    let down = h.cursor();
    assert!(down > start1 && down < start + PARAGRAPH.len(), "Down from the end of a row stays in the paragraph: {down}");
    assert_eq!(h.place().top, rows[1].0);
    h.act(Action::EditUp);
    assert_eq!(h.cursor(), end0, "Up returns to the end of the row");
    // Left from the next row's start is the same place.
    h.put(start1);
    h.act(Action::EditLeft);
    h.act(Action::EditLeft);
    h.act(Action::EditRight);
    assert_eq!(h.cursor(), end0);
    h.act(Action::EditUp);
    assert_eq!(h.cursor(), 7, "Up from the first row of the paragraph goes to the blank line above");
    h.put(end0);
    h.act(Action::EditUp);
    assert_eq!(h.cursor(), 7);
}

#[test]
fn a_step_from_inside_a_joined_character_moves_one_row() {
    // `e` and a combining accent are one glyph: the byte between them is inside it.
    let text = "cafe\u{301} ".repeat(12);
    let mut h = Harness::new(&text, 20);
    h.put(4);
    let first = h.place();
    h.act(Action::EditDown);
    assert!(h.cursor() > 4);
    let after = h.place();
    assert!(after.top > first.top && after.top < first.top + 1.5 * first.height, "one row: {first:?} -> {after:?}");
}

const LOREM: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat.";

/// The texts every walk goes over.
fn walk_texts() -> Vec<(&'static str, String)> {
    let md = format!("# Title\n\n{LOREM}\n\n{LOREM} {LOREM}\n\nShort line.\n\n{LOREM}\n");
    let list = format!("- {LOREM}\n- second item that is short\n  - nested item {LOREM}\n1. numbered {LOREM}\n");
    let code = "```rust\nfn main() { let x = vec![1, 2, 3]; let y: Vec<_> = x.iter().map(|v| v * 2).filter(|v| *v > 2).collect(); println!(\"{:?}\", y); }\n    // a comment that is quite long and goes past the edge of the pane for sure, yes indeed\n```\n".to_string();
    let table = "| column one | column two | column three is long |\n|---|---|---|\n| a cell with words in it | another cell with more words | the third cell that wraps past the pane edge |\n".to_string();
    let nospace = format!("see https://example.org/{}/end\nnext", "abcdefghij".repeat(12));
    let tabs = "\tone\ttwo\tthree\tfour\tfive\tsix\tseven\teight\tnine\tten\televen\ttwelve\n\t\tdeep\tline\n".to_string();
    let cjk = "日本語のテキストはスペースなしで折り返されます。これは長い段落で、ペインの幅を超えて何行にも折り返されるはずです。中文也一样，没有空格。\nnext".to_string();
    let comb = "cafe\u{301} na\u{ef}ve re\u{301}sume\u{301} cre\u{300}me bru\u{302}le\u{301}e ".repeat(6);
    let emoji = "hello 👋 world 🌍 love ❤️ family 👩‍👩‍👧 flag 🇺🇸 thumbs 👍🏽 done. ".repeat(4);
    let crlf = format!("# Title\r\n\r\n{LOREM}\r\n\r\n{LOREM}\r\n");
    let one_crlf = format!("{LOREM}\r\n{LOREM}\n{LOREM}");
    let lone_cr = format!("{LOREM}\rsecond part\n{LOREM}");
    vec![
        ("markdown", md), ("list", list), ("code", code), ("table", table), ("nospace", nospace), ("tabs", tabs),
        ("cjk", cjk), ("combining", comb), ("emoji", emoji), ("empty", String::new()), ("one_char", "a".into()),
        ("only_newlines", "\n\n\n\n".into()), ("crlf", crlf), ("one_crlf", one_crlf), ("lone_cr", lone_cr),
    ]
}

/// Down from the first byte to the end and Up back: every key moves the drawn cursor exactly one row, never
/// backward, never into the middle of a character.
fn walk(h: &mut Harness, what: &str) {
    for down in [true, false] {
        let body = h.edit().buf.body().to_string();
        let (action, end) = if down { (Action::EditDown, body.len()) } else { (Action::EditUp, 0) };
        h.put(if down { 0 } else { body.len() });
        for step in 0..10_000 {
            let (before, at) = (h.place(), h.cursor());
            h.act(action);
            let (now, to) = (h.place(), h.cursor());
            let rows = ((now.top - before.top) / before.height).round() as i32 * if down { 1 } else { -1 };
            assert!(body.is_char_boundary(to), "{what}: step {step} landed inside a character: {at} -> {to}");
            assert!(if down { to >= at } else { to <= at }, "{what}: step {step} went the wrong way: {at} -> {to}");
            assert!(rows == 1 || (to == end && rows == 0), "{what} ({}): step {step} moved the drawn cursor {rows} rows: {at} -> {to}", if down { "down" } else { "up" });
            if to == at {
                assert_eq!(to, end, "{what}: stuck at {at}");
                break;
            }
            assert!(step < 9_999, "{what}: no end");
        }
    }
}

#[test]
fn a_walk_down_and_up_moves_one_row_per_key_in_every_text() {
    for cells in [40usize, 23] {
        for (name, text) in walk_texts() {
            let mut h = Harness::new(&text, cells);
            walk(&mut h, &format!("{name} at {cells} cells"));
        }
        let front = format!("---\ntitle: a note\ntags: [x, y]\n---\n{LOREM}\n\n{LOREM}\n");
        walk(&mut Harness::concept(&front, cells), &format!("frontmatter at {cells} cells"));
        let in_file = format!("---\ntitle: a note\n---\n\n{LOREM}\n\n{LOREM}\n");
        walk(&mut Harness::new(&in_file, cells), &format!("frontmatter in a file at {cells} cells"));
    }
}

#[test]
fn a_row_with_right_to_left_text_is_stepped_by_the_files_lines() {
    // The second row of the paragraph holds a Hebrew word: its layout runs right to left, so a step onto it is the
    // file's next line, not a row.
    let content = format!("{}\nnext", "word ".repeat(7) + "שלום עולם " + &"word ".repeat(4));
    let mut h = Harness::new(&content, 20);
    h.put(2);
    h.act(Action::EditDown);
    assert!(h.cursor() > content.find('\n').unwrap(), "stepped by the file's lines: {}", h.cursor());
}

#[test]
fn a_reload_ends_the_kept_column() {
    let mut h = Harness::concept("---\nk: v\n---\nabcdefgh\nab\nabcdefgh", 40);
    h.put(5);
    h.act(Action::EditDown);
    assert!(h.edit().goal.is_some());
    h.editor.reload_concept(true, "---\nk: v\n---\nabcdefgh\nab\nabcdefgh");
    assert_eq!(h.edit().goal, None);
}

#[test]
fn the_kept_column_survives_a_change_of_scale() {
    let (a, b, c) = ("a".repeat(60), "b".repeat(10), "c".repeat(60));
    let body = format!("{a}\n{b}\n{c}");
    let shaped = |fs: &mut FontSystem, scale: f32| {
        let one = MarkdownPreview::new_plain_spans(fs, &[("m".into(), false)], 1000.0, scale);
        let width = one.buffer.layout_runs().next().unwrap().glyphs[0].w * 100.5;
        MarkdownPreview::new_plain_spans(fs, &[(body.clone(), false), ("\n\n-".into(), false)], width, scale)
    };
    let mut fs = FontSystem::new();
    let small = shaped(&mut fs, 1.0);
    let down = rows::vertical(&small.buffer, &body, 40, None, 1).unwrap();
    assert_eq!(down.cursor, 61 + 10);
    // The window zooms before the next Down: the kept column is still column 40.
    let large = shaped(&mut fs, 1.5);
    let again = rows::vertical(&large.buffer, &body, down.cursor, Some(down.goal), 1).unwrap();
    assert_eq!(again.cursor, 72 + 40);
}
