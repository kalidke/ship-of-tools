//! The in-pane editor: the active edit (`EditState`), the file edit awaiting its `file.read`, the shaped buffer the
//! pane draws, and the keys that edit the buffer.

use crate::ui::*;
use crate::ui::input::keypress::KeyPress;

/// Active concept-annotation edit. `None` when the preview pane is in
/// read-only view mode (the default); `Some` when the user pressed `e`
/// on a cursored annotation. Carries enough context to fire a
/// well-formed `concept.write` and to discriminate a stale-write
/// refusal from a fresh-edit refusal.
#[derive(Clone)]
pub(in crate::ui) struct EditState {
    /// Concept target being edited (`files/path/to.rs`,
    /// `modules/Foo`, ...). Matched against `ConceptWriteDone.target`
    /// when the write reply lands.
    target: String,
    /// `synced_against` AST hash captured at edit-enter time. Sent as
    /// `expected_ast_hash` on every save so the backend gates the
    /// write with optimistic concurrency (Linux's `4ebca35`).
    expected_ast_hash: Option<String>,
    /// YAML frontmatter (`---\n...\n---\n`) captured at edit-enter,
    /// or `None` when the source had none. Rendered above the
    /// editable region as a read-only header; concatenated with
    /// `buf.body()` on save so the on-disk file's frontmatter
    /// survives round-trip byte-perfect.
    header: Option<String>,
    /// Body captured when entering edit mode — used to detect whether
    /// the buffer is dirty (the discard-confirm modal cares; save
    /// snaps this to the current body so post-save edits start
    /// clean again).
    original: String,
    /// Live editable text + cursor. Holds *body only* — frontmatter
    /// is in `header`.
    buf: EditBuffer,
    /// True while the discard-confirm modal is up (Esc pressed on a
    /// dirty buffer). `y` confirms discard, `n` / Esc / any other key
    /// dismisses and returns to editing.
    confirm_discard: bool,
    /// True while the stale-write banner is up (a `concept.write`
    /// returned `stale_write` because the on-disk `synced_against`
    /// no longer matches our `expected_ast_hash`). `r` re-reads the
    /// file and replaces the buffer with the on-disk content
    /// (discarding edits); `k` dismisses the banner and lets the
    /// user keep editing (the next save will fail again until they
    /// reload or the file changes back). Never auto-clobber.
    stale_banner: bool,
    /// `Some(files:<relpath>)` when editing a general source file (vs a
    /// `.concept/` annotation, where this is `None`). Selects the save path:
    /// `Some` → `file.write` keyed on this node id; `None` → `concept.write`
    /// keyed on `target`.
    file_node_id: Option<String>,
    /// Content version from the file's `file.read`, sent back as
    /// `file.write`'s `expected_version` for optimistic concurrency. `None`
    /// for concept edits (they gate on `expected_ast_hash`).
    file_version: Option<String>,
}

impl EditState {
    fn is_dirty(&self) -> bool {
        self.buf.body() != self.original
    }

    /// Reassemble the full on-disk content from `header` + the live
    /// buffer body — sent as the payload of every `concept.write`.
    fn full_content(&self) -> String {
        match &self.header {
            Some(h) => h.clone() + self.buf.body(),
            None => self.buf.body().to_string(),
        }
    }

