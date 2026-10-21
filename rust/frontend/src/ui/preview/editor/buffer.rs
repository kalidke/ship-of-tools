// ui/preview/editor/buffer.rs — minimal editable text buffer for the concept-write
// editor in the Preview pane.
//
// Scope per a 2026-05-15T21:32Z design note:
//   - Insert / Delete / Backspace, single-char granularity
//   - Cursor motion: ArrowLeft/Right (char), Up/Down (visual line via
//     logical-line proxy), Home, End, Ctrl+Home, Ctrl+End, PgUp, PgDn
//   - Enter inserts a literal `\n`
//
// The body is a `String` (UTF-8). Cursor is a byte index aligned to a
// `char` boundary. All mutating ops uphold that invariant; the helpers
// here are the single place to look for off-by-one or boundary bugs.
//
// "Visual line" up/down uses logical lines (split on `\n`) as a proxy.
// Annotation files are typically narrow enough that wrap-relevant
// behaviour matches; once edit mode grows real wrap-aware navigation
// (phase 2 likely with `$EDITOR` shell-out), this proxy will be
// retired together.

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
        }
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
                self.body = prev.body;
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
                self.body = next.body;
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
        self.body.replace_range(s..e, "");
        self.cursor = s;
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
        self.body.insert_str(self.cursor, s);
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
        self.body.replace_range(prev..self.cursor, "");
        self.cursor = prev;
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
        self.body.replace_range(self.cursor..next, "");
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
    /// preceding `\n`, or byte 0 if there isn't one).
    pub fn move_line_start(&mut self) {
        self.cursor = line_start(&self.body, self.cursor);
    }

    /// Move to the end of the current logical line (the position of the
    /// next `\n`, or EOF).
    pub fn move_line_end(&mut self) {
        self.cursor = line_end(&self.body, self.cursor);
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
        let prev_line_terminator = cur_start - 1; // the `\n`
        let prev_start = line_start(&self.body, prev_line_terminator);
        let prev_text = &self.body[prev_start..prev_line_terminator];
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
        let next_start = cur_end + 1; // skip the `\n`
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
        self.body[..byte]
            .char_indices()
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    fn next_boundary(&self, byte: usize) -> usize {
        self.body[byte..]
            .chars()
            .next()
            .map(|c| byte + c.len_utf8())
            .unwrap_or(byte)
    }
}

/// Byte index of the first char on the line containing `byte`. Either
/// the position after the most recent `\n` before `byte`, or 0.
fn line_start(s: &str, byte: usize) -> usize {
    s[..byte].rfind('\n').map(|i| i + 1).unwrap_or(0)
}

/// Byte index of the line terminator (the `\n`) on the line containing
/// `byte`, or the buffer length if the line runs to EOF.
fn line_end(s: &str, byte: usize) -> usize {
    s[byte..].find('\n').map(|i| byte + i).unwrap_or(s.len())
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
