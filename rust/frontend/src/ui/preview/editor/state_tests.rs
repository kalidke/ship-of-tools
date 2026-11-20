//! Up, Down, PageUp and PageDown in edit mode move by the row on screen. A winit key event cannot be built outside the
//! event loop, so the tests press through `edit_buffer_key`, which `Editor::key` calls for every key the buffer takes.

use super::*;
use cosmic_text::FontSystem;
use std::time::{Duration, Instant};

/// Font systems kept between this binary's tests: making one reads the computer's fonts (about 30 ms in a debug build),
/// so a test takes one from here, or makes one when none is free, and gives it back when it ends.
static FONTS: std::sync::Mutex<Vec<FontSystem>> = std::sync::Mutex::new(Vec::new());

/// A font system from `FONTS`, given back when dropped.
struct Fonts(Option<FontSystem>);

impl Fonts {
    fn take() -> Self {
        Self(Some(FONTS.lock().unwrap_or_else(|e| e.into_inner()).pop().unwrap_or_else(FontSystem::new)))
    }
}

impl std::ops::Deref for Fonts {
    type Target = FontSystem;
    fn deref(&self) -> &FontSystem { self.0.as_ref().unwrap() }
}

impl std::ops::DerefMut for Fonts {
    fn deref_mut(&mut self) -> &mut FontSystem { self.0.as_mut().unwrap() }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        FONTS.lock().unwrap_or_else(|e| e.into_inner()).extend(self.0.take());
    }
}

/// An editor over `content`, shaped at `cells` monospace cells of width.
struct Harness {
    fs: Fonts,
    width: f32,
    scale: f32,
    editor: Editor,
}

impl Harness {
    fn new(content: &str, cells: usize) -> Self {
        let mut fs = Fonts::take();
        let one = MarkdownPreview::new_plain_spans(&mut fs, &["m".into()], 1000.0, 1.0);
        let cell = one.buffer.layout_runs().next().unwrap().glyphs[0].w;
        let mut editor = Editor::default();
        editor.arm_file("files:x".into());
        editor.open_file("files:x", true, content.to_string(), "v".into());
        let mut h = Self { fs, width: cell * cells as f32 + 0.5, scale: 1.0, editor };
        h.reshape();
        h
    }

    /// The same harness on another text. A font system is slow to make (it reads the computer's fonts), so a test of
    /// many texts makes one harness and gives it each text in turn.
    fn retext(&mut self, content: &str) {
        self.editor = Editor::default();
        self.editor.arm_file("files:x".into());
        self.editor.open_file("files:x", true, content.to_string(), "v".into());
        self.reshape();
    }

    /// The window's re-shape after a key: the edit's spans shaped at the pane's width.
    fn reshape(&mut self) {
        self.editor.reshape(&KeyBindings::defaults(), self.width, self.scale, &mut self.fs);
    }