    /// The edit preview's styled spans. Layout:
    ///   - read-only frontmatter header (if any), prefixed line-by-line
    ///     with `│ ` so the user reads it as a sidebar
    ///   - blank line separator
    ///   - editable body with `█` injected at the cursor byte
    ///   - blank line + status footer ("modified" if dirty; modal
    ///     prompt when `confirm_discard` is up)
    fn spans(&self, bindings: &KeyBindings) -> Vec<(String, bool)> {
        // Compose styled spans: header, body (cursor block + optional
        // selection tint), footer. Concatenated text is what renders; the
        // `bool` flags the selected run so `new_plain_spans` tints it.
        let mut spans: Vec<(String, bool)> = Vec::new();
        if let Some(header) = &self.header {
            let mut h = String::new();
            for line in header.lines() {
                h.push_str("│ ");
                h.push_str(line);
                h.push('\n');
            }
            h.push('\n');
            spans.push((h, false));
        }
        let body = self.buf.body();
        let cur = self.buf.cursor();
        const CURSOR: &str = "\u{2588}";
        let has_sel = self.buf.selection_range().is_some();
        match self.buf.selection_range() {
            // Cursor sits at one end of the range (a = start, b = end). Place
            // the cursor block on its side; tint the selected run amber.
            Some((a, b)) if cur <= a => {
                spans.push((body[..a].to_string(), false));
                spans.push((CURSOR.to_string(), false));
                spans.push((body[a..b].to_string(), true));
                spans.push((body[b..].to_string(), false));
            }
            Some((a, b)) => {
                spans.push((body[..a].to_string(), false));
                spans.push((body[a..b].to_string(), true));
                spans.push((CURSOR.to_string(), false));
                spans.push((body[b..].to_string(), false));
            }
            None => {
                spans.push((body[..cur].to_string(), false));
                spans.push((CURSOR.to_string(), false));
                spans.push((body[cur..].to_string(), false));
            }
        }
        // Footer: blank line + status. Modal overrides everything — when the
        // user is confirming, that's the only thing they should read.
        // Priority: stale > discard-confirm > selection > modified > clean.
        let mut footer = String::from("\n\n");
        if self.stale_banner {
            footer.push_str(&format!(
                "── STALE: file changed on disk. {} reload (discard edits) · {} keep editing ──",
                bindings.labels(Action::StaleReload), bindings.labels(Action::StaleKeep)));

        } else if self.confirm_discard {
            footer.push_str(&format!(
                "── DISCARD UNSAVED EDITS? {} discard · another key keeps editing ──",
                bindings.labels(Action::DiscardConfirm)));

        } else if has_sel {
            footer.push_str(&format!("── edit mode · selection · {} copy · {} cut · {} save ──",
                bindings.first_label(Action::EditCopy), bindings.first_label(Action::EditCut), bindings.first_label(Action::EditSave)));
        } else if self.is_dirty() {
            footer.push_str(&format!("── edit mode · modified · {} save · {} discard ──",
                bindings.first_label(Action::EditSave), bindings.first_label(Action::Cancel)));
        } else {
            footer.push_str(&format!("── edit mode · clean · {} save · {} exit ──",
                bindings.first_label(Action::EditSave), bindings.first_label(Action::Cancel)));
        }
        spans.push((footer, false));
        spans
    }
}

/// The preview pane's editor. An edit opens on a cursored annotation at once, or on a general file when the
/// `file.read` its request named answers; the shaped buffer is rebuilt from the edit and is absent without one.
/// A row's park carries the edit (its banners included) but never the shaped buffer or a file request in flight.
#[derive(Default)]
pub(in crate::ui) struct Editor {
    edit: Option<EditState>,
    /// Node id (`files:<relpath>`) of a general-file edit-enter (or a stale reload) awaiting its `file.read` reply.
    pending_file: Option<String>,
    /// Shaped preview buffer for the active edit, drawn in place of the file preview.
    shaped: Option<MarkdownPreview>,
}

/// What a key in edit mode did, for the window to send, re-shape and repaint.
pub(in crate::ui) enum EditorKey {
    /// The discard prompt was answered: the edit closed, or the prompt went down.
    DiscardAnswered,
    /// Reload a stale file edit from disk; its `file.read` is armed.
    ReloadFile(String),
    /// Reload a stale annotation edit from disk.
    ReloadConcept(String),
    /// The stale banner took the key (kept, or ignored).
    StaleAnswered,
    /// Cancel on a dirty buffer raised the discard prompt.
    ConfirmDiscard,
    /// Cancel on a clean buffer closed the edit.
    Closed,
    SaveFile { node_id: String, content: String, expected_version: Option<String> },
    SaveConcept { target: String, content: String, expected_ast_hash: Option<String> },
    /// The buffer or its cursor took the key.
    Edited,
}

/// What a `file.read` reply meant to the editor.
pub(in crate::ui) enum FileReadOutcome {
    Opened,
    Missing,
    Unasked,
}

impl Editor {
    pub(in crate::ui) fn is_editing(&self) -> bool { self.edit.is_some() }
    pub(in crate::ui) fn confirming_discard(&self) -> bool { self.edit.as_ref().is_some_and(|e| e.confirm_discard) }
    pub(in crate::ui) fn stale(&self) -> bool { self.edit.as_ref().is_some_and(|e| e.stale_banner) }

    /// The target of an edit whose stale banner is up.
    pub(in crate::ui) fn stale_target(&self) -> Option<&str> {
        self.edit.as_ref().filter(|edit| edit.stale_banner).map(|edit| edit.target.as_str())
    }

