// ui/preview/editor/buffer.rs — minimal editable text buffer for the concept-write
// editor in the Preview pane.
//
// Scope per a 2026-05-15T21:32Z design note:
//   - Insert / Delete / Backspace, single-char granularity
//   - Cursor motion: ArrowLeft/Right (char), Up/Down (the row on screen,
//     see below), Home, End, Ctrl+Home, Ctrl+End, PgUp, PgDn
//   - Enter inserts a literal `\n`
//
// The body is a `String` (UTF-8). Cursor is a byte index aligned to a
// `char` boundary. All mutating ops uphold that invariant; the helpers
// here are the single place to look for off-by-one or boundary bugs.
//
// Up, Down, PgUp and PgDn move by the row on screen: `rows::vertical`
// reads the rows of the buffer the window draws and lands the cursor with
// `set_cursor`. `move_up`, `move_down` and their `_rows` forms below step by
// logical lines (the shaper's: see `lines`) and run only for a step `vertical` cannot
// make: a row of right-to-left text, or a shaped buffer that is not the
// body's text. Home and End stay on the logical line, so in a wrapped
// paragraph they go to the paragraph's start and end. The cursor is not in
// the shaped text: the window draws it over the rows (`rows::place`).
// Lines end where the shaper ends them (`lines`, `end_len`: a CRLF is one end,
// a lone CR, LF, NEL, LS and PS are each one), and the shaped rows' index reads
// the same function. A CRLF is one position: no motion, landing or removal
// leaves the cursor, and so a selection edge, between the `\r` and the `\n`
// (`off_crlf`), since the drawn text turns each span's line ends in by itself.

/// One point-in-time buffer state for the undo/redo stacks.
#[derive(Debug, Clone)]
struct Snapshot {
    body: String,
    cursor: usize,
}

/// Coarse edit class used to coalesce a run of same-kind, contiguous edits
/// into a single undo step (so undo peels off a typed word, not one char).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditKind {
    Insert,
    Delete,
}

/// Cap on undo depth — bounds memory on a long editing session. Oldest
/// states are dropped first.
const MAX_UNDO: usize = 500;

#[derive(Debug, Clone)]
pub struct EditBuffer {
    body: String,
    cursor: usize,
    /// Past states, most-recent last. Each entry is the buffer *before* an
    /// edit group; `undo()` pops one and restores it.
    undo: Vec<Snapshot>,
    /// States popped by `undo()`, available to `redo()`. Cleared on any new
    /// edit (classic linear-history model).
    redo: Vec<Snapshot>,
    /// Kind of the in-progress coalescing run, or `None` after a group break
    /// (undo/redo, paste, or a non-contiguous edit).
    last_kind: Option<EditKind>,
    /// Cursor position the current run left off at. A new edit coalesces only
    /// if it continues from here — so a cursor move between edits (which lands
    /// elsewhere) starts a fresh undo group on its own, without the move
    /// methods having to touch undo state.
    last_edit_pos: Option<usize>,
    /// Fixed end of an active selection (byte index, char-aligned). The
    /// cursor is the moving end; the selection spans `min(anchor, cursor)..
    /// max(anchor, cursor)`. `None` when nothing is selected. A plain motion
    /// clears it; a Shift+motion sets it (if unset) and sweeps the cursor.
    anchor: Option<usize>,
    /// Replaced by a number no buffer has had at every change of the text (and at birth), so a layout shaped from an
    /// earlier text, or from another buffer's, is told from this one even when the lengths agree. A move of the
    /// cursor leaves it.
    rev: u64,
    /// The bytes the text's changes since the last `take_change` cover, `None` after none (and at birth).
    change: Option<Change>,
}

/// What the text's changes since revision `since` did: bytes `start..old_end` of that text are `start..new_end` of
/// this one, and the rest is as it was (the bytes after `old_end` moved by the difference).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Change {
    pub since: u64,
    pub start: usize,
    pub old_end: usize,
    pub new_end: usize,
}

impl EditBuffer {
    pub fn new(body: String) -> Self {
        Self {
            body,
            cursor: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            last_kind: None,
            last_edit_pos: None,
            anchor: None,
            rev: next_rev(),
            change: None,
        }
    }