    /// The cursor's place as the window draws it: re-shaped first only when the window would (a motion owes none).
    fn place(&mut self) -> rows::Place {
        if self.editor.needs_reshape() {
            self.reshape();
        }
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

    /// A key pressed at the editor as the window does: the editor's outcome, then the re-shape if it is owed.
    fn press_key(&mut self, action: Option<Action>, typed: Option<&str>) -> Option<EditorKey> {
        let outcome = self.editor.press_for_test(action, typed, 3);
        if self.editor.owes_shaping(self.width) {
            self.reshape();
        }
        outcome
    }

    /// The last line the edit draws: its footer.
    fn footer(&mut self) -> String {
        self.editor.shaped().unwrap().buffer.lines.last().unwrap().text().to_string()
    }

    fn edit(&mut self) -> &mut EditState {
        self.editor.edit.as_mut().unwrap()
    }

    fn body(&mut self) -> String {
        self.edit().buf.body().to_string()
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
        edit_buffer_key(edit, pressed, page_rows, self.editor.shaped.as_ref().zip(self.editor.index.as_ref()));
        // The window re-shapes after a key only when the shaped text is no longer what the edit draws.
        if self.editor.needs_reshape() {
            self.reshape();
        }
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
        let shaped = MarkdownPreview::new_plain_spans(&mut self.fs, &[body], self.width, 1.0);
        shaped.buffer.layout_runs().filter(|r| r.line_i == line).count()
    }
}

const PARAGRAPH: &str = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau";

#[test]
fn a_long_texts_shaped_rows_reach_its_last_line() {
    let mut fs = Fonts::take();
    let body = (0..1000).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
    let shaped = MarkdownPreview::new_plain_spans(&mut fs, &[body], 400.0, 1.0);
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

/// Highlighters kept between this binary's tests, as `FONTS` keeps font systems: making one compiles six grammars'
/// queries (about 0.35 s in a debug build), and a pane needs one but never highlights here.
static HIGHLIGHTS: std::sync::Mutex<Vec<crate::ui::preview::markdown::highlight::HighlightService>> = std::sync::Mutex::new(Vec::new());

/// A pane's preview, its highlighter given back to `HIGHLIGHTS` when dropped.
struct View(Option<ShapedPreview>);

impl std::ops::Deref for View {
    type Target = ShapedPreview;
    fn deref(&self) -> &ShapedPreview { self.0.as_ref().unwrap() }
}

impl std::ops::DerefMut for View {
    fn deref_mut(&mut self) -> &mut ShapedPreview { self.0.as_mut().unwrap() }
}

impl Drop for View {
    fn drop(&mut self) {
        HIGHLIGHTS.lock().unwrap_or_else(|e| e.into_inner()).extend(self.0.take().map(ShapedPreview::into_highlight));
    }
}

/// A pane over the editor: a preview whose line is taller than the editor's (prose, 22 px against 19), `visible` px
/// of text high, which each cursor move scrolls as the window's key path does and then lays out as a frame does.
struct Pane {
    view: View,
    visible: f32,
}

impl Pane {
    fn new(h: &mut Harness, visible: f32) -> Self {
        let highlight = HIGHLIGHTS.lock().unwrap_or_else(|e| e.into_inner()).pop()
            .unwrap_or_else(|| crate::ui::preview::markdown::highlight::HighlightService::new().unwrap());
        let md = MarkdownPreview::new(&mut h.fs, "text", 400.0, visible, 1.0, &Default::default(), &Default::default(),
            &highlight, &Default::default(), false);
        let rect = ScreenRect { x: 0.0, y: 0.0, w: 400.0, h: visible };
        assert!(md.line_height() > 19.0, "the preview's line must be taller than the editor's");
        Self { view: View(Some(ShapedPreview::new(md, highlight, rect))), visible }
    }

    /// The same pane `visible` px high, scrolled to the top.
    fn resize(&mut self, h: &mut Harness, visible: f32) {
        self.view.layout(ScreenRect { x: 0.0, y: 0.0, w: 400.0, h: visible }, &mut h.fs);
        self.view.scroll_to_top(Instant::now(), Duration::ZERO);
        self.visible = visible;
    }

    /// The key path's own steps after a key: re-shape, follow, then the frame's clamp.
    fn after_key(&mut self, h: &mut Harness) {
        h.reshape();
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
        let mut h = Harness::new(&para, cells);
        for col in 0..cells {
            h.retext(&para);
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
        ("cr_only_file", format!("{LOREM}\rsecond line\rthird line\r")), ("final_lone_cr", format!("{LOREM}\n\n{LOREM}\r")),
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
        let mut h = Harness::new("", cells);
        for (name, text) in walk_texts() {
            h.retext(&text);
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
        let one = MarkdownPreview::new_plain_spans(fs, &["m".into()], 1000.0, scale);
        let width = one.buffer.layout_runs().next().unwrap().glyphs[0].w * 100.5;
        MarkdownPreview::new_plain_spans(fs, &[body.clone(), "\n\n-".into()], width, scale)
    };
    let mut fs = Fonts::take();
    let small = shaped(&mut fs, 1.0);
    let index = rows::Index::new(&small.buffer, &body, 7).unwrap();
    let down = rows::vertical(&small.buffer, &index, &body, 7, 40, None, 1).unwrap();
    assert_eq!(down.cursor, 61 + 10);
    // The window zooms before the next Down: the kept column is still column 40.
    let large = shaped(&mut fs, 1.5);
    let index = rows::Index::new(&large.buffer, &body, 7).unwrap();
    let again = rows::vertical(&large.buffer, &index, &body, 7, down.cursor, Some(down.goal), 1).unwrap();
    assert_eq!(again.cursor, 72 + 40);
}

#[test]
fn a_crlf_file_is_followed_row_by_row() {
    let text = (0..80).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\r\n");
    let mut h = Harness::new(&text, 40);
    let mut pane = Pane::new(&mut h, 200.0);
    pane.after_key(&mut h);
    for _ in 0..40 {
        h.act(Action::EditDown);
        pane.after_key(&mut h);
        assert!(pane.shows_cursor(&h), "the cursor is out of view at {}", h.cursor());
    }
    assert!(pane.view.scroll() > 0.0, "the view followed the cursor down the file");
}

#[test]
fn a_pane_shorter_than_two_rows_does_not_flip_the_view_on_any_row() {
    // The editor's rows are 19 px and the preview scrolls in 22 px lines: rows 1 to 9 start off the preview's
    // grid, and a pane of 14, 22, 30 or 37 px cannot hold some of them whole at any scroll.
    let text = (0..12).map(|i| format!("row {i} of text here")).collect::<Vec<_>>().join("\n");
    let mut h = Harness::new(&text, 40);
    let mut pane = Pane::new(&mut h, 2.0);
    for visible in [2.0, 14.0, 22.0, 30.0, 37.0] {
        h.retext(&text);
        pane.resize(&mut h, visible);
        pane.after_key(&mut h);
        for row in 0..10 {
            h.put(text.split('\n').take(row).map(|l| l.len() + 1).sum::<usize>());
            pane.after_key(&mut h);
            let mut seen = Vec::new();
            for _ in 0..4 {
                h.act(Action::EditRight);
                pane.after_key(&mut h);
                seen.push(pane.view.scroll());
            }
            assert!(seen.windows(2).all(|w| w[0] == w[1]), "a {visible} px pane, row {row}: {seen:?}");
        }
    }
}

#[test]
fn a_follow_counts_the_distance_a_wheel_scroll_still_owes() {
    let mut h = Harness::new(&long_text(), 40);
    let mut pane = Pane::new(&mut h, 200.0);
    pane.after_key(&mut h);
    // The wheel asks for 40 lines over 150 ms; the cursor stands at the top, in view where the view is now.
    let t0 = Instant::now();
    pane.view.scroll_by(40, t0, Duration::from_millis(150));
    h.act(Action::EditRight);
    h.reshape();
    h.editor.follow_cursor(&mut pane.view, pane.visible, t0, Duration::ZERO);
    let line_h = pane.view.md().line_height();
    let max = crate::ui::preview::pane::preview_max_scroll(line_h, pane.visible, h.editor.shaped().unwrap());
    pane.view.advance(t0 + Duration::from_secs(2), max, pane.visible / line_h, true);
    assert!(pane.shows_cursor(&h), "the wheel's owed lines took the view from the cursor");
}

/// The shaped lines the window draws are the header's, the body's and the footer's two: a selection cut anywhere
/// must not add one.
fn shaped_lines_are_the_bodys(h: &mut Harness, header_lines: usize) {
    let body_lines = h.edit().buf.body().split('\n').count();
    let shaped = h.editor.shaped().unwrap().buffer.lines.len();
    assert_eq!(shaped, header_lines + body_lines + 2, "the drawn text has {shaped} lines for {body_lines} body lines");
}

#[test]
fn no_motion_in_a_crlf_file_rests_inside_a_line_end() {
    let text = "ab\r\ncd\r\n\r\nefgh\r\nx";
    let mut h = Harness::new(text, 40);
    let inside = |h: &mut Harness| {
        let (body, at) = (h.edit().buf.body().to_string(), h.cursor());
        assert!(!(at > 0 && body.as_bytes().get(at - 1) == Some(&b'\r') && body.as_bytes().get(at) == Some(&b'\n')), "cursor {at} is inside a line end");
        if let Some((a, b)) = h.edit().buf.selection_range() {
            for edge in [a, b] {
                assert!(!(edge > 0 && body.as_bytes().get(edge - 1) == Some(&b'\r') && body.as_bytes().get(edge) == Some(&b'\n')), "selection edge {edge} is inside a line end");
            }
        }
    };
    let actions = [Action::EditLeft, Action::EditRight, Action::EditUp, Action::EditDown, Action::EditHome, Action::EditEnd,
        Action::EditPageUp, Action::EditPageDown, Action::EditStart, Action::EditFinish];
    for start in (0..=text.len()).filter(|&i| !(i > 0 && text.as_bytes()[i - 1] == b'\r' && text.as_bytes().get(i) == Some(&b'\n'))) {
        for action in actions {
            for shift in [false, true] {
                h.put(start);
                h.edit().buf.clear_selection();
                h.press(Key::Named(NamedKey::ArrowDown), Some(action), shift, 3);
                inside(&mut h);
                shaped_lines_are_the_bodys(&mut h, 0);
                h.press(Key::Named(NamedKey::ArrowDown), Some(action), shift, 3);
                inside(&mut h);
            }
        }
    }
    // Right and Left cross a line end whole; End stops before the CR.
    h.put(2);
    h.act(Action::EditRight);
    assert_eq!(h.cursor(), 4);
    h.act(Action::EditLeft);
    assert_eq!(h.cursor(), 2);
    h.act(Action::EditEnd);
    assert_eq!(h.cursor(), 2);
}

#[test]
fn deleting_a_crlf_takes_both_characters_and_never_leaves_the_cursor_inside_one() {
    let mut h = Harness::new("ab\r\ncd", 40);
    h.put(4);
    h.press(Key::Named(NamedKey::Backspace), Some(Action::EditBackspace), false, 3);
    assert_eq!((h.body(), h.cursor()), ("abcd".to_string(), 2));
    let mut h = Harness::new("ab\r\ncd", 40);
    h.put(2);
    h.press(Key::Named(NamedKey::Delete), Some(Action::EditDelete), false, 3);
    assert_eq!((h.body(), h.cursor()), ("abcd".to_string(), 2));
    // Removing what stood between a CR and an LF brings them together: the cursor is before the pair.
    let mut h = Harness::new("a\rX\nb", 40);
    h.put(3);
    h.press(Key::Named(NamedKey::Backspace), Some(Action::EditBackspace), false, 3);
    assert_eq!((h.body(), h.cursor()), ("a\r\nb".to_string(), 1));
}

#[test]
fn a_selection_that_ends_at_a_crlf_line_end_adds_no_line_to_the_drawn_text() {
    let text = format!("# Title\r\n\r\n{LOREM}\r\n\r\n{LOREM}\r\n");
    let second = text.find(LOREM).unwrap() + LOREM.len() + "\r\n\r\n".len();
    let mut h = Harness::new(&text, 40);
    h.put(second);
    h.act(Action::EditEnd);
    let mut last = h.place();
    shaped_lines_are_the_bodys_own_after(&mut h);
    for _ in 0..3 {
        h.press(Key::Named(NamedKey::ArrowUp), Some(Action::EditUp), true, 3);
        shaped_lines_are_the_bodys_own_after(&mut h);
        let now = h.place();
        assert!(now.top < last.top, "Shift+Up moved the bar up a row: {last:?} -> {now:?}");
        assert!(((last.top - now.top) / now.height).round() as i32 == 1, "one row: {last:?} -> {now:?}");
        last = now;
    }
    // Shift+End alone, and Shift+Right across lines.
    let mut h = Harness::new(&text, 40);
    h.put(second);
    h.press(Key::Named(NamedKey::End), Some(Action::EditEnd), true, 3);
    shaped_lines_are_the_bodys_own_after(&mut h);
    assert!(h.editor.cursor_place().is_some());
    let mut h = Harness::new("ab\r\ncd\r\nef", 40);
    for _ in 0..8 {
        h.press(Key::Named(NamedKey::ArrowRight), Some(Action::EditRight), true, 3);
        shaped_lines_are_the_bodys_own_after(&mut h);
        assert!(h.editor.cursor_place().is_some(), "a bar at {}", h.cursor());
    }
}

fn shaped_lines_are_the_bodys_own_after(h: &mut Harness) {
    shaped_lines_are_the_bodys(h, 0);
    assert!(h.editor.cursor_place().is_some(), "no bar at {}", h.cursor());
}

#[test]
fn a_selection_drawn_across_crlf_lines_maps_every_row() {
    let text = "ab\r\ncd\r\nef\r\ngh";
    let mut h = Harness::new(text, 40);
    let mut tops = Vec::new();
    for _ in 0..3 {
        h.press(Key::Named(NamedKey::ArrowDown), Some(Action::EditDown), true, 3);
        shaped_lines_are_the_bodys_own_after(&mut h);
        tops.push(h.place().top);
    }
    assert!(tops.windows(2).all(|w| w[1] > w[0]), "{tops:?}");
    // The tint is the selected run only: the rows above it keep their text.
    let lines: Vec<&str> = h.editor.shaped().unwrap().buffer.lines.iter().map(|l| l.text()).collect();
    assert_eq!(&lines[..4], ["ab", "cd", "ef", "gh"]);
}

const HEBREW: &str = "שלום עולם";
const ARABIC: &str = "مرحبا بالعالم";

#[test]
fn the_bar_is_drawn_on_a_right_to_left_row_at_the_glyphs_leading_edge() {
    for (name, line) in [("hebrew", HEBREW), ("arabic", ARABIC)] {
        let mut h = Harness::new(&format!("{line}\nafter"), 40);
        let mut xs = Vec::new();
        for at in line.char_indices().map(|(i, _)| i).chain([line.len()]) {
            h.put(at);
            let place = h.editor.cursor_place().unwrap_or_else(|| panic!("{name}: no bar at {at}"));
            xs.push(place.x);
        }
        // Reading goes right to left: the start of the line is its right end, the end of it the left.
        assert!(xs[0] > xs[xs.len() - 1] + 1.0, "{name}: {xs:?}");
        assert!(xs.windows(2).all(|w| w[1] <= w[0] + 0.01), "{name}: later bytes stand further left: {xs:?}");
        // The next line's bar is on the next row.
        h.put(line.len() + 1);
        assert!(h.editor.cursor_place().unwrap().top > h.place().top - 1.0);
    }
}

#[test]
fn the_bar_is_drawn_on_a_mixed_row_at_every_byte_and_at_both_ends() {
    let line = format!("abc {HEBREW} def");
    let mut h = Harness::new(&line, 40);
    let mut xs = Vec::new();
    for at in line.char_indices().map(|(i, _)| i).chain([line.len()]) {
        h.put(at);
        xs.push(h.editor.cursor_place().unwrap_or_else(|| panic!("no bar at {at}")).x);
    }
    assert_eq!(xs[0], 0.0, "the start of the row");
    let end = xs[xs.len() - 1];
    assert!(end > xs[xs.len() - 2], "the end of the row is right of its last letter");
    // Inside the Hebrew word the bar runs right to left.
    let (from, to) = (line.find('ש').unwrap(), line.find(" def").unwrap());
    let inside: Vec<f32> = line.char_indices().filter(|(i, _)| (from..to).contains(i)).map(|(i, _)| xs[line[..i].chars().count()]).collect();
    assert!(inside.windows(2).all(|w| w[1] < w[0] + 0.01), "{inside:?}");
    // A paragraph that wraps with one Hebrew word keeps a bar at every byte.
    let para = format!("{} {HEBREW} {}", "word ".repeat(7), "word ".repeat(7));
    let mut h = Harness::new(&para, 20);
    for at in para.char_indices().map(|(i, _)| i).chain([para.len()]) {
        h.put(at);
        assert!(h.editor.cursor_place().is_some(), "no bar at {at}");
    }
}

fn place_at(top: f32) -> rows::Place {
    rows::Place { x: 100.0, top, height: 19.0 }
}

#[test]
fn the_bar_is_clipped_as_the_text_it_stands_on_is() {
    let pane = ScreenRect { x: 50.0, y: 40.0, w: 300.0, h: 100.0 };
    let pad = crate::ui::render::text::EXTRA_TOP_PAD_PX;
    // The buffer is drawn from pane.y + pad - scroll; its text is cut at the pane's top, at its right edge and at the
    // foot of the last whole line.
    let clip_bottom = pane.y + pad + 95.0;
    let rect = |place, scroll| rows::bar(place, pane, scroll, clip_bottom);
    // A row in the middle: the bar's vertical extent is the row's, from the same origin as the text.
    let mid = rect(place_at(38.0), 0.0).unwrap();
    assert_eq!((mid.y, mid.h), (pane.y + pad + 38.0, 19.0));
    assert!(mid.x < pane.x + 100.0 && mid.x + mid.w > pane.x + 100.0, "centered on its place");
    // At the pane's top edge: a row whose top is at pane.y is whole, however far it is above the padding.
    let top = rect(place_at(-pad), 0.0).unwrap();
    assert_eq!((top.y, top.h), (pane.y, 19.0));
    // Partly cut at the top: the part above the pane is not drawn.
    let cut_top = rect(place_at(-pad - 5.0), 0.0).unwrap();
    assert_eq!((cut_top.y, cut_top.h), (pane.y, 14.0));
    assert!(rect(place_at(-pad - 19.0), 0.0).is_none(), "a row wholly above");
    // At the bottom edge: a row that ends at the text's clip is whole; one that crosses it is cut there.
    let foot = rect(place_at(95.0 - 19.0), 0.0).unwrap();
    assert_eq!((foot.y + foot.h, foot.h), (clip_bottom, 19.0));
    let cut_foot = rect(place_at(95.0 - 10.0), 0.0).unwrap();
    assert_eq!((cut_foot.y + cut_foot.h, cut_foot.h), (clip_bottom, 10.0));
    assert!(rect(place_at(95.0), 0.0).is_none(), "a row wholly below");
    // A scrolled view moves the row with the text.
    assert_eq!(rect(place_at(138.0), 100.0).unwrap().y, mid.y);
    // At the pane's right edge and its left the bar is whole, moved in to the edge, and never outside the pane;
    // a bar's width past either edge, nothing of the text is there.
    let right = rows::bar(rows::Place { x: pane.w, ..place_at(10.0) }, pane, 0.0, clip_bottom).unwrap();
    assert_eq!((right.x + right.w, right.w), (pane.x + pane.w, 2.28));
    let left = rows::bar(rows::Place { x: 0.0, ..place_at(10.0) }, pane, 0.0, clip_bottom).unwrap();
    assert_eq!((left.x, left.w), (pane.x, 2.28), "whole at column 0");
    assert!(rows::bar(rows::Place { x: pane.w + 5.0, ..place_at(10.0) }, pane, 0.0, clip_bottom).is_none());
    assert!(rows::bar(rows::Place { x: -5.0, ..place_at(10.0) }, pane, 0.0, clip_bottom).is_none());
}

#[test]
fn the_row_list_is_built_once_per_shaping_and_not_by_a_key_a_frame_or_a_follow() {
    let mut h = Harness::new(&long_text(), 40);
    let mut pane = Pane::new(&mut h, 200.0);
    let built = || rows::BUILDS.with(|b| b.get());
    let before = built();
    h.reshape();
    assert_eq!(built() - before, 1, "one build for each shaping");
    // 40 keys with the window's work between them except the re-shape (a cursor move leaves the text, so the layout
    // stands): the step, the follow's place and the frame's place read the one list.
    let before = built();
    let logical = Key::Named(NamedKey::ArrowDown);
    for _ in 0..40 {
        let pressed = Pressed { logical: &logical, action: Some(Action::EditDown), ctrl: false, alt: false, shift: false, super_: false };
        let edit = h.editor.edit.as_mut().unwrap();
        edit_buffer_key(edit, pressed, 3, h.editor.shaped.as_ref().zip(h.editor.index.as_ref()));
        h.editor.follow_cursor(&mut pane.view, pane.visible, Instant::now(), Duration::ZERO);
        let _ = h.editor.cursor_place();
    }
    assert_eq!(built() - before, 0, "no key, follow or bar built a row list");
    assert!(h.cursor() > 0, "the keys moved the cursor");
}

#[test]
fn a_text_changed_to_one_of_the_same_length_since_the_shaping_has_no_place_and_no_step() {
    let mut h = Harness::new("abcd\nefgh\nijkl", 40);
    h.put(1);
    assert!(h.editor.cursor_place().is_some());
    // Overtype one character: the same lengths, a different text, not shaped again.
    h.edit().buf.set_selecting(true);
    h.edit().buf.move_right();
    h.edit().buf.insert_char('X');
    assert_eq!(h.body(), "aXcd\nefgh\nijkl");
    assert_eq!(h.editor.cursor_place(), None, "the layout is of the text before the overtyping");
    let (body, rev, cursor) = { let e = h.edit(); (e.buf.body().to_string(), e.buf.rev(), e.buf.cursor()) };
    let (shaped, index) = (h.editor.shaped.as_ref().unwrap(), h.editor.index.as_ref().unwrap());
    assert!(rows::vertical(&shaped.buffer, index, &body, rev, cursor, None, 1).is_none());
    // Shaped again, it has both.
    h.reshape();
    assert!(h.editor.cursor_place().is_some());
}

#[test]
fn the_edit_buffer_fits_a_pane_by_width_by_scale_or_not_at_all() {
    let mut h = Harness::new("some text to shape", 40);
    let (w, scale) = (h.width, 1.0);
    assert_eq!(h.editor.fit(w, scale), Fit::Fits);
    assert_eq!(h.editor.fit(w + 0.2, scale), Fit::Width, "a resized pane is laid out again");
    assert_eq!(h.editor.fit(w, 1.5), Fit::Scale, "a zoom shapes again");
    assert_eq!(h.editor.fit(w + 0.2, 1.5), Fit::Scale);
    assert_eq!(Editor::default().fit(w, scale), Fit::Fits, "no shaping, nothing to redo");
    assert_eq!(h.editor.fit(0.0, scale), Fit::Fits, "a hidden pane is not fitted");
}

#[test]
fn a_new_pane_width_lays_the_buffer_out_again_and_the_rows_are_those_of_a_fresh_shaping() {
    let text = format!("{PARAGRAPH}\n\n{PARAGRAPH}\r\nshort\n{}", "x".repeat(70));
    let mut h = Harness::new(&text, 40);
    let mut fresh = Harness::new(&text, 23);
    let width = fresh.width;
    let builds = rows::BUILDS.with(|b| b.get());
    assert!(h.editor.relayout(width, &mut h.fs), "the text is what was shaped");
    assert_eq!(rows::BUILDS.with(|b| b.get()) - builds, 1, "one index, no second shaping");
    assert_eq!(h.editor.shaped().unwrap().buffer.size().0, Some(width));
    for at in positions(&text) {
        h.put(at);
        fresh.put(at);
        assert_eq!(h.editor.cursor_place(), fresh.editor.cursor_place(), "at {at}");
    }
    // The steps agree too.
    h.put(5);
    fresh.put(5);
    for _ in 0..12 {
        h.act(Action::EditDown);
        fresh.act(Action::EditDown);
        assert_eq!(h.cursor(), fresh.cursor());
    }
    // A text changed since it was shaped is not laid out: it is shaped again.
    h.edit().buf.insert_char('Z');
    assert!(!h.editor.relayout(width + 5.0, &mut h.fs));
}

#[test]
fn a_motion_with_no_selection_owes_no_reshape_and_a_selection_or_an_edit_does() {
    let mut h = Harness::new(&long_text(), 40);
    let built = || rows::BUILDS.with(|b| b.get());
    let before = built();
    for action in [Action::EditDown, Action::EditDown, Action::EditRight, Action::EditUp, Action::EditEnd, Action::EditHome,
        Action::EditPageDown, Action::EditPageUp, Action::EditFinish, Action::EditStart] {
        h.act(action);
        assert!(!h.editor.needs_reshape(), "{action:?}");
    }
    for _ in 0..40 {
        h.act(Action::EditDown);
    }
    assert_eq!(built() - before, 0, "40 held Downs and ten other motions shaped nothing");
    assert!(h.cursor() > 0 && h.editor.cursor_place().is_some());
    // A selection that begins changes the footer's words; one that grows changes nothing shaped (its bands are drawn
    // over the rows); dropping it changes the footer again; an edit changes the text.
    h.press(Key::Named(NamedKey::ArrowDown), Some(Action::EditDown), true, 3);
    assert_eq!(built() - before, 1);
    h.press(Key::Named(NamedKey::ArrowDown), Some(Action::EditDown), true, 3);
    assert!(!h.editor.needs_reshape());
    assert_eq!(built() - before, 1, "a selection that grows is drawn over the shaped text");
    h.act(Action::EditRight);
    assert_eq!(built() - before, 2, "dropping it re-shapes once");
    h.type_char("x");
    assert_eq!(built() - before, 3);
}

/// The bands `rows::selection` gives row by row, each `(x0, x1)`, for the selection `a..b` put in `h`.
fn bands_by_row(h: &mut Harness, a: usize, b: usize) -> Vec<(f32, Vec<(f32, f32)>)> {
    h.edit().buf.clear_selection();
    h.put(a);
    h.edit().buf.set_selecting(true);
    h.edit().buf.set_cursor(b);
    if h.editor.needs_reshape() {
        h.reshape();
    }
    let mut rows: Vec<(f32, Vec<(f32, f32)>)> = Vec::new();
    for (x, top, w, _) in h.editor.selection_bands(f32::MIN, f32::MAX) {
        match rows.last_mut() {
            Some((t, bands)) if *t == top => bands.push((x, x + w)),
            _ => rows.push((top, vec![(x, x + w)])),
        }
    }
    h.edit().buf.clear_selection();
    rows
}

#[test]
fn the_selection_is_drawn_as_bands_under_its_text_on_every_row_it_spans() {
    let content = format!("before\n\n{PARAGRAPH}\nabc\n{HEBREW}\nafter");
    let mut h = Harness::new(&content, 24);
    let em_half = h.editor.shaped().unwrap().buffer.metrics().font_size / 2.0;
    let x = |h: &mut Harness, at: usize| { h.put(at); h.place().x };
    let (para, abc) = (8, 9 + PARAGRAPH.len());
    // From inside the first line to inside the paragraph's third row: the rest of `before` and its line end, the empty
    // line's end, the paragraph's first two rows whole and its third up to the selection's end.
    assert!(h.rows_of_line(2) >= 4);
    let ends: Vec<(usize, usize)> = h.editor.shaped().unwrap().buffer.layout_runs().filter(|r| r.line_i == 2)
        .map(|r| (para + r.glyphs.iter().map(|g| g.start).min().unwrap(), para + r.glyphs.iter().map(|g| g.end).max().unwrap())).collect();
    let b = ends[2].0 + 3;
    let bands = bands_by_row(&mut h, 3, b);
    assert_eq!(bands.len(), 5, "{bands:?}");
    let (x3, x6) = (x(&mut h, 3), x(&mut h, 6));
    assert_eq!(bands[0].1, [(x3, x6), (x6, x6 + em_half)], "the rest of the first line and its end");
    assert_eq!(bands[1].1, [(0.0, em_half)], "the empty line's end");
    for (row, (s, e)) in ends.iter().take(2).enumerate() {
        let (x0, x1) = (x(&mut h, *s), x(&mut h, *e));
        assert_eq!(bands[2 + row].1, [(x0, x1)], "the paragraph's row {row} whole");
    }
    let (x0, x1) = (x(&mut h, ends[2].0), x(&mut h, b));
    assert_eq!(bands[4].1, [(x0, x1)], "the third row up to the selection's end");
    // A selection that ends at a line end has no band for it.
    let bands = bands_by_row(&mut h, abc, abc + 3);
    assert_eq!(bands.len(), 1);
    assert_eq!(bands[0].1, [(0.0, x(&mut h, abc + 3))]);
    // A right-to-left word: its band runs from the word's end, at the left, to its start, at the right.
    let (heb, word) = (abc + 4, HEBREW.find(' ').unwrap());
    let bands = bands_by_row(&mut h, heb, heb + word);
    assert_eq!(bands.len(), 1);
    let (right, left) = (x(&mut h, heb), x(&mut h, heb + word));
    assert!(left < right);
    let (b0, b1) = bands[0].1[0];
    assert!(bands[0].1.len() == 1 && (b0 - left).abs() < 0.01 && (b1 - right).abs() < 0.01, "{bands:?} against {left}..{right}");
    // Only the rows asked for are read.
    h.put(0);
    h.edit().buf.set_selecting(true);
    h.edit().buf.set_cursor(content.len());
    let (all, place) = (h.editor.selection_bands(f32::MIN, f32::MAX).len(), h.editor.cursor_place().unwrap());
    assert!(h.editor.selection_bands(place.top - 1.0, place.top + 1.0).len() < all);
    // The text keeps its own color: nothing of the selection is in the shaped text.
    h.reshape();
    assert!(h.editor.shaped().unwrap().buffer.lines.iter().all(|l| l.attrs_list().spans().is_empty()));
    // A selection that starts at a line end shows that end, and an empty line selected from its start shows.
    assert_eq!(bands_by_row(&mut h, 6, 7), [(0.0, vec![(x6, x6 + em_half)])], "the first line's end alone");
    let empty = bands_by_row(&mut h, 7, 8);
    assert!(empty.len() == 1 && empty[0].1 == [(0.0, em_half)], "the empty line from its start: {empty:?}");
    // On a row that mixes directions the bands part where right-to-left letters lie unselected between selected ones.
    h.retext(&format!("abc {HEBREW} def"));
    h.put(1);
    h.edit().buf.set_selecting(true);
    h.edit().buf.set_cursor(8);
    bands_cover_the_selected_glyphs(&mut h, "a mixed row");
    assert_eq!(h.editor.selection_bands(f32::MIN, f32::MAX).len(), 2, "`bc ` and the word's first two letters, apart");
}

/// WCAG's contrast of two colors.
fn contrast(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
    let lum = |(r, g, b): (u8, u8, u8)| 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b);
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// An sRGB byte in linear light.
fn linear(c: u8) -> f64 {
    let c = c as f64 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// The selection's tint at `alpha` of 255 over the surface, blended in linear light as an sRGB surface blends it.
fn tint_over_the_surface(alpha: u8) -> (u8, u8, u8) {
    use crate::ui::render::{brushes::SELECTION_BG_RGBA as top, cells::SURFACE_RGB as ground};
    let a = alpha as f64 / 255.0;
    let enc = |l: f64| ((if l <= 0.0031308 { 12.92 * l } else { 1.055 * l.powf(1.0 / 2.4) - 0.055 }) * 255.0).round() as u8;
    let mix = |t: u8, g: u8| enc(a * linear(t) + (1.0 - a) * linear(g));
    (mix(top[0], ground.0), mix(top[1], ground.1), mix(top[2], ground.2))
}

#[test]
fn the_selection_and_the_cursor_are_painted_in_colors_that_read() {
    use crate::ui::render::{brushes::{Solid, EDIT_SELECTION_BG_RGB as band, EDIT_TEXT_RGB as text, SELECTION_BG_RGBA}, cells::SURFACE_RGB as ground};
    let (b, bar) = (ScreenRect { x: 10.0, y: 40.0, w: 30.0, h: 19.0 }, ScreenRect { x: 40.0, y: 40.0, w: 2.5, h: 19.0 });
    let rects = |rs: &[ScreenRect]| rs.iter().map(|r| (r.x, r.y, r.w, r.h)).collect::<Vec<_>>();
    let paints = rows::edit_paints(vec![b], Some(bar));
    assert_eq!(paints.iter().map(|(brush, rs)| (*brush, rects(rs))).collect::<Vec<_>>(),
        [(Solid::EditSelectionBg, rects(&[b])), (Solid::EditBar, rects(&[bar]))], "the bands, then the bar over them");
    // The band is the shared tint dimmed, at alpha 12, made opaque: the same hue, its own brush.
    assert_eq!(band, tint_over_the_surface(12));
    // The text reads on the band at 7 : 1 or better, and the band on the ground at 1.5 : 1 or better.
    assert!(contrast(text, band) >= 7.0 && contrast(band, ground) >= 1.5,
        "text on the band {:.2}, band on the ground {:.2}", contrast(text, band), contrast(band, ground));
    // The bar, in the text's color, reads at 3 : 1 or better on both, by its color alone.
    assert!(contrast(text, ground) >= 3.0 && contrast(text, band) >= 3.0);
    // The shared tint, the other panes' band, would not do: the text reads 1.43 : 1 on it.
    let shared = tint_over_the_surface(SELECTION_BG_RGBA[3]);
    assert!(contrast(text, shared) < 1.5, "{:.2}", contrast(text, shared));
}

#[test]
fn a_selection_band_is_clipped_as_the_text_is() {
    let pane = ScreenRect { x: 50.0, y: 40.0, w: 300.0, h: 100.0 };
    let pad = crate::ui::render::text::EXTRA_TOP_PAD_PX;
    let clip_bottom = pane.y + pad + 95.0;
    let band = |b| rows::band(b, pane, 0.0, clip_bottom);
    assert_eq!(band((10.0, 38.0, 40.0, 19.0)).map(|r| (r.x, r.y, r.w, r.h)), Some((60.0, pane.y + pad + 38.0, 40.0, 19.0)));
    // Cut at the pane's right and left edges, and at its top and the foot of the last whole line, as the bar is.
    assert_eq!(band((280.0, 38.0, 40.0, 19.0)).map(|r| (r.x + r.w, r.w)), Some((pane.x + pane.w, 20.0)));
    assert_eq!(band((-10.0, 38.0, 40.0, 19.0)).map(|r| (r.x, r.w)), Some((pane.x, 30.0)));
    assert_eq!(band((10.0, -pad - 5.0, 40.0, 19.0)).map(|r| (r.y, r.h)), Some((pane.y, 14.0)));
    assert_eq!(band((10.0, 85.0, 40.0, 19.0)).map(|r| (r.y + r.h, r.h)), Some((clip_bottom, 10.0)));
    assert!(band((310.0, 38.0, 40.0, 19.0)).is_none() && band((10.0, 95.0, 40.0, 19.0)).is_none());
}

/// Every cursor position the buffer allows in `body`: a char boundary that is not inside a CRLF.
fn positions(body: &str) -> Vec<usize> {
    (0..=body.len()).filter(|&i| body.is_char_boundary(i) && !(i > 0 && body[..i].ends_with('\r') && body[i..].starts_with('\n'))).collect()
}

/// The tops of the rows the drawn text gives the body line that holds byte `at`.
fn row_tops(h: &mut Harness, at: usize) -> Vec<f32> {
    let body = h.body();
    let lines: Vec<(usize, usize)> = super::super::buffer::lines(&body).collect();
    let k = lines.partition_point(|l| l.0 <= at) - 1;
    let buffer = &h.editor.shaped().unwrap().buffer;
    let shaped = buffer.lines.len() - 2 - lines.len() + k;
    buffer.layout_runs().filter(|r| r.line_i == shaped).map(|r| r.line_top).collect()
}

#[test]
fn a_file_whose_last_line_ends_in_a_lone_cr_has_a_bar_and_a_step_at_every_byte() {
    let cases = [
        ("cr_only_file", "first line\rsecond line\rthird line\r".to_string()),
        ("paragraph_then_cr", format!("{LOREM}\r")),
        ("lf_file_last_line_cr", format!("{LOREM}\n\n{LOREM}\r")),
        ("cr_only_no_final_cr", "first\rsecond\rthird".to_string()),
    ];
    for (name, text) in cases {
        let mut h = Harness::new(&text, 40);
        assert_eq!(h.editor.shaped().unwrap().buffer.lines.len(), super::super::buffer::lines(&text).count() + 2, "{name}: the drawn text has the file's lines");
        for at in positions(&text) {
            h.put(at);
            assert!(h.editor.cursor_place().is_some(), "{name}: no bar at {at}");
            let rev = h.edit().buf.rev();
            let (shaped, index) = (h.editor.shaped.as_ref().unwrap(), h.editor.index.as_ref().unwrap());
            assert!(rows::vertical(&shaped.buffer, index, &text, rev, at, None, 1).is_some(), "{name}: no step down at {at}");
        }
    }
    // Down from the first paragraph's start stays inside it.
    let mut h = Harness::new(&format!("{LOREM}\n\n{LOREM}\r"), 40);
    h.act(Action::EditDown);
    assert!(h.cursor() > 0 && h.cursor() < LOREM.len(), "{}", h.cursor());
    // Delete between the two CRs of a final `\r\r\n` leaves a final lone CR.
    let mut h = Harness::new("t.\r\r\n", 40);
    h.put(3);
    h.press(Key::Named(NamedKey::Delete), Some(Action::EditDelete), false, 3);
    assert_eq!(h.body(), "t.\r");
    assert!(h.editor.cursor_place().is_some());
}

/// Every text of up to `max` characters from `alphabet`.
fn texts_up_to(alphabet: &[&str], max: usize) -> Vec<String> {
    let mut all = vec![String::new()];
    let mut layer = vec![String::new()];
    for _ in 0..max {
        layer = layer.iter().flat_map(|t| alphabet.iter().map(move |c| format!("{t}{c}"))).collect();
        all.extend(layer.iter().cloned());
    }
    all
}

#[test]
fn every_short_text_has_one_row_for_each_cursor_position() {
    let mut h = Harness::new("", 40);
    for text in texts_up_to(&["a", "\r", "\n"], 6) {
        h.retext(&text);
        for at in positions(&text) {
            h.put(at);
            let place = h.editor.cursor_place().unwrap_or_else(|| panic!("{text:?}: no bar at {at}"));
            let tops = row_tops(&mut h, at);
            assert_eq!(tops.iter().filter(|&&t| t == place.top).count(), 1, "{text:?} at {at}: the bar is on one row of its line: {place:?} {tops:?}");
        }
    }
    // A wide and a right-to-left letter: every position has a bar on one row of its line.
    for text in texts_up_to(&["あ", "א", "\r", "\n"], 4) {
        h.retext(&text);
        for at in positions(&text) {
            h.put(at);
            let place = h.editor.cursor_place().unwrap_or_else(|| panic!("{text:?}: no bar at {at}"));
            let tops = row_tops(&mut h, at);
            assert_eq!(tops.iter().filter(|&&t| t == place.top).count(), 1, "{text:?} at {at}: {place:?} {tops:?}");
        }
    }
}

#[test]
fn from_every_position_of_a_short_text_up_then_down_and_down_then_up_come_back_to_the_row() {
    let mut h = Harness::new("", 40);
    for text in texts_up_to(&["a", "\r", "\n"], 5) {
        h.retext(&text);
        for at in positions(&text) {
            for (go, back) in [(Action::EditUp, Action::EditDown), (Action::EditDown, Action::EditUp)] {
                h.put(at);
                let before = h.place().top;
                h.act(go);
                let (moved, mid) = (h.cursor(), h.place().top);
                if mid != before && (mid < before) == (go == Action::EditUp) {
                    h.act(back);
                    assert_eq!(h.place().top, before, "{text:?}: {go:?} from {at} to {moved} and back is not on the starting row");
                } else if go == Action::EditUp {
                    assert_eq!(moved, 0, "{text:?}: Up from the first row at {at}");
                }
            }
        }
    }
}

#[test]
fn a_delete_before_a_crlf_that_leaves_one_leaves_the_cursor_before_it() {
    let mut h = Harness::new("a\rX\nb", 40);
    h.put(2);
    h.press(Key::Named(NamedKey::Delete), Some(Action::EditDelete), false, 3);
    assert_eq!((h.body(), h.cursor()), ("a\r\nb".to_string(), 1));
}

#[test]
fn the_bar_on_a_right_to_left_glyph_stands_at_the_edge_it_is_read_from() {
    // The row's glyphs, read from the shaped buffer: the first byte's bar is the right edge of the glyph that starts
    // there, the end of the text's the left edge of the glyph that ends there.
    let mut h = Harness::new(HEBREW, 40);
    let glyphs: Vec<(usize, usize, f32, f32)> = h.editor.shaped().unwrap().buffer.layout_runs().next().unwrap()
        .glyphs.iter().map(|g| (g.start, g.end, g.x, g.w)).collect();
    assert!(glyphs.iter().all(|g| g.0 < g.1));
    let first = glyphs.iter().find(|g| g.0 == 0).unwrap();
    let last = glyphs.iter().max_by_key(|g| g.1).unwrap();
    h.put(0);
    assert_eq!(h.place().x, first.2 + first.3, "at the start: the right edge of the first glyph");
    h.put(HEBREW.len());
    assert_eq!(h.place().x, last.2, "at the end: the left edge of the last glyph");
}

#[test]
fn a_landed_save_changes_the_footer_and_owes_a_reshape() {
    let mut h = Harness::new("alpha\nbeta", 40);
    h.press_key(None, Some("x"));
    assert!(h.footer().contains("modified"));
    assert!(!h.editor.needs_reshape());
    // The reply snaps the baseline: no text, selection or banner changed, only the footer's words.
    h.editor.mark_file_saved("v2".into());
    assert!(h.editor.needs_reshape(), "the footer says modified until it is shaped again");
    h.reshape();
    assert!(h.footer().contains("clean"), "{}", h.footer());
    assert!(!h.editor.needs_reshape());
}

#[test]
fn escape_on_an_edited_buffer_draws_the_discard_prompt_and_any_other_key_takes_it_down() {
    let mut h = Harness::new("alpha\nbeta", 40);
    h.press_key(None, Some("x"));
    assert!(matches!(h.press_key(Some(Action::Cancel), None), Some(EditorKey::ConfirmDiscard)));
    assert!(h.footer().contains("DISCARD UNSAVED EDITS?"), "{}", h.footer());
    assert!(matches!(h.press_key(Some(Action::EditRight), None), Some(EditorKey::DiscardAnswered)));
    assert!(h.footer().contains("modified") && !h.footer().contains("DISCARD"), "{}", h.footer());
    // The stale banner, raised by a refused save and answered by keeping the edit.
    h.edit().stale_banner = true;
    h.reshape();
    assert!(h.footer().contains("STALE"), "{}", h.footer());
    assert!(matches!(h.press_key(Some(Action::StaleKeep), None), Some(EditorKey::StaleAnswered)));
    assert!(h.footer().contains("modified") && !h.footer().contains("STALE"), "{}", h.footer());
}

#[test]
fn a_pane_with_no_width_is_not_shaped_for_and_is_when_it_returns() {
    let mut h = Harness::new("alpha", 40);
    h.press_key(None, Some("x"));
    h.edit().buf.insert_char('y');
    assert!(h.editor.needs_reshape());
    assert!(!h.editor.owes_shaping(0.0), "no shaping to a pixel for a pane with no width");
    assert!(h.editor.owes_shaping(h.width), "and the width's return owes it");
}

#[test]
fn discarding_with_the_confirm_key_closes_the_edit_as_a_clean_escape_does() {
    // `Closed` is the outcome whose window arm fetches the file's preview again; `DiscardAnswered` is not: after a
    // save, more edits and a discard, the preview showed the render from before the session.
    let mut h = Harness::new("alpha", 40);
    h.press_key(None, Some("x"));
    assert!(matches!(h.press_key(Some(Action::Cancel), None), Some(EditorKey::ConfirmDiscard)));
    assert!(matches!(h.press_key(Some(Action::DiscardConfirm), None), Some(EditorKey::Closed)));
    assert!(!h.editor.is_editing());
    // Any other answer keeps the edit.
    let mut h = Harness::new("alpha", 40);
    h.press_key(None, Some("x"));
    h.press_key(Some(Action::Cancel), None);
    assert!(matches!(h.press_key(Some(Action::EditLeft), None), Some(EditorKey::DiscardAnswered)));
    assert!(h.editor.is_editing());
}

/// The lines the edit buffers of this thread handed the shaper while `f` ran.
fn lines_shaped(h: &mut Harness, f: impl FnOnce(&mut Harness)) -> usize {
    let before = SHAPED.with(|n| n.get());
    f(h);
    SHAPED.with(|n| n.get()) - before
}

/// The edit's buffer and index, brought to it in place key by key, against a fresh whole-text shaping of the same
/// edit: the same lines (text and attributes), the same layout runs (glyph ranges and directions, positions within
/// 0.01 px), the same index and the same place for the cursor.
fn equals_a_fresh_shaping(h: &mut Harness, what: &str) {
    let close = |a: f32, b: f32| (a - b).abs() < 0.01;
    let edit = h.editor.edit.as_ref().unwrap();
    let fresh = MarkdownPreview::new_plain_spans(&mut h.fs, &edit.spans(&KeyBindings::defaults()), h.width, h.scale);
    let (body, rev) = (edit.buf.body(), edit.buf.rev());
    let index = rows::Index::new(&fresh.buffer, body, rev).unwrap_or_else(|| panic!("{what}: a fresh shaping has no index"));
    let shaped = h.editor.shaped.as_ref().unwrap();
    let lines = |b: &cosmic_text::Buffer| b.lines.iter().map(|l| (l.text().to_string(), l.attrs_list().clone())).collect::<Vec<_>>();
    assert_eq!(lines(&shaped.buffer), lines(&fresh.buffer), "{what}: the lines");
    let runs = |b: &cosmic_text::Buffer| b.layout_runs().map(|r| ((r.line_i, r.line_top, r.line_height, r.line_w),
        r.glyphs.iter().map(|g| (g.start, g.end, g.level.is_rtl(), g.x, g.y, g.w)).collect::<Vec<_>>())).collect::<Vec<_>>();
    let (own, theirs) = (runs(&shaped.buffer), runs(&fresh.buffer));
    assert_eq!(own.len(), theirs.len(), "{what}: the number of rows");
    for (row, (a, b)) in own.iter().zip(&theirs).enumerate() {
        let same = a.0.0 == b.0.0 && close(a.0.1, b.0.1) && close(a.0.2, b.0.2) && close(a.0.3, b.0.3) && a.1.len() == b.1.len()
            && a.1.iter().zip(&b.1).all(|(g, f)| (g.0, g.1, g.2) == (f.0, f.1, f.2) && close(g.3, f.3) && close(g.4, f.4) && close(g.5, f.5));
        assert!(same, "{what}: row {row} differs: {a:?} against {b:?}");
    }
    let ((own_lines, own_first, own_rows), (lines, first, rows)) = (h.editor.index.as_ref().unwrap().parts(), index.parts());
    assert_eq!((own_lines, own_first), (lines, first), "{what}: the index's lines");
    assert!(own_rows.len() == rows.len() && own_rows.iter().zip(&rows).all(|(a, b)| (a.0, a.1) == (b.0, b.1) && close(a.2, b.2) && close(a.3, b.3)),
        "{what}: the index's rows");
    let (own, theirs) = (h.editor.cursor_place(), rows::place(&fresh.buffer, &index, body, rev, edit.buf.cursor()));
    assert!(match (own, theirs) { (Some(a), Some(b)) => close(a.x, b.x) && close(a.top, b.top) && close(a.height, b.height), _ => false },
        "{what}: the cursor's place {own:?} against {theirs:?}");
    bands_cover_the_selected_glyphs(h, what);
}

/// The selection's bands against the glyphs (an oracle, not another run of `rows::selection`): every glyph with a
/// selected byte lies under a band on its row, and no other glyph does.
fn bands_cover_the_selected_glyphs(h: &mut Harness, what: &str) {
    let edit = h.editor.edit.as_ref().unwrap();
    let Some((a, b)) = edit.buf.selection_range() else { return };
    let bands = h.editor.selection_bands(f32::MIN, f32::MAX);
    let lines: Vec<(usize, usize)> = super::super::buffer::lines(edit.buf.body()).collect();
    let first = h.editor.index.as_ref().unwrap().first();
    for run in h.editor.shaped().unwrap().buffer.layout_runs() {
        let Some(&(start, _)) = run.line_i.checked_sub(first).and_then(|k| lines.get(k)) else { continue };
        for g in run.glyphs.iter().filter(|g| g.w > 0.0) {
            let (selected, mid) = (start + g.start < b && start + g.end > a, g.x + g.w / 2.0);
            let covered = bands.iter().any(|&(x, top, w, _)| top == run.line_top && x < mid && mid < x + w);
            assert_eq!(selected, covered, "{what}: the glyph {:?} of line {} selected {a}..{b}", &run.text[g.start..g.end], run.line_i);
        }
    }
}

/// `h` shaped as the window would, which must be the whole text: the count of the lines handed the shaper is every line.
fn shaped_whole(h: &mut Harness, what: &str) {
    let shaped = lines_shaped(h, |h| h.reshape());
    assert_eq!(shaped, h.editor.shaped().unwrap().buffer.lines.len(), "{what}: not shaped whole");
}

/// A walk of `steps` random keys over `h` from a fixed seed, held equal to a fresh shaping every 50 keys and at the end.
fn walk_against_fresh_shapings(h: &mut Harness, seed: u64, steps: usize) {
    let mut state = seed;
    let mut below = |n: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % n as u64) as usize
    };
    let letters = ["a", "Z", " ", "あ", "ש", "ب", "e\u{301}", "👍", "\t"];
    let pastes = ["one\ntwo", "x\r\ny\rz\n", "\r", "\n", "\r\n", "שלום\nwor ld", "\u{2028}a\u{85}", "\u{1c}"];
    let motions = [Action::EditLeft, Action::EditRight, Action::EditUp, Action::EditDown, Action::EditHome, Action::EditEnd];
    let (width, mut other) = (h.width, None);
    for step in 0..steps {
        let body = h.body();
        let what = format!("step {step}");
        match below(20) {
            0..=3 => { h.press_key(None, Some(letters[below(letters.len())])); }
            4 => { h.press_key(Some(Action::EditNewline), None); }
            5 => { h.press_key(Some(Action::EditBackspace), None); }
            6 => { h.press_key(Some(Action::EditDelete), None); }
            7 => {
                h.edit().buf.insert_str(pastes[below(pastes.len())]);
                h.press_key(Some(Action::EditRight), None);
            }
            8 => { h.press_key(Some(Action::EditUndo), None); }
            9 => { h.press_key(Some(Action::EditRedo), None); }
            10 | 11 => {
                // Anywhere, or at a line's end or start, where Backspace and Delete meet the line ends.
                let all = positions(&body);
                let ends: Vec<usize> = super::super::buffer::lines(&body).flat_map(|(s, e)| [s, e]).collect();
                let at = if below(2) == 0 { all[below(all.len())] } else { ends[below(ends.len())] };
                h.put(at);
            }
            12 | 13 => h.press(Key::Named(NamedKey::ArrowDown), Some(motions[below(motions.len())]), below(2) == 0, 3),
            14 => {
                // The footer's words: a landed save, the discard prompt raised and answered, the stale banner kept.
                match below(3) {
                    0 => h.editor.mark_file_saved("v".into()),
                    1 if h.edit().is_dirty() => { h.press_key(Some(Action::Cancel), None); }
                    _ => h.editor.mark_stale(),
                }
                h.press_key(Some(Action::EditRight), None);
                h.press_key(Some(Action::StaleKeep), None);
            }
            // The events below shape the whole text, the walk's dearest steps: a quarter of their draws are kept.
            16..=18 if below(4) != 0 => { h.press_key(None, Some(letters[below(letters.len())])); }
            15 => {
                // Edits while the pane has no width, at places apart, shaped together when it returns.
                for _ in 0..=below(3) {
                    let all = positions(&h.body());
                    h.put(all[below(all.len())]);
                    match below(3) {
                        0 => h.edit().buf.insert_char('w'),
                        1 => h.edit().buf.backspace(),
                        _ => h.edit().buf.insert_str("v\n"),
                    }
                }
            }
            16 => {
                // A reload, with a header of another length for an annotation: the whole text, never the buffer of the
                // text before it.
                if h.editor.file_node_id().is_some() {
                    h.editor.arm_file("files:x".into());
                    h.editor.open_file("files:x", true, format!("{body}z"), "v".into());
                } else {
                    let more = "more: x\n".repeat(below(3));
                    h.editor.reload_concept(true, &format!("---\ntitle: reloaded\n{more}---\n{body}z"));
                }
                shaped_whole(h, &what);
            }
            17 => {
                // A change pending at a new width or a new text scale: the whole text.
                h.edit().buf.insert_char('k');
                if below(2) == 0 {
                    h.width = if h.width == width { width * 0.8 } else { width };
                } else {
                    h.scale = if h.scale == 1.0 { 1.25 } else { 1.0 };
                }
                shaped_whole(h, &what);
            }
            18 => {
                // Another row's edit brought back: the whole text, unless it is the very text shaped.
                let parked = h.editor.park();
                if let Some(back) = std::mem::replace(&mut other, parked) {
                    let shaped_from = h.editor.drawn.map(|d| d.rev);
                    let same = Some(EditState::drawn(&back).rev) == shaped_from;
                    h.editor.restore(Some(back));
                    if same { h.reshape() } else { shaped_whole(h, &what) }
                }
            }
            _ => {
                // A selection replaced: Shift steps, then a letter or a paste.
                for _ in 0..=below(4) {
                    h.press(Key::Named(NamedKey::ArrowDown), Some(motions[below(motions.len())]), true, 3);
                }
                if below(2) == 0 { h.press_key(None, Some("q")); } else { h.edit().buf.insert_str("r\ns"); h.press_key(Some(Action::EditLeft), None); }
            }
        }
        if h.editor.owes_shaping(h.width) {
            h.reshape();
        }
        if step % 25 == 24 || step + 1 == steps {
            equals_a_fresh_shaping(h, &what);
        }
    }
}

#[test]
fn every_edit_in_place_equals_a_fresh_shaping_of_its_text() {
    // Paragraphs that wrap at both widths, kept short: a key re-shapes the whole line it lands in.
    let lorem = &LOREM[..80];
    let paragraphs = format!("{lorem}\n\n{HEBREW} and {ARABIC} in a row of words\r\nA CRLF line\r\n\ttabbed\tline\rafter a lone CR\n\
        日本語のテキストは{lorem}\u{2028}after a line separator\u{85}after a next line\n\u{1c}a file separator\n👩‍👩‍👧 family\r");
    let mut h = Harness::concept(&format!("---\ntitle: a note\n---\n{paragraphs}"), 30);
    walk_against_fresh_shapings(&mut h, 0x5eed_1234_abcd_0001, 700);
    let mut h = Harness::new(&paragraphs, 23);
    walk_against_fresh_shapings(&mut h, 0x0dd_ba11_cafe_f00d, 300);
    // An undo or a redo between texts that share bytes across a multi-byte line end (each case a selection replaced,
    // then undone, redone and undone): PS and `é` share their last byte, NEL and `Å` theirs, LS and PS their first two.
    let cases: [(&str, (usize, usize), &str, bool); 8] = [
        ("a\u{2029}b", (1, 4), "é", true), ("a\u{85}b", (1, 3), "Å", true), ("a\u{2028}b", (1, 4), "\u{2029}", false),
        ("xé\ny", (1, 3), "\u{2029}", false), ("x\u{2029}\u{2029}y", (1, 4), "©", true), ("q\u{2028}r\n", (1, 4), "\u{85}", false),
        ("a\rb", (2, 3), "\n", false), ("a\r\u{2029}b", (2, 5), "\nz", false),
    ];
    for (text, (a, b), with, typed) in cases {
        h.retext(text);
        h.put(a);
        h.edit().buf.set_selecting(true);
        h.edit().buf.set_cursor(b);
        if typed { h.press_key(None, Some(with)); } else { h.edit().buf.insert_str(with); h.reshape(); }
        equals_a_fresh_shaping(&mut h, &format!("{text:?} replaced"));
        for (i, key) in [Action::EditUndo, Action::EditRedo, Action::EditUndo].into_iter().enumerate() {
            h.press_key(Some(key), None);
            equals_a_fresh_shaping(&mut h, &format!("{text:?} after {key:?} {i}"));
        }
    }
}

#[test]
fn an_edit_shapes_only_the_lines_it_changes() {
    let text = (0..300).map(|i| format!("line {i} of the file, with words")).collect::<Vec<_>>().join("\n");
    // The start of line `k` of the body as it is now.
    let line = |h: &mut Harness, k: usize| h.body().split('\n').take(k).map(|l| l.len() + 1).sum::<usize>();
    let mut h = Harness::new("", 60);
    assert_eq!(lines_shaped(&mut h, |h| h.retext(&text)), 302, "the first shaping is the whole text and the footer's two lines");
    let at = line(&mut h, 3) + 5;
    h.put(at);
    h.type_char("x");
    // The footer says modified from here on, so each count is the body's lines alone.
    assert_eq!(lines_shaped(&mut h, |h| h.type_char("y")), 1, "a typed letter");
    assert_eq!(lines_shaped(&mut h, |h| { let at = line(h, 10); h.put(at); h.type_char("y") }), 1, "a letter typed at a line's start");
    assert_eq!(lines_shaped(&mut h, |h| h.act(Action::EditNewline)), 2, "Enter");
    assert_eq!(lines_shaped(&mut h, |h| h.act(Action::EditBackspace)), 1, "a Backspace that joins two lines");
    assert_eq!(lines_shaped(&mut h, |h| { let at = line(h, 20) - 1; h.put(at); h.act(Action::EditDelete) }), 1, "a Delete that joins two lines");
    assert_eq!(lines_shaped(&mut h, |h| h.act(Action::EditDelete)), 1, "a Delete");
    let paste = |h: &mut Harness, text: &str| { h.edit().buf.insert_str(text); h.reshape() };
    assert_eq!(lines_shaped(&mut h, |h| paste(h, "a\nb\nc")), 3, "a paste of three lines");
    assert_eq!(lines_shaped(&mut h, |h| paste(h, "a\nb\nc\n")), 4, "a paste of three lines with their ends");
    assert_eq!(lines_shaped(&mut h, |h| h.act(Action::EditUndo)), 1, "an undo of it");
    assert_eq!(lines_shaped(&mut h, |h| h.act(Action::EditRedo)), 4, "a redo of it");
    assert_eq!(lines_shaped(&mut h, |h| h.act(Action::EditRight)), 0, "a motion");
    let select = |h: &mut Harness| h.press(Key::Named(NamedKey::ArrowRight), Some(Action::EditRight), true, 3);
    assert_eq!(lines_shaped(&mut h, select), 1, "a selection that begins: the footer");
    assert_eq!(lines_shaped(&mut h, select), 0, "a selection step");
    assert_eq!(lines_shaped(&mut h, |h| h.press(Key::Named(NamedKey::ArrowDown), Some(Action::EditDown), true, 3)), 0, "a selection step by a row");
    assert_eq!(lines_shaped(&mut h, |h| h.type_char("z")), 2, "a selection replaced by a letter: its line and the footer");
    equals_a_fresh_shaping(&mut h, "after the counted keys");
}

#[test]
fn the_footers_words_change_with_one_line_shaped() {
    let mut h = Harness::new(&long_text(), 40);
    let footer = |h: &mut Harness| h.footer();
    assert_eq!(lines_shaped(&mut h, |h| { h.press_key(None, Some("x")); }), 2, "a letter, and the footer says modified");
    assert!(footer(&mut h).contains("modified"));
    assert_eq!(lines_shaped(&mut h, |h| { h.press_key(Some(Action::Cancel), None); }), 1);
    assert!(footer(&mut h).contains("DISCARD UNSAVED EDITS?"));
    assert_eq!(lines_shaped(&mut h, |h| { h.press_key(Some(Action::EditRight), None); }), 1);
    assert!(footer(&mut h).contains("modified"));
    assert_eq!(lines_shaped(&mut h, |h| { h.editor.mark_file_saved("v2".into()); h.reshape() }), 1);
    assert!(footer(&mut h).contains("clean"));
    assert_eq!(lines_shaped(&mut h, |h| { h.editor.mark_stale(); h.reshape() }), 1);
    assert!(footer(&mut h).contains("STALE"));
    assert_eq!(lines_shaped(&mut h, |h| { h.press_key(Some(Action::StaleKeep), None); }), 1);
    assert!(footer(&mut h).contains("clean"));
    equals_a_fresh_shaping(&mut h, "after the footer's changes");
}

/// A file of `lines` lines of about 90 characters each, words of several lengths.
fn big_file(lines: usize) -> String {
    let words = ["alpha", "be", "gamma", "delta", "epsilon", "mu", "omicron", "pi", "sigma", "tau", "upsilon", "x"];
    (0..lines).map(|i| {
        let mut line = format!("{i:05}");
        let mut k = i;
        while line.len() < 88 {
            line.push(' ');
            line.push_str(words[k % words.len()]);
            k = k.wrapping_mul(7).wrapping_add(3);
        }
        line
    }).collect::<Vec<_>>().join("\n")
}

/// The times of EDIT-KEY-RESHAPES-WHOLE-FILE on a 5,000-line file of about 90 characters a line: each key from its
/// press to a buffer ready to draw, the mean of 20. A timing, so run by hand in a release build:
/// `cargo test --release -p sot-frontend -- --ignored edit_keys_on_a_big_file --nocapture --test-threads 1`.
#[test]
#[ignore = "a timing, run by hand in a release build"]
fn edit_keys_on_a_big_file() {
    let text = big_file(5000);
    let mut h = Harness::new(&text, 120);
    let middle = text.len() / 2;
    let middle = middle + text[middle..].find('\n').unwrap() + 40;
    let ms = |h: &mut Harness, what: &str, key: &mut dyn FnMut(&mut Harness)| {
        let started = Instant::now();
        for _ in 0..20 {
            key(h);
        }
        println!("{what:>16}: {:8.3} ms", started.elapsed().as_secs_f64() * 1000.0 / 20.0);
    };
    let owed = |h: &mut Harness| if h.editor.needs_reshape() { h.reshape() };
    h.put(middle);
    h.type_char("x");
    ms(&mut h, "type a letter", &mut |h| h.type_char("y"));
    ms(&mut h, "backspace", &mut |h| h.act(Action::EditBackspace));
    ms(&mut h, "delete", &mut |h| { h.act(Action::EditLeft); h.act(Action::EditDelete) });
    ms(&mut h, "enter", &mut |h| h.act(Action::EditNewline));
    ms(&mut h, "paste a line", &mut |h| { h.edit().buf.insert_str("a pasted line of words\n"); owed(h) });
    ms(&mut h, "undo", &mut |h| h.act(Action::EditUndo));
    ms(&mut h, "redo", &mut |h| h.act(Action::EditRedo));
    h.press(Key::Named(NamedKey::ArrowRight), Some(Action::EditRight), true, 3);
    ms(&mut h, "selection step", &mut |h| h.press(Key::Named(NamedKey::ArrowRight), Some(Action::EditRight), true, 3));
    ms(&mut h, "motion", &mut |h| h.act(Action::EditRight));
    let (shaped, body, rev) = (h.editor.shaped.as_ref().unwrap(), h.editor.edit.as_ref().unwrap().buf.body(), h.editor.edit.as_ref().unwrap().buf.rev());
    let started = Instant::now();
    for _ in 0..20 {
        std::hint::black_box(rows::Index::new(&shaped.buffer, body, rev));
    }
    println!("{:>16}: {:8.3} ms", "index rebuild", started.elapsed().as_secs_f64() * 1000.0 / 20.0);
}
