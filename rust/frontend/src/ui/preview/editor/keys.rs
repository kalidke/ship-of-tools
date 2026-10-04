//! The editor's keys, and entering edit mode from the preview.

use crate::ui::*;
use std::ops::ControlFlow::{self, Break, Continue};
use crate::ui::input::keypress::KeyPress;

pub(in crate::ui) fn editor_key(state: &mut State, key: KeyPress<'_>) -> ControlFlow<()> {
    let KeyPress { event, action, .. } = key;
    // Edit mode hijacks all keys — typing into the
    // editable annotation body takes precedence
    // over scroll keys. Ctrl+S saves; Esc discards
    // (commit 3 adds the dirty-confirm modal).
    if let Some(edit) = state.edit_state.as_mut() {
        // 1/3-pane step here too so the editor's
        // cursor doesn't blow past visible context
        // on PgUp/PgDn.
        let page_rows = (state.pane_rects.preview.height as usize / 3).max(1);
        // When the discard-confirm modal is up,
        // the key handler is just y/n/Esc. Any
        // other key dismisses the modal and
        // returns to editing without consuming
        // the character (felt safer than letting
        // a stray keystroke leak into the buffer
        // during a confirmation).
        if edit.confirm_discard {
            match &event.logical_key {
                // `y` or a second Esc → discard edits and
                // leave the editor (Esc-to-confirm-exit,
                // chosen 2026-06-09: first Esc raises this
                // prompt, a second Esc exits). `n` or any
                // other key cancels back to editing.
                _ if action == Some(Action::DiscardConfirm) && !event.repeat =>
                {
                    state.edit_state = None;
                    state.preview_edit = None;
                }
                _ => {
                    edit.confirm_discard = false;
                }
            }
            state.window.request_redraw();
            return Break(());
        }
        // Stale banner intercepts before edit keys
        // too — r reloads from disk (discards
        // edits), k keeps the banner dismissed so
        // the user can keep editing. The next save
        // will fail again until the underlying
        // file changes or the user reloads.
        if edit.stale_banner {
            match &event.logical_key {
                _ if action == Some(Action::StaleReload) && !event.repeat =>
                {
                    // Reload from disk, discarding edits. For
                    // a file edit re-fire file.read (the
                    // FileRead handler replaces the buffer +
                    // clears the banner via a fresh
                    // edit_state); for a concept edit re-fire
                    // concept.read.
                    let ws = state.active_workspace_id.clone();
                    if let Some(node_id) = edit.file_node_id.clone() {
                        state.pending_file_edit = Some(node_id.clone());
                        if let Err(e) = state.send(OutgoingReq::FileRead {
                            node_id,
                            workspace_id: ws,
                        }) {
                            tracing::warn!(error = %e,
                                "drop file.read for stale reload");
                        }
                    } else {
                        let target = edit.target.clone();
                        let generation = state.next_concept_gen();
                        if let Err(e) = state.send(OutgoingReq::ConceptRead {
                            target,
                            workspace_id: ws,
                            generation,
                        }) {
                            tracing::warn!(error = %e,
                                "drop concept.read for stale reload");
                        }
                    }
                }
                _ if action == Some(Action::StaleKeep) && !event.repeat =>
                {
                    edit.stale_banner = false;
                }
                _ => {
                    // Any other key: ignored.
                    // Banner stays up until the
                    // user picks r or k.
                }
            }
            state.rebuild_edit_preview();
            state.window.request_redraw();
            return Break(());
        }
        // Track whether the buffer changed so we
        // only rebuild `preview_edit` when needed.
        // Cheap either way (few-KB shape), but it
        // keeps the trace log clean of redundant
        // rebuilds during cursor-only navigation.
        let mut buf_changed = false;
        match &event.logical_key {
            _ if action == Some(Action::Cancel) && !event.repeat => {
                // Dirty buffer → confirm modal.
                // Clean buffer → discard right
                // away (no value in asking when
                // there are no edits to lose).
                if edit.is_dirty() {
                    edit.confirm_discard = true;
                } else {
                    state.edit_state = None;
                    state.preview_edit = None;
                    // Re-fetch the underlying preview so
                    // it reflects content saved during
                    // this edit session — the cached
                    // preview is the pre-edit render.
                    // Clearing the fired-guard lets
                    // maybe_fire_preview re-issue
                    // preview.get for the still-selected
                    // node.
                    state.preview_node_id_fired = None;
                    state.maybe_fire_preview();
                }
                state.window.request_redraw();
                return Break(());
            }
            _ if action == Some(Action::EditSave) => {
                let content = edit.full_content();
                let ws = state.active_workspace_id.clone();
                if let Some(node_id) = edit.file_node_id.clone() {
                    // General-file save → file.write, gated
                    // on the version we read (conflict-aware).
                    let expected_version = edit.file_version.clone();
                    if let Err(e) = state.send(OutgoingReq::FileWrite {
                        node_id,
                        content,
                        expected_version,
                        workspace_id: ws,
                    }) {
                        tracing::warn!(error = %e,
                            "drop file.write — channel closed");
                    }
                } else {
                    // Concept-annotation save (existing path).
                    let target = edit.target.clone();
                    let expected = edit.expected_ast_hash.clone();
                    if let Err(e) = state.send(OutgoingReq::ConceptWrite {
                        target,
                        content,
                        expected_ast_hash: expected,
                        workspace_id: ws,
                    }) {
                        tracing::warn!(error = %e,
                            "drop concept.write — channel closed");
                    }
                }
            }
            _ => buf_changed = edit_buffer_key(edit, key, page_rows),
        }
        // Cursor moves count as a content change for
        // the preview because the injected `█` is
        // part of the rendered string — rebuild
        // unconditionally for now (cheap; can
        // optimise later if profiling shows it).
        let _ = buf_changed;
        state.rebuild_edit_preview();
        state.window.request_redraw();
        return Break(());
    }
    Continue(())
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
                        bytes = text.len(),
                        "editor.copy → clipboard"
                    ),
                    Err(e) => tracing::warn!(
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
    let KeyPress { event, action, shift, super_, .. } = key;
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
            // and `key_to_pty_bytes`: a Command
            // chord that resolved to no editor
            // action must not insert text either.
            let is_command =
                cfg!(target_os = "macos") && super_;
            for c in s.chars() {
                if !is_command && !c.is_control() {
                    edit.buf.insert_char(c);
                    buf_changed = true;
                }
            }
        }
        _ => {}
    }
    buf_changed
}

