use super::*;

#[test]
fn insert_str_at_start() {
    let mut b = EditBuffer::new("world".into());
    b.insert_str("hello ");
    assert_eq!(b.body(), "hello world");
    assert_eq!(b.cursor(), 6);
}

#[test]
fn undo_redo_single_insert() {
    let mut b = EditBuffer::new(String::new());
    b.insert_char('x');
    assert!(b.can_undo());
    assert!(b.undo());
    assert_eq!(b.body(), "");
    assert_eq!(b.cursor(), 0);
    assert!(b.can_redo());
    assert!(b.redo());
    assert_eq!(b.body(), "x");
    assert_eq!(b.cursor(), 1);
}

#[test]
fn undo_coalesces_a_typed_run() {
    // A contiguous run of chars undoes in one step, not char-by-char.
    let mut b = EditBuffer::new(String::new());
    for c in "hello".chars() {
        b.insert_char(c);
    }
    assert!(b.undo());
    assert_eq!(b.body(), "");
    assert!(!b.can_undo());
}

#[test]
fn cursor_move_breaks_the_undo_run() {
    let mut b = EditBuffer::new(String::new());
    for c in "ab".chars() {
        b.insert_char(c); // group 1: "" -> "ab"
    }
    b.move_left(); // lands off the run end -> next edit forks
    b.insert_char('Z'); // group 2: "ab" -> "aZb"
    assert_eq!(b.body(), "aZb");
    assert!(b.undo());
    assert_eq!(b.body(), "ab"); // only group 2 undone
    assert!(b.undo());
    assert_eq!(b.body(), ""); // group 1 undone
}

#[test]
fn paste_is_one_atomic_step_and_isolates_typing() {
    let mut b = EditBuffer::new(String::new());
    b.insert_char('a'); // typing group
    b.insert_str("BIG"); // paste = its own step; resets the run
    b.insert_char('z'); // fresh typing group
    assert_eq!(b.body(), "aBIGz");
    assert!(b.undo());
    assert_eq!(b.body(), "aBIG");
    assert!(b.undo());
    assert_eq!(b.body(), "a");
    assert!(b.undo());
    assert_eq!(b.body(), "");
}

#[test]
fn backspace_run_coalesces() {
    let mut b = EditBuffer::new(String::new());
    for c in "abc".chars() {
        b.insert_char(c);
    }
    b.backspace();
    b.backspace();
    assert_eq!(b.body(), "a");
    assert!(b.undo()); // both backspaces undone together
    assert_eq!(b.body(), "abc");
}

#[test]
fn new_edit_clears_redo() {
    let mut b = EditBuffer::new(String::new());
    b.insert_char('a');
    b.undo();
    assert!(b.can_redo());
    b.insert_char('b'); // forks history
    assert!(!b.can_redo());
    assert_eq!(b.body(), "b");
}

#[test]
fn undo_redo_on_empty_history_is_noop() {
    let mut b = EditBuffer::new("x".into());
    assert!(!b.undo());
    assert!(!b.redo());
    assert_eq!(b.body(), "x");
}

#[test]
fn insert_char_advances_cursor() {
    let mut b = EditBuffer::new(String::new());
    b.insert_char('a');
    b.insert_char('b');
    assert_eq!(b.body(), "ab");
    assert_eq!(b.cursor(), 2);
}

#[test]
fn backspace_at_start_is_noop() {
    let mut b = EditBuffer::new("foo".into());
    b.backspace();
    assert_eq!(b.body(), "foo");
    assert_eq!(b.cursor(), 0);
}

#[test]
fn backspace_removes_prev_char() {
    let mut b = EditBuffer::new(String::new());
    b.insert_str("abc");
    b.backspace();
    assert_eq!(b.body(), "ab");
    assert_eq!(b.cursor(), 2);
}

#[test]
fn backspace_handles_multibyte() {
    let mut b = EditBuffer::new(String::new());
    b.insert_char('a');
    b.insert_char('é'); // 2 bytes
    assert_eq!(b.cursor(), 3);
    b.backspace();
    assert_eq!(b.body(), "a");
    assert_eq!(b.cursor(), 1);
}

#[test]
fn delete_at_end_is_noop() {
    let mut b = EditBuffer::new("foo".into());
    b.move_buf_end();
    b.delete();
    assert_eq!(b.body(), "foo");
    assert_eq!(b.cursor(), 3);
}

#[test]
fn delete_removes_next_char_keeps_cursor() {
    let mut b = EditBuffer::new("abc".into());
    // cursor at 0
    b.delete();
    assert_eq!(b.body(), "bc");
    assert_eq!(b.cursor(), 0);
}

#[test]
fn move_left_right_clamps_at_edges() {
    let mut b = EditBuffer::new("ab".into());
    b.move_left();
    assert_eq!(b.cursor(), 0);
    b.move_right();
    b.move_right();
    b.move_right();
    assert_eq!(b.cursor(), 2);
}