    pub(in crate::ui) fn edits_target(&self, target: &str) -> bool {
        self.edit.as_ref().map(|e| e.target == target).unwrap_or(false)
    }

    pub(in crate::ui) fn file_node_id(&self) -> Option<&str> {
        self.edit.as_ref().and_then(|e| e.file_node_id.as_deref())
    }

    pub(in crate::ui) fn shaped(&self) -> Option<&MarkdownPreview> { self.shaped.as_ref() }

    /// Edit a loaded annotation: the frontmatter is split out so the buffer holds the body only; the header renders
    /// read-only above it and is preserved verbatim on save.
    pub(in crate::ui) fn enter_concept(&mut self, target: String, synced_against: Option<String>, content: &str) {
        let (header, body) = split_frontmatter(content);
        self.edit = Some(EditState {
            target,
            expected_ast_hash: synced_against,
            header,
            original: body.clone(),
            buf: EditBuffer::new(body),
            confirm_discard: false,
            stale_banner: false,
            file_node_id: None,
            file_version: None,
        });
    }

    /// A general file's `file.read` was sent; its reply opens the edit.
    pub(in crate::ui) fn arm_file(&mut self, node_id: String) { self.pending_file = Some(node_id); }

    /// A `file.read` reply: the armed request's opens the editor on the file when it exists (replacing any prior
    /// edit, which is how a stale reload discards); any other is ignored.
    pub(in crate::ui) fn open_file(
        &mut self, node_id: &str, exists: bool, content: String, version: String,
    ) -> FileReadOutcome {
        if self.pending_file.as_deref() != Some(node_id) {
            return FileReadOutcome::Unasked;
        }
        self.pending_file = None;
        if !exists {
            return FileReadOutcome::Missing;
        }
        self.edit = Some(EditState {
            target: node_id.to_string(),
            expected_ast_hash: None,
            header: None,
            original: content.clone(),
            buf: EditBuffer::new(content),
            confirm_discard: false,
            stale_banner: false,
            file_node_id: Some(node_id.to_string()),
            file_version: Some(version),
        });
        FileReadOutcome::Opened
    }

    /// A stale annotation edit's reload: the on-disk content replaces the buffer and both banners go down.
    pub(in crate::ui) fn reload_concept(&mut self, exists: bool, content: &str) {
        if let Some(edit) = self.edit.as_mut() {
            let (header, body) = split_frontmatter(content);
            edit.header = header;
            edit.expected_ast_hash = if exists {
                sot_protocol::annotation::synced_against(content)
            } else {
                None
            };
            edit.original = body.clone();
            edit.buf = EditBuffer::new(body);
            edit.stale_banner = false;
            edit.confirm_discard = false;
        }
    }

    /// A save landed: the dirty baseline snaps to the buffer, so further edits start clean.
    pub(in crate::ui) fn mark_saved(&mut self) {
        if let Some(edit) = self.edit.as_mut() {
            edit.original = edit.buf.body().to_string();
        }
    }

    /// A file save landed: the baseline snaps and the new version gates the next save.
    pub(in crate::ui) fn mark_file_saved(&mut self, version: String) {
        if let Some(edit) = self.edit.as_mut() {
            edit.original = edit.buf.body().to_string();
            edit.file_version = Some(version);
        }
    }

    /// A save was refused as stale: the banner goes up.
    pub(in crate::ui) fn mark_stale(&mut self) {
        if let Some(edit) = self.edit.as_mut() {
            edit.stale_banner = true;
        }
    }

    /// Leave edit mode; the pane falls back to the read-only preview.
    pub(in crate::ui) fn close(&mut self) {
        self.edit = None;
        self.shaped = None;
    }

    /// The edit a row parks with.
    pub(in crate::ui) fn park(&self) -> Option<EditState> { self.edit.clone() }

    /// A row's restore brings back its parked edit; the caller re-shapes it.
    pub(in crate::ui) fn restore(&mut self, edit: Option<EditState>) { self.edit = edit; }

    /// Re-shape the buffer from the edit through `shape`, which is given the edit's spans; without an edit the
    /// shaped buffer goes.
    pub(in crate::ui) fn reshape(
        &mut self, bindings: &KeyBindings, shape: impl FnOnce(&[(String, bool)]) -> MarkdownPreview,
    ) {
        self.shaped = self.edit.as_ref().map(|edit| shape(&edit.spans(bindings)));
    }