    /// The change since the last call (since birth, at the first), with the revision it starts from; `None` when the
    /// text has not changed since.
    pub fn take_change(&mut self) -> Option<Change> {
        self.change.take()
    }

    /// Fold a change of bytes `start..old_end` of the text to `start..new_end` into `change`, under a new revision:
    /// every change of the text is told here, so `change` covers them all.
    fn record(&mut self, start: usize, old_end: usize, new_end: usize) {
        let since = std::mem::replace(&mut self.rev, next_rev());
        let c = self.change.unwrap_or(Change { since, start, old_end: start, new_end: start });
        // `c` covers `c.start..c.new_end` of the text before this change; the union is mapped back past it.
        let hi = c.new_end.max(old_end);
        self.change = Some(Change { since: c.since, start: c.start.min(start), old_end: hi - c.new_end + c.old_end, new_end: hi - old_end + new_end });
    }

    /// Replace `range` of the body with `text`, the one way a key changes the text.
    fn splice(&mut self, range: std::ops::Range<usize>, text: &str) {
        self.record(range.start, range.end, range.start + text.len());
        self.body.replace_range(range, text);
    }

    /// Make the body `body` (an undo or a redo), told as the bytes between the two texts' common start and end.
    fn replace_body(&mut self, body: String) {
        let (old, new) = (self.body.as_bytes(), body.as_bytes());
        let mut start = old.iter().zip(new).take_while(|(a, b)| a == b).count();
        let shorter = old.len().min(new.len()) - start;
        let mut tail = old.iter().rev().zip(new.iter().rev()).take(shorter).take_while(|(a, b)| a == b).count();
        // Both texts agree on the bytes either side, so a char boundary of one is a boundary of the other.
        while !self.body.is_char_boundary(start) {
            start -= 1;
        }
        while !self.body.is_char_boundary(old.len() - tail) {
            tail -= 1;
        }
        let (old_end, new_end) = (old.len() - tail, new.len() - tail);
        self.record(start, old_end, new_end);
        self.body = body;
    }