#[test]
fn move_line_start_and_end() {
    let mut b = EditBuffer::new("hello\nworld".into());
    // cursor at 0 — line_start no-op
    b.move_line_end();
    assert_eq!(b.cursor(), 5);
    b.insert_char('\n'); // now "hello\n\nworld" with cursor at 6
    b.move_line_end();
    assert_eq!(b.cursor(), 6); // empty line — same position
    b.move_buf_end();
    b.move_line_start();
    assert_eq!(b.cursor(), 7); // first char of "world"
}

#[test]
fn move_up_preserves_column() {
    let mut b = EditBuffer::new("abcde\nfg\nhijklm".into());
    // cursor at 0; move to byte 8 = 'h' position? Let's do this
    // explicitly: line 2 is "fg" starting at 6, so cursor at 8 is
    // the end of line 2 ("g" at 7, after "g" at 8).
    b.cursor = 8;
    // Up from end-of-line-2 (col 2) → end-of-line-1 (line 1 is
    // "abcde", col 2 lands on 'c' which is byte 2).
    b.move_up();
    assert_eq!(b.cursor(), 2);
}

#[test]
fn move_down_clamps_to_shorter_line() {
    let mut b = EditBuffer::new("abcde\nfg".into());
    // cursor at 4 (after 'd' on line 1, col 4)
    b.cursor = 4;
    b.move_down();
    // Line 2 "fg" only has 2 chars; cursor lands at end of "fg" = byte 8.
    assert_eq!(b.cursor(), 8);
}

#[test]
fn enter_inserts_newline() {
    let mut b = EditBuffer::new("ab".into());
    b.move_buf_end();
    b.insert_char('\n');
    b.insert_str("cd");
    assert_eq!(b.body(), "ab\ncd");
    assert_eq!(b.cursor(), 5);
}

#[test]
fn buf_start_and_end_jumps() {
    let mut b = EditBuffer::new("xyz".into());
    b.move_buf_end();
    assert_eq!(b.cursor(), 3);
    b.move_buf_start();
    assert_eq!(b.cursor(), 0);
}

#[test]
fn page_up_down_stops_at_buffer_edges() {
    let mut b = EditBuffer::new("a\nb\nc".into());
    b.move_buf_end();
    b.move_up_rows(100);
    assert_eq!(b.cursor(), 0); // hit buf start
    b.move_down_rows(100);
    assert_eq!(b.cursor(), b.body().len()); // hit buf end
}

#[test]
fn shift_motion_sweeps_a_selection() {
    let mut b = EditBuffer::new("hello world".into());
    b.begin_or_keep_selection();
    for _ in 0..5 {
        b.move_right();
    }
    assert_eq!(b.selection_range(), Some((0, 5)));
    assert_eq!(b.selected_text(), Some("hello"));
    // Anchoring again keeps the existing anchor (no reset mid-sweep).
    b.begin_or_keep_selection();
    b.move_right();
    assert_eq!(b.selected_text(), Some("hello "));
}

#[test]
fn selection_backward_normalizes_range() {
    let mut b = EditBuffer::new("hello".into());
    b.move_buf_end(); // cursor at 5
    b.begin_or_keep_selection();
    b.move_left();
    b.move_left(); // cursor at 3, anchor at 5
    assert_eq!(b.selection_range(), Some((3, 5)));
    assert_eq!(b.selected_text(), Some("lo"));
}

#[test]
fn empty_selection_reads_none() {
    let mut b = EditBuffer::new("hi".into());
    b.begin_or_keep_selection(); // anchor == cursor
    assert_eq!(b.selection_range(), None);
    assert_eq!(b.selected_text(), None);
}

#[test]
fn typing_over_a_selection_replaces_it_atomically() {
    let mut b = EditBuffer::new("hello world".into());
    b.begin_or_keep_selection();
    for _ in 0..5 {
        b.move_right(); // select "hello"
    }
    b.insert_char('H'); // type-over
    assert_eq!(b.body(), "H world");
    assert!(b.selection_range().is_none());
    // The whole type-over undoes in one step back to the original.
    assert!(b.undo());
    assert_eq!(b.body(), "hello world");
}

#[test]
fn paste_over_a_selection_replaces_it() {
    let mut b = EditBuffer::new("hello world".into());
    b.begin_or_keep_selection();
    for _ in 0..5 {
        b.move_right();
    }
    b.insert_str("goodbye"); // paste replaces "hello"
    assert_eq!(b.body(), "goodbye world");
    assert!(b.undo());
    assert_eq!(b.body(), "hello world");
}

#[test]
fn backspace_deletes_a_selection() {
    let mut b = EditBuffer::new("hello world".into());
    b.move_buf_end();
    b.begin_or_keep_selection();
    for _ in 0..6 {
        b.move_left(); // select " world"
    }
    assert!(b.delete_selection());
    assert_eq!(b.body(), "hello");
    assert_eq!(b.cursor(), 5);
}

#[test]
fn plain_motion_clears_selection() {
    let mut b = EditBuffer::new("hello".into());
    b.begin_or_keep_selection();
    b.move_right();
    assert!(b.selection_range().is_some());
    b.clear_selection();
    b.move_right();
    assert!(b.selection_range().is_none());
}