    /// Edit mode takes every key (`None` without an edit). The discard prompt takes only its answer; the stale
    /// banner only reload or keep; otherwise cancel, save, or the buffer's own keys. `page_rows` is the paging step.
    pub(in crate::ui) fn key(&mut self, key: KeyPress<'_>, page_rows: usize) -> Option<EditorKey> {
        let KeyPress { event, action, .. } = key;
        let edit = self.edit.as_mut()?;
        // When the discard-confirm modal is up,
        // the key handler is just y/n/Esc. Any
        // other key dismisses the modal and
        // returns to editing without consuming
        // the character (felt safer than letting
        // a stray keystroke leak into the buffer
        // during a confirmation).
        if edit.confirm_discard {
            // `y` or a second Esc → discard edits and
            // leave the editor (Esc-to-confirm-exit,
            // chosen 2026-06-09: first Esc raises this
            // prompt, a second Esc exits). `n` or any
            // other key cancels back to editing.
            if action == Some(Action::DiscardConfirm) && !event.repeat {
                self.close();
            } else {
                edit.confirm_discard = false;
            }
            return Some(EditorKey::DiscardAnswered);
        }
        // Stale banner intercepts before edit keys
        // too — r reloads from disk (discards
        // edits), k keeps the banner dismissed so
        // the user can keep editing. The next save
        // will fail again until the underlying
        // file changes or the user reloads.
        if edit.stale_banner {
            if action == Some(Action::StaleReload) && !event.repeat {
                // Reload from disk, discarding edits. For
                // a file edit re-fire file.read (the
                // FileRead handler replaces the buffer +
                // clears the banner via a fresh
                // edit); for a concept edit re-fire
                // concept.read.
                if let Some(node_id) = edit.file_node_id.clone() {
                    self.pending_file = Some(node_id.clone());
                    return Some(EditorKey::ReloadFile(node_id));
                }
                return Some(EditorKey::ReloadConcept(edit.target.clone()));
            }
            if action == Some(Action::StaleKeep) && !event.repeat {
                edit.stale_banner = false;
            }
            // Any other key: ignored. Banner stays up until the user picks r or k.
            return Some(EditorKey::StaleAnswered);
        }
        if action == Some(Action::Cancel) && !event.repeat {
            // Dirty buffer → confirm modal.
            // Clean buffer → discard right
            // away (no value in asking when
            // there are no edits to lose).
            if edit.is_dirty() {
                edit.confirm_discard = true;
                return Some(EditorKey::ConfirmDiscard);
            }
            self.close();
            return Some(EditorKey::Closed);
        }
        if action == Some(Action::EditSave) {
            let content = edit.full_content();
            return Some(match edit.file_node_id.clone() {
                // General-file save → file.write, gated
                // on the version we read (conflict-aware).
                Some(node_id) => {
                    EditorKey::SaveFile { node_id, content, expected_version: edit.file_version.clone() }
                }
                // Concept-annotation save (existing path).
                None => EditorKey::SaveConcept {
                    target: edit.target.clone(),
                    content,
                    expected_ast_hash: edit.expected_ast_hash.clone(),
                },
            });
        }
        edit_buffer_key(edit, key, page_rows);
        Some(EditorKey::Edited)
    }
}