    pub fn rev(&self) -> u64 {
        self.rev
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Restore the most recent pre-edit state; pushes the current state onto
    /// the redo stack. Returns `false` (no-op) when there's nothing to undo.
    pub fn undo(&mut self) -> bool {
        match self.undo.pop() {
            Some(prev) => {
                self.redo.push(Snapshot {
                    body: self.body.clone(),
                    cursor: self.cursor,
                });
                self.replace_body(prev.body);
                self.cursor = prev.cursor.min(self.body.len());
                self.last_kind = None;
                self.last_edit_pos = None;
                self.anchor = None;
                true
            }
            None => false,
        }
    }

    /// Reapply the most recently undone state. Returns `false` when the redo
    /// stack is empty.
    pub fn redo(&mut self) -> bool {
        match self.redo.pop() {
            Some(next) => {
                self.undo.push(Snapshot {
                    body: self.body.clone(),
                    cursor: self.cursor,
                });
                self.replace_body(next.body);
                self.cursor = next.cursor.min(self.body.len());
                self.last_kind = None;
                self.last_edit_pos = None;
                self.anchor = None;
                true
            }
            None => false,
        }
    }

    /// Decide whether a `kind` edit at the current cursor extends the active
    /// run or starts a new undo group, snapshot accordingly, and invalidate
    /// the redo stack. Call *before* mutating; callers set `last_edit_pos` to
    /// the new cursor afterward.
    fn checkpoint(&mut self, kind: EditKind) {
        let contiguous =
            self.last_kind == Some(kind) && self.last_edit_pos == Some(self.cursor);
        if !contiguous {
            self.undo.push(Snapshot {
                body: self.body.clone(),
                cursor: self.cursor,
            });
            if self.undo.len() > MAX_UNDO {
                self.undo.remove(0);
            }
            self.last_kind = Some(kind);
        }
        self.redo.clear();
    }

    pub fn body(&self) -> &str {
        &self.body
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Byte range `(start, end)` of the active selection with `start < end`,
    /// or `None` when nothing is selected (no anchor, or an empty one).
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        let (s, e) = if a <= self.cursor {
            (a, self.cursor)
        } else {
            (self.cursor, a)
        };
        if s == e {
            None
        } else {
            Some((s, e))
        }
    }

    /// The currently-selected substring, or `None` when nothing is selected.
    pub fn selected_text(&self) -> Option<&str> {
        self.selection_range().map(|(s, e)| &self.body[s..e])
    }

    /// Anchor a selection at the cursor if one isn't already active. Call
    /// before a Shift+motion so the moving cursor sweeps out a range.
    pub fn begin_or_keep_selection(&mut self) {
        if self.anchor.is_none() {
            self.anchor = Some(self.cursor);
        }
    }

    /// Drop any active selection (the cursor stays put). Call before a plain
    /// (un-shifted) motion.
    pub fn clear_selection(&mut self) {
        self.anchor = None;
    }

    /// Prepare for a motion: `extend` keeps/starts the selection (a Shift+
    /// motion), otherwise it drops it (a plain motion). Call immediately
    /// before a `move_*` so the cursor sweeps or collapses correctly.
    pub fn set_selecting(&mut self, extend: bool) {
        if extend {
            self.begin_or_keep_selection();
        } else {
            self.clear_selection();
        }
    }

    /// Remove the selected range as one undo step; the cursor lands at the
    /// range start and the selection clears. Returns whether anything was
    /// removed (also clears a stale empty anchor on the `false` path).
    fn delete_selection_raw(&mut self) -> bool {
        let Some((s, e)) = self.selection_range() else {
            self.anchor = None;
            return false;
        };
        self.undo.push(Snapshot {
            body: self.body.clone(),
            cursor: self.cursor,
        });
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.splice(s..e, "");
        self.cursor = off_crlf(&self.body, s);
        self.anchor = None;
        self.last_kind = None;
        self.last_edit_pos = None;
        true
    }

    /// Delete the active selection (Backspace/Delete/cut over a selection).
    /// One atomic undo step; no-op when nothing is selected.
    pub fn delete_selection(&mut self) -> bool {
        self.delete_selection_raw()
    }

    /// Raw insertion with no undo bookkeeping — shared by `insert_char`
    /// (coalesced typing) and `insert_str` (atomic paste).
    fn insert_str_raw(&mut self, s: &str) {
        self.splice(self.cursor..self.cursor, s);
        self.cursor += s.len();
    }

    /// Insert a string at the cursor as a single atomic undo step (e.g. an
    /// IME composition or a paste). Isolated from any surrounding typing run.
    pub fn insert_str(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        // A selection is replaced atomically: the delete snapshots the
        // pre-edit state, then the raw insert rides the same undo step.
        if self.selection_range().is_some() {
            self.delete_selection_raw();
            self.insert_str_raw(s);
            self.last_kind = None;
            self.last_edit_pos = None;
            return;
        }
        self.undo.push(Snapshot {
            body: self.body.clone(),
            cursor: self.cursor,
        });
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.last_kind = None;
        self.last_edit_pos = None;
        self.insert_str_raw(s);
    }

    /// Insert one char at the cursor. Consecutive contiguous chars coalesce
    /// into one undo step.
    pub fn insert_char(&mut self, c: char) {
        // Type-over: a char typed with an active selection replaces it as one
        // atomic step (delete snapshots; the raw insert rides the same step).
        if self.selection_range().is_some() {
            self.delete_selection_raw();
            let mut buf = [0u8; 4];
            self.insert_str_raw(c.encode_utf8(&mut buf));
            self.last_kind = None;
            self.last_edit_pos = None;
            return;
        }
        self.checkpoint(EditKind::Insert);
        let mut buf = [0u8; 4];
        self.insert_str_raw(c.encode_utf8(&mut buf));
        self.last_edit_pos = Some(self.cursor);
    }

    /// Remove the char immediately before the cursor (Backspace). No-op
    /// at byte 0. Cursor lands on the position the removed char used to
    /// occupy.
    pub fn backspace(&mut self) {
        // Over a selection, Backspace deletes the selection (one step).
        if self.delete_selection() {
            return;
        }
        if self.cursor == 0 {
            return;
        }
        self.checkpoint(EditKind::Delete);
        let prev = self.prev_boundary(self.cursor);
        self.splice(prev..self.cursor, "");
        self.cursor = off_crlf(&self.body, prev);
        self.last_edit_pos = Some(self.cursor);
    }

    /// Remove the char at the cursor (Delete). No-op at EOF. Cursor
    /// position is unchanged.
    pub fn delete(&mut self) {
        // Over a selection, Delete removes the selection (one step).
        if self.delete_selection() {
            return;
        }
        if self.cursor >= self.body.len() {
            return;
        }
        self.checkpoint(EditKind::Delete);
        let next = self.next_boundary(self.cursor);
        self.splice(self.cursor..next, "");
        self.cursor = off_crlf(&self.body, self.cursor);
        self.last_edit_pos = Some(self.cursor);
    }

    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.prev_boundary(self.cursor);
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.body.len() {
            self.cursor = self.next_boundary(self.cursor);
        }
    }