pub(in crate::ui) fn enter_editor(state: &mut State) {
    // Concept-annotation edit takes priority when one
    // is loaded for the cursored node (content is
    // already in `state.concept`).
    let mut entered = false;
    if let (Some(target), Some(info)) =
        (state.concept_target_fired.clone(), state.concept.as_ref())
    {
        if info.target == target && info.exists {
            // Split out frontmatter so the
            // editable buffer holds the body
            // only; the header renders
            // read-only above the edit area
            // and is preserved verbatim on
            // save.
            let (header, body) = split_frontmatter(&info.content);
            state.edit_state = Some(EditState {
                target,
                expected_ast_hash: info.synced_against.clone(),
                header,
                original: body.clone(),
                buf: EditBuffer::new(body),
                confirm_discard: false,
                stale_banner: false,
                file_node_id: None,
                file_version: None,
            });
            state.rebuild_edit_preview();
            entered = true;
        }
    }
    // Otherwise, if the preview is showing a general
    // file, edit the file itself: fetch its raw text
    // via file.read and enter edit mode when the reply
    // lands (see the FileRead handler). `pending_file_edit`
    // matches the reply to this request.
    if !entered && state.edit_state.is_none() {
        if let Some(node_id) = state.preview_node_id_fired.clone() {
            if node_id.starts_with("files:") {
                let ws = state.active_workspace_id.clone();
                if let Err(e) = state.send(OutgoingReq::FileRead {
                    node_id: node_id.clone(),
                    workspace_id: ws,
                }) {
                    tracing::warn!(error = %e, "drop file.read for edit-enter");
                } else {
                    state.pending_file_edit = Some(node_id);
                }
            }
        }
    }
}