fn edit_buffer_key(edit: &mut EditState, key: KeyPress<'_>, page_rows: usize) -> bool {
    let KeyPress { event, action, .. } = key;
    let mut buf_changed = false;
    match &event.logical_key {
        _ if action == Some(Action::EditUndo) => {
            if edit.buf.undo() {
                buf_changed = true;
            }
        }
        _ if action == Some(Action::EditRedo) => {
            if edit.buf.redo() {
                buf_changed = true;
            }
        }
        // Ctrl+C: copy the active selection to the OS
        // clipboard. Consumed even with no selection so
        // it never types a literal "c" into the buffer.
        _ if action == Some(Action::EditCopy) =>
        {
            if let Some(sel) = edit.buf.selected_text() {
                let text = sel.to_string();
                match arboard::Clipboard::new()
                    .and_then(|mut cb| cb.set_text(text.clone()))
                {
                    Ok(()) => tracing::info!(
                        target: "sot::ui::preview::editor::keys",
                        bytes = text.len(),
                        "editor.copy → clipboard"
                    ),
                    Err(e) => tracing::warn!(
                        target: "sot::ui::preview::editor::keys",
                        error = %e,
                        "clipboard write failed; editor copy dropped"
                    ),
                }
            }
        }
        // Ctrl+X: cut — copy the selection, then delete
        // it as one undo step. No selection → consumed
        // no-op (never types an "x").
        _ if action == Some(Action::EditCut) =>
        {
            if let Some(sel) = edit.buf.selected_text() {
                let text = sel.to_string();
                if let Err(e) = arboard::Clipboard::new()
                    .and_then(|mut cb| cb.set_text(text))
                {
                    tracing::warn!(
                        target: "sot::ui::preview::editor::keys",
                        error = %e,
                        "clipboard write failed; editor cut still deletes"
                    );
                }
                edit.buf.delete_selection();
                buf_changed = true;
            }
        }
        // Paste (Ctrl+V / Cmd+V): insert the OS
        // clipboard as one atomic undo step. Without
        // this arm Ctrl+V falls through to the generic
        // Character arm below and types a literal "v".
        // Normalize line endings to the buffer's `\n`
        // convention (Enter inserts `\n`).
        _ if action == Some(Action::EditPaste) =>
        {
            if let Some(text) = read_clipboard_text() {
                edit.buf.insert_str(
                    &text.replace("\r\n", "\n").replace('\r', "\n"),
                );
                buf_changed = true;
            }
        }
        _ if action == Some(Action::EditNewline) => {
            edit.buf.insert_char('\n');
            buf_changed = true;
        }
        _ if action == Some(Action::EditBackspace) => {
            edit.buf.backspace();
            buf_changed = true;
        }
        _ if action == Some(Action::EditDelete) => {
            edit.buf.delete();
            buf_changed = true;
        }
        _ => buf_changed = edit_motion_key(edit, key, page_rows),
    }
    buf_changed
}

fn edit_motion_key(edit: &mut EditState, key: KeyPress<'_>, page_rows: usize) -> bool {
    let KeyPress { event, action, shift, ctrl, alt, super_ } = key;
    let mut buf_changed = false;
    match &event.logical_key {
        // Motion keys: `set_selecting(shift)` extends a
        // selection on Shift+motion and drops it on a
        // plain motion. (Shift+Arrow no longer cycles
        // workspaces here — that's gated to non-edit
        // mode at the top of the key handler.)
        _ if action == Some(Action::EditLeft) => {
            edit.buf.set_selecting(shift);
            edit.buf.move_left();
        }
        _ if action == Some(Action::EditRight) => {
            edit.buf.set_selecting(shift);
            edit.buf.move_right();
        }
        _ if action == Some(Action::EditUp) => {
            edit.buf.set_selecting(shift);
            edit.buf.move_up();
        }
        _ if action == Some(Action::EditDown) => {
            edit.buf.set_selecting(shift);
            edit.buf.move_down();
        }
        _ if action == Some(Action::EditStart) => {
            edit.buf.set_selecting(shift);
            edit.buf.move_buf_start();
        }
        _ if action == Some(Action::EditFinish) => {
            edit.buf.set_selecting(shift);
            edit.buf.move_buf_end();
        }
        _ if action == Some(Action::EditHome) => {
            edit.buf.set_selecting(shift);
            edit.buf.move_line_start();
        }
        _ if action == Some(Action::EditEnd) => {
            edit.buf.set_selecting(shift);
            edit.buf.move_line_end();
        }
        _ if action == Some(Action::EditPageUp) => {
            edit.buf.set_selecting(shift);
            edit.buf.move_up_rows(page_rows);
        }
        _ if action == Some(Action::EditPageDown) => {
            edit.buf.set_selecting(shift);
            edit.buf.move_down_rows(page_rows);
        }
        Key::Named(NamedKey::Space) => {
            edit.buf.insert_char(' ');
            buf_changed = true;
        }
        Key::Character(s) => {
            // Same invariant as the Julia input line
            // and `key_to_pty_bytes`: a chord that
            // resolved to no editor action must not
            // insert text either.
            let types = super::keys::chord_types_text(ctrl, alt, super_);
            for c in s.chars() {
                if types && !c.is_control() {
                    edit.buf.insert_char(c);
                    buf_changed = true;
                }
            }
        }
        _ => {}
    }
    buf_changed
}