    /// Move to the start of the current logical line (after the
    /// preceding line end, or byte 0 if there isn't one).
    pub fn move_line_start(&mut self) {
        self.cursor = line_start(&self.body, self.cursor);
    }

    /// Move to the end of the current logical line (before its line end, or EOF).
    pub fn move_line_end(&mut self) {
        self.cursor = line_end(&self.body, self.cursor);
    }

    /// Put the cursor at `at`, a `char` boundary of the body (before the `\r` of a CRLF if `at` is inside it); any
    /// other offset leaves it where it is.
    pub fn set_cursor(&mut self, at: usize) {
        if self.body.is_char_boundary(at) {
            self.cursor = off_crlf(&self.body, at);
        }
    }

    pub fn move_buf_start(&mut self) {
        self.cursor = 0;
    }

    pub fn move_buf_end(&mut self) {
        self.cursor = self.body.len();
    }

    /// Move up one logical line, preserving the column (in chars) when
    /// possible. If the target line is shorter than the current column,
    /// the cursor lands at that line's end.
    pub fn move_up(&mut self) {
        let cur_start = line_start(&self.body, self.cursor);
        if cur_start == 0 {
            // Already on the first line — collapse to start instead of
            // doing nothing, so a held Up reliably reaches buf start.
            self.cursor = 0;
            return;
        }
        let col = self.body[cur_start..self.cursor].chars().count();
        // The line end before this line: two bytes for a CRLF, else the one character.
        let prev_end = cur_start - if self.body[..cur_start].ends_with("\r\n") { 2 } else { self.body[..cur_start].chars().next_back().map_or(1, char::len_utf8) };
        let prev_start = line_start(&self.body, prev_end);
        let prev_text = &self.body[prev_start..prev_end];
        self.cursor = prev_start + nth_char_byte(prev_text, col);
    }

    /// Move down one logical line, preserving the column.
    pub fn move_down(&mut self) {
        let cur_start = line_start(&self.body, self.cursor);
        let cur_end = line_end(&self.body, self.cursor);
        if cur_end >= self.body.len() {
            // Last line — collapse to end on a held Down.
            self.cursor = self.body.len();
            return;
        }
        let col = self.body[cur_start..self.cursor].chars().count();
        let next_start = cur_end + end_len(&self.body, cur_end).unwrap_or(0);
        let next_end = line_end(&self.body, next_start);
        let next_text = &self.body[next_start..next_end];
        self.cursor = next_start + nth_char_byte(next_text, col);
    }

    /// Move up `rows` logical lines at once (PageUp). Column is
    /// preserved on the row we land on.
    pub fn move_up_rows(&mut self, rows: usize) {
        for _ in 0..rows {
            let before = self.cursor;
            self.move_up();
            if self.cursor == before {
                break;
            }
        }
    }

    /// Move down `rows` logical lines at once (PageDown).
    pub fn move_down_rows(&mut self, rows: usize) {
        for _ in 0..rows {
            let before = self.cursor;
            self.move_down();
            if self.cursor == before {
                break;
            }
        }
    }

    fn prev_boundary(&self, byte: usize) -> usize {
        if self.body[..byte].ends_with("\r\n") {
            return byte - 2;
        }
        self.body[..byte]
            .char_indices()
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    fn next_boundary(&self, byte: usize) -> usize {
        if self.body[byte..].starts_with("\r\n") {
            return byte + 2;
        }
        self.body[byte..]
            .chars()
            .next()
            .map(|c| byte + c.len_utf8())
            .unwrap_or(byte)
    }
}

/// A revision number no buffer has had before: the shaped layout of a text names the revision it was shaped from.
fn next_rev() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// `at`, or the byte before the `\r` when `at` falls between the `\r` and the `\n` of a CRLF: a line end is one
/// position, so no cursor or selection edge ever stands inside it.
fn off_crlf(s: &str, at: usize) -> usize {
    if at > 0 && s.as_bytes().get(at - 1) == Some(&b'\r') && s.as_bytes().get(at) == Some(&b'\n') { at - 1 } else { at }
}

/// The characters a line ends at, as the shaper ends one (`normalize_newlines`): a CRLF is one end, and a lone CR,
/// LF, NEL, LS and PS are each one. This and `end_len` are the one place that says where lines end, for the buffer's
/// motions, the shaped rows' index (`rows`) and, by the shaper's own rule, the drawn text.
const END_CHARS: [char; 5] = ['\r', '\n', '\u{85}', '\u{2028}', '\u{2029}'];

/// The length of the line end that starts at `at`, or `None` where none does (`\r\n` is one end of two bytes; a CR
/// at the end of the text, or before anything else, is a lone CR).
pub(super) fn end_len(s: &str, at: usize) -> Option<usize> {
    let c = s[at..].chars().next().filter(|c| END_CHARS.contains(c))?;
    Some(if c == '\r' && s.as_bytes().get(at + 1) == Some(&b'\n') { 2 } else { c.len_utf8() })
}

/// The lines of `s` as `(start, end)` byte ranges without their ends: one more than there are ends, so a text that
/// ends in a line end ends in an empty line.
pub(super) fn lines(s: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut next = Some(0);
    std::iter::from_fn(move || {
        let start = next?;
        let end = s[start..].find(END_CHARS).map_or(s.len(), |i| start + i);
        next = end_len(s, end).map(|len| end + len);
        Some((start, end))
    })
}

/// What `change` did to the lines of the text (`lines`): the lines `before` of the text it changed (indexes into `old`,
/// that text's lines) are now `after` (byte ranges in `body`, the text now). The lines before `before` are as they
/// were, and those after it are too but for moving by the change's length. A CR is read with the character after it
/// (with an LF it is one end), so the line a CR ends just before the change is in `before`, and so is a line that
/// starts where the change ends.
pub(super) fn line_change(old: &[(usize, usize)], body: &str, change: &Change) -> (std::ops::Range<usize>, Vec<(usize, usize)>) {
    let line_of = |at: usize| old.partition_point(|l| l.0 <= at) - 1;
    let cr_before = change.start > 0 && body.as_bytes()[change.start - 1] == b'\r';
    let (first, last) = (line_of(change.start - usize::from(cr_before)), line_of(change.old_end));
    let from = old[first].0;
    let after = lines(&body[from..]).map(|(s, e)| (from + s, from + e)).take_while(|l| l.0 <= change.new_end).collect();
    (first..last + 1, after)
}

/// Byte index of the first char on the line containing `byte`: after the last line end before it, or 0.
fn line_start(s: &str, byte: usize) -> usize {
    s[..byte].rfind(END_CHARS).map_or(0, |i| i + s[i..].chars().next().map_or(1, char::len_utf8))
}

/// Byte index of the line end on the line containing `byte` (before the `\r` of a CRLF), or the buffer length if the
/// line runs to EOF.
fn line_end(s: &str, byte: usize) -> usize {
    s[byte..].find(END_CHARS).map_or(s.len(), |i| byte + i)
}

/// Byte offset of the `n`-th char in `s`, or `s.len()` if `s` has fewer
/// than `n` chars. Used by up/down to land at the same column.
fn nth_char_byte(s: &str, n: usize) -> usize {
    s.char_indices()
        .nth(n)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

#[cfg(test)]
#[path = "buffer_tests.rs"]
mod tests;
