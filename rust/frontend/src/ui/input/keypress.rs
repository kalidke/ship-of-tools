//! One keypress, from the key event to the layer that takes it.

use super::*;
use std::ops::ControlFlow::{self, Break, Continue};
use winit::event::KeyEvent;
use winit::keyboard::ModifiersState;
use crate::ui::input::global_keys::{confirm_quit_key, drawer_chords, help_key, layout_chords, navigation_chords, scroll_line_chords, table_scroll_key, window_chords};
use crate::ui::nav::keys::nav_tree_key;

/// One keypress as `keyboard_input` resolved it; every layer reads it, none resolves it again.
#[derive(Clone, Copy)]
pub(in crate::ui) struct KeyPress<'a> {
    pub(in crate::ui) event: &'a KeyEvent,
    pub(in crate::ui) action: Option<Action>,
    pub(in crate::ui) ctrl: bool,
    pub(in crate::ui) alt: bool,
    pub(in crate::ui) shift: bool,
    pub(in crate::ui) super_: bool,
}

pub(in crate::ui) fn keyboard_input(state: &mut State, event_loop: &ActiveEventLoop, modifiers: ModifiersState, event: KeyEvent, is_synthetic: bool) {
    if is_synthetic {
        // Focus-driven synthetic key events (Alt etc. on focus
        // change) don't represent user intent; ignore.
        return;
    }
    // Allow repeat for arrow keys (Up/Down hold-to-scroll feels
    // wrong without it) but not for action keys.
    if event.state != ElementState::Pressed {
        return;
    }
    if matches!(event.logical_key, Key::Named(NamedKey::Control | NamedKey::Shift |
        NamedKey::Alt | NamedKey::Super | NamedKey::Meta | NamedKey::AltGraph)) { return; }
    // A real, non-synthetic keypress, past this point — presence
    // reporting (design point A) precedes and is independent of
    // whatever action this key resolves to below.
    state.report_presence();
    // Snapshot-and-clear the destroy arm. The D handler
    // re-arms on first press; any other key (cursor move,
    // mode switch, etc.) silently clears it. Same pattern
    // as the now-retired exit-confirm.
    let was_destroy_pending = state.pending_destroy_target.clone();
    state.pending_destroy_target = None;
    let label = key_label(&event.logical_key);
    let ctrl = modifiers.control_key();
    let alt = modifiers.alt_key();
    let shift = modifiers.shift_key();
    let super_ = modifiers.super_key();
    let base_key = event.key_without_modifiers();
    let context = state.help_context();
    let action = state.bindings.resolve(&event.logical_key, Some(&base_key),
        Modifiers { ctrl, alt, shift, super_ }, context.consumes_text(), |a| context.allows(a));
    let key = KeyPress { event: &event, action, ctrl, alt, shift, super_ };
    let _ = route_key(state, event_loop, key, label, context, was_destroy_pending);
}

/// Routes one keypress through the layers in their fixed order; `Break` ends the keypress, `Continue` hands it on.
fn route_key(state: &mut State, event_loop: &ActiveEventLoop, key: KeyPress<'_>, label: String, context: help::Context, was_destroy_pending: Option<WsKey>) -> ControlFlow<()> {
    let KeyPress { event, action, ctrl, alt, shift, super_ } = key;
    confirm_quit_key(state, event_loop, key)?;
    help_key(state, key, context)?;

    tracing::info!(
        ?event.logical_key,
        label = %label,
        repeat = event.repeat,
        ctrl,
        shift,
        alt,
        super_,
        "key pressed"
    );
    window_chords(state, key, label.clone())?;
    navigation_chords(state, key, label.clone())?;
    // Maximise / restore the focused pane via Alt+= (maximise)
    // and Esc (restore) — defaults, overridable in the
    // keybindings file. The visible pane follows `focus`, so
    // Ctrl+Arrow while maximised swaps which pane is on screen —
    // what "I'm zoomed in but want to peek at another pane" wants.
    // Maximise is intercepted globally including LLM focus (user
    // picked pane-management consistency over forwarding Alt+= to
    // the shell). Restore is gated on `state.maximized` so Esc
    // only un-maximises when a pane is actually maximised —
    // otherwise Esc falls through to the pty / edit mode / etc.
    if !event.repeat {
        layout_chords(state, key, label.clone())?;
        drawer_chords(state, key, label.clone())?;
    }
    scroll_line_chords(state, key)?;
    table_scroll_key(state, key)?;
    // Focus-dispatched handling. NavTree = tree nav + mode
    // switches; Repl = code typing + Enter to submit. Preview
    // and Llm are passive today — Escape returns focus to the
    // tree so the user is never stranded with no input target.
    match state.focus {
        PaneFocus::NavTree => {
            nav_tree_key(state, event_loop, key, label.clone(), was_destroy_pending)?;
        }
        PaneFocus::Repl => {
            // Ctrl+L — clear the REPL drawer scrollback (the
            // universal REPL-clear; maintainer note, 2026-07-03, "how do I
            // clear the repl"). Clears the log, its decoded
            // inline figures, and the scroll offset; the julia
            // process and its state are untouched (`r` on a .jl
            // is the process-restart gesture). Terminal drawer
            // unaffected — its pty owns Ctrl+L natively.
            if !event.repeat
                && action == Some(Action::ReplClear)
            {
                state.repl_log.clear();
                state.repl_images.clear();
                state.repl_image_slots.clear();
                state.repl_scroll = 0;
                state.status = "repl · scrollback cleared".to_string();
                state.window.request_redraw();
                return Break(());
            }
            // Paste shortcut (Ctrl+V / Cmd+V / Shift+Insert):
            // read the OS clipboard. The Terminal drawer gets it
            // as a bracketed-paste blob on its pty (like the LLM
            // pane); the Julia REPL drawer gets it appended to
            // its input buffer. Intercepted before the
            // terminal/REPL split below, where Ctrl+V would
            // otherwise send a bare 0x16 to the pty or type a
            // literal "v" into the buffer.
            let is_paste_shortcut = !event.repeat && action == Some(Action::Paste);
            if is_paste_shortcut {
                if state.drawer == DrawerContent::Terminal {
                    forward_clipboard_paste_to_local_term(state);
                } else if let Some(text) = read_clipboard_text() {
                    // REPL input is an editable buffer, not a pty
                    // — no bracketed-paste envelope. Normalize to
                    // `\n`; embedded newlines stay in the buffer
                    // (Shift+Enter inserts them too) and the user
                    // submits with Enter.
                    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
                    state.repl_input.push_str(&normalized);
                    state.repl_scroll = 0;
                }
                state.last_key = Some(label);
                state.window.request_redraw();
                return Break(());
            }
            // G4: when the drawer is showing the local terminal,
            // every keystroke is forwarded to its PTY and the
            // REPL input/history/scrollback handling below is
            // bypassed entirely. Drawer toggles (Ctrl+T / Ctrl+J)
            // and Ctrl+Arrow are intercepted globally before this
            // arm, so they still exit/switch the pane; Tab falls
            // through to the PTY for shell completion.
            if state.drawer == DrawerContent::Terminal {
                // Plain PageUp/PageDown page our scrollback ring —
                // same convention as the LLM pane, so claude in
                // the drawer scrolls like claude in the LLM pane.
                // Alternate-screen apps (vim/less) page themselves,
                // so they get the raw key; Shift+PgUp/PgDn is the
                // escape hatch that hands a primary-screen app the
                // plain key. One-third-pane step like the REPL pane.
                let h = state.pane_rects.repl.height as i32;
                let page_step = (h / 3).max(1);
                #[cfg(windows)]
                let attach_alt_screen = state
                    .attach_term
                    .as_ref()
                    .map(|t| t.screen().alternate_screen());
                #[cfg(not(windows))]
                let attach_alt_screen: Option<bool> = None;
                let alt_screen = state
                    .local_term
                    .as_ref()
                    .map(|t| t.screen().alternate_screen())
                    .or(attach_alt_screen)
                    .unwrap_or(false);
                if !alt_screen {
                    match &event.logical_key {
                        _ if action == Some(Action::ScrollPageUp) => {
                            scroll_drawer_ring(state, page_step);
                            state.window.request_redraw();
                            return Break(());
                        }
                        _ if action == Some(Action::ScrollPageDown) => {
                            scroll_drawer_ring(state, -page_step);
                            state.window.request_redraw();
                            return Break(());
                        }
                        _ => {}
                    }
                }
                if let Some(bytes) = key_to_pty_bytes(&event.logical_key, ctrl, shift, super_) {
                    // Typing snaps back to the live tail so the
                    // cursor/prompt is visible (standard emulator
                    // behaviour).
                    if let Some(t) = state.local_term.as_mut() {
                        t.send_input(&bytes);
                        t.screen_mut().set_scrollback(0);
                    }
                    #[cfg(windows)]
                    if let Some(t) = state.attach_term.as_mut() {
                        t.send_input(&bytes);
                        t.screen_mut().set_scrollback(0);
                    }
                    state.last_key = Some(label);
                    state.window.request_redraw();
                }
                return Break(());
            }
            // Scrollback navigation intercepts before any
            // input-buffer arms, so PgUp/PgDn / Ctrl+u/d
            // don't try to also type into repl_input. Held
            // keys repeat (no `!event.repeat` guard) so
            // hold-to-scroll feels natural. Sign flip vs
            // preview: REPL's scroll origin is the tail,
            // so PgUp grows the offset (older). PgUp/PgDn
            // step is one-third of the pane (not a full
            // page) so two rows of context survive the
            // scroll; full-page jumps were dropping the
            // user out of where they were reading.
            let h = state.pane_rects.repl.height as i32;
            let page_step = (h / 3).max(1);
            match &event.logical_key {
                _ if action == Some(Action::ScrollPageUp) => {
                    let new = (state.repl_scroll as i32 + page_step).max(0);
                    state.repl_scroll = new as u16;
                    state.window.request_redraw();
                    return Break(());
                }
                _ if action == Some(Action::ScrollPageDown) => {
                    let new = (state.repl_scroll as i32 - page_step).max(0);
                    state.repl_scroll = new as u16;
                    state.window.request_redraw();
                    return Break(());
                }
                _ if action == Some(Action::PreviewHalfUp) => {
                    let new = (state.repl_scroll as i32 + h / 2).max(0);
                    state.repl_scroll = new as u16;
                    state.window.request_redraw();
                    return Break(());
                }
                _ if action == Some(Action::PreviewHalfDown) => {
                    let new = (state.repl_scroll as i32 - h / 2).max(0);
                    state.repl_scroll = new as u16;
                    state.window.request_redraw();
                    return Break(());
                }
                // Ctrl+C interrupts a running eval (repl.interrupt).
                // Only dispatched when something is actually in
                // flight: the backend schedules an InterruptException
                // into the eval task and the error+done frames stream
                // back to finalize the entry (no eval_id -- the kernel
                // interrupts its CURRENT_EVAL). With nothing running,
                // Ctrl+C clears the input line (standard REPL UX)
                // instead of typing a literal 'c'.
                _ if action == Some(Action::ReplInterrupt) => {
                    if state.repl_log.iter().any(|e| e.in_flight) {
                        if let Err(e) =
                            state.send(crate::transport::OutgoingReq::ReplInterrupt {
                                workspace_id: state.active_workspace_id.clone(),
                            })
                        {
                            tracing::warn!(error = %e, "drop repl.interrupt - channel closed");
                        } else {
                            tracing::info!("repl.interrupt dispatched (Ctrl+C)");
                            state.status = "interrupting...".to_string();
                        }
                    } else {
                        state.repl_input.clear();
                        state.repl_pkg_mode = false;
                    }
                    state.repl_scroll = 0;
                    state.window.request_redraw();
                    return Break(());
                }
                // Up/Down walk REPL history. Allowed to repeat
                // so hold-to-walk feels natural. Returns
                // early so the input-buffer match below
                // doesn't also see the keypress.
                _ if action == Some(Action::ReplHistoryPrev) => {
                    if let Some(prev) = state.history_step_back() {
                        state.repl_input = prev;
                        state.repl_scroll = 0;
                        state.window.request_redraw();
                    }
                    return Break(());
                }
                _ if action == Some(Action::ReplHistoryNext) => {
                    if let Some(next) = state.history_step_forward() {
                        state.repl_input = next;
                        state.repl_scroll = 0;
                        state.window.request_redraw();
                    }
                    return Break(());
                }
                _ => {}
            }
            match &event.logical_key {
                // Escape returns focus to the tree pane; it
                // does NOT exit the app from inside the REPL
                // — exit only happens from tree focus, which
                // is the safer default for an input pane.
                _ if action == Some(Action::ReturnNav) && !event.repeat => {
                    state.set_focus(PaneFocus::NavTree);
                }
                // Shift+Enter inserts a literal newline into
                // the input buffer instead of submitting —
                // mirrors the convention used by Slack /
                // Discord / VS Code REPLs and a handful of
                // shells. Repeat is allowed so hold-down
                // appends multiple blank lines.
                _ if action == Some(Action::ReplNewline) => {
                    state.repl_input.push('\n');
                    state.repl_scroll = 0;
                }
                _ if action == Some(Action::ReplSubmit) && !event.repeat => {
                    state.submit_repl_input();
                    // Snap back to live when the user
                    // commits a line — the new entry is at
                    // the tail and the user expects to see
                    // its output.
                    state.repl_scroll = 0;
                }
                Key::Named(NamedKey::Backspace) => {
                    // Backspace at start of empty input in
                    // pkg mode leaves pkg mode — mirrors
                    // the standard Julia REPL UX.
                    if state.repl_input.is_empty() && state.repl_pkg_mode {
                        state.repl_pkg_mode = false;
                    } else {
                        state.repl_input.pop();
                    }
                    state.repl_scroll = 0;
                }
                Key::Named(NamedKey::Space) => {
                    state.repl_input.push(' ');
                    state.repl_scroll = 0;
                }
                Key::Character(s) => {
                    // A Command chord that didn't resolve to an
                    // action above must not leak into the Julia
                    // input either (macOS: winit delivers
                    // Cmd+<letter> as a plain Character with
                    // `super_` set — the same invariant
                    // `key_to_pty_bytes` enforces for the ptys).
                    if cfg!(target_os = "macos") && super_ {
                        // consumed, not typed
                    } else if s.as_str() == "]"
                        && state.repl_input.is_empty()
                        && !state.repl_pkg_mode
                    {
                        // `]` at start of empty input enters
                        // pkg mode and consumes the keypress —
                        // again mirroring the standard REPL.
                        state.repl_pkg_mode = true;
                        state.repl_scroll = 0;
                    } else {
                        // Append the typed string verbatim.
                        // winit honours shift/IME so
                        // casing/accents already arrive
                        // correctly. Filter only control
                        // chars so stray sequences don't
                        // leak into the buffer.
                        for c in s.chars() {
                            if !c.is_control() {
                                state.repl_input.push(c);
                            }
                        }
                        state.repl_scroll = 0;
                    }
                }
                _ => {}
            }
        }
        PaneFocus::Preview => {
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
            // Page transport for paginated previews (ADR 0021):
            // n/p and PgDn/PgUp re-fire preview.get for the
            // *shown* node at page ± 1 (clamped). Driven purely
            // by the reply's page extras — the chrome never
            // knows it's a PDF. Consumed even at the clamp edges
            // so a stray press on page 1/N doesn't leak into
            // other handlers; on NON-paginated previews PgUp/
            // PgDn fall through to the text-scroll arms below.
            // No autorepeat: each page is a fresh pdftoppm run.
            if let Some((page, count)) = state.preview_page {
                if count > 1 && !event.repeat {
                    {
                        let next = match action {
                            Some(Action::PageNext) => Some(page.saturating_add(1).min(count)),
                            Some(Action::PagePrev) => Some(page.saturating_sub(1).max(1)),
                            _ => None,
                        };
                        if let Some(np) = next {
                            if np != page {
                                if let Some(node_id) =
                                    state.preview_node_id_fired.clone()
                                {
                                    // New page opens at fit; drop
                                    // any pending zoom re-raster.
                                    state.preview_page_raster_pending = None;
                                    let (fit_w, fit_h) = state.preview_fit_px();
                                    let generation = state.next_preview_gen();
                                    if let Err(e) = state.send(
                                        crate::transport::OutgoingReq::PreviewGet {
                                            node_id,
                                            workspace_id: state
                                                .active_workspace_id
                                                .clone(),
                                            page: Some(np),
                                            fit_w,
                                            fit_h,
                                            generation,
                                        },
                                    ) {
                                        tracing::warn!(error = %e,
                                            "drop page-turn preview.get — channel closed");
                                    }
                                }
                            }
                            state.last_key = Some(label);
                            state.window.request_redraw();
                            return Break(());
                        }
                    }
                }
            }
            // Esc → tree; PgUp/PgDn / Ctrl+u / Ctrl+d /
            // Home / End scroll the preview's flowed text.
            // Held keys repeat for hold-to-scroll. Viewport
            // size is taken from the chrome cell height of
            // the pane — close enough to a body line for
            // the user not to notice the small mismatch
            // with the mouse-wheel row math, and it keeps
            // all four panes on the same rule.
            let h = state.pane_rects.preview.height as i32;
            // PNG-pane zoom/pan routed through `KeyBindings`
            // so `.sot/keybindings.toml` can rebind each
            // action. Defaults: zoom in/out is Shift+Arrow
            // up/down (plus `+`/`=`/`-`); reset is `r` or
            // `0`; pan is the bare arrows. Order matters —
            // ZoomIn checked before PanUp so Shift+ArrowUp
            // doesn't double-fire (the Chord matcher ignores
            // surplus shift for the `=`/`+` compatibility
            // case, so the same key can match both action
            // lists; first-hit wins). Pan step is 10% of
            // the pane size per press so the perceived
            // increment is constant regardless of zoom; the
            // render-time clamp keeps the canvas covering
            // the pane.
            if let Some(img_px) = state.preview_png.as_ref().map(|q| q.size_px) {
                const ZOOM_STEP: f32 = 1.25;
                const PAN_FRAC: f32 = 0.1;
                // Subtract the reserved figure-caption band before
                // ANY of this: the image lives in `image_rect`, not
                // the whole pane, and png_zoom_max keys off pane
                // height (fit = min(w/iw, h/ih)). Computing the
                // ceiling against the unreduced pane made the
                // reachable max zoom depend on whether a caption
                // happened to be set — silently 2.9%–6.4% short on a
                // height-constrained image, since the render path
                // re-clamps with the correct (larger) ceiling, so it
                // degraded to under-zoom rather than a misdraw.
                // Extracted so it's testable — see
                // `preview_image_pane_px`. Reads the band height the
                // last frame published; the one-frame lag is
                // inherent (the band isn't known until the caption
                // is shaped) and harmless, since the render pass
                // re-clamps zoom against its own current ceiling.
                let pane_rect = preview_image_pane_px(
                    (
                        state.pane_rects.preview.width,
                        state.pane_rects.preview.height,
                    ),
                    state.cell_w,
                    state.cell_h,
                    state.caption_band_px,
                );
                let (pane_w, pane_h) = (pane_rect.w, pane_rect.h);
                // Zoom ceiling is per-image: how big a single
                // source pixel may get on screen (16×16 px), not
                // a fixed multiple of fit-to-pane. A dense raster
                // whose native pixels are sub-screen-pixel at fit
                // gets generous headroom; a tiny already-magnified
                // image is held near fit.
                let zoom_max = png_zoom_max(pane_w, pane_h, img_px);
                let mut handled = true;
                if !event.repeat
                    && action == Some(Action::PreviewPngReset)
                {
                    state.preview_png_zoom = 1.0;
                    state.preview_png_pan_px = (0.0, 0.0);
                } else if action == Some(Action::PreviewPngZoomIn) {
                    // Zoom sequence: 1.0 → 1.25 → 2 → 3 → 4 → …
                    // up to the per-image ceiling (`zoom_max`).
                    // Once we're past 1.5×, step in integer
                    // multiples of fit so increments stay
                    // predictable and avoid the moiré beating of
                    // fractional zoom against the nearest-
                    // neighbour sampler grid — which keeps dense
                    // scientific rasters reading crisply per-pixel
                    // (user ask 2026-05-22). The final value is
                    // clamped to the ceiling, so the last step may
                    // land on a fractional zoom that puts a source
                    // pixel at exactly 16 screen px.
                    let cur = state.preview_png_zoom;
                    let raw_next = if cur < 1.5 {
                        let raw = cur * ZOOM_STEP;
                        if raw >= 1.5 {
                            2.0
                        } else {
                            raw
                        }
                    } else {
                        cur + 1.0
                    };
                    let next = raw_next.clamp(1.0, zoom_max);
                    state.scale_png_pan_for_zoom(cur, next);
                    state.preview_png_zoom = next;
                } else if action == Some(Action::PreviewPngZoomOut) {
                    // Mirror of zoom-in: integer-step down
                    // from ≥ 2, then drop back through 1.25
                    // → 1.0. Hitting 2.0 → 1.25 is the
                    // discrete jump out of integer mode so
                    // the user lands cleanly on the
                    // multiplicative step below 1.5×.
                    let cur = state.preview_png_zoom;
                    let next = if cur > 1.5 {
                        let raw = cur - 1.0;
                        if raw < 2.0 {
                            1.25
                        } else {
                            raw
                        }
                    } else {
                        (cur / ZOOM_STEP).max(1.0)
                    };
                    state.scale_png_pan_for_zoom(cur, next);
                    state.preview_png_zoom = next;
                    if state.preview_png_zoom <= 1.0 {
                        state.preview_png_pan_px = (0.0, 0.0);
                    }
                } else if action == Some(Action::PreviewPngPanLeft) {
                    state.preview_png_pan_px.0 += pane_w * PAN_FRAC;
                } else if action == Some(Action::PreviewPngPanRight) {
                    state.preview_png_pan_px.0 -= pane_w * PAN_FRAC;
                } else if action == Some(Action::PreviewPngPanUp) {
                    state.preview_png_pan_px.1 += pane_h * PAN_FRAC;
                } else if action == Some(Action::PreviewPngPanDown) {
                    state.preview_png_pan_px.1 -= pane_h * PAN_FRAC;
                } else if action == Some(Action::PreviewScalebarToggle)
                {
                    // ADR 0034 Ctrl+S. With a scale present this
                    // flips the overlay; with NONE it opens the
                    // pixel-size prompt (§4 live entry) instead of
                    // no-opping, so an uncalibrated raster is one
                    // keystroke from a real bar.
                    if state.preview_scale.is_some() {
                        state.scalebar_on = !state.scalebar_on;
                    } else if !state.begin_scale_entry() {
                        state.status = "scalebar · no image previewed".to_string();
                    }
                } else {
                    handled = false;
                }
                if handled {
                    // View carry is written through from the
                    // render pass (`preview_png_cache`) — a save
                    // here would record LAST frame's ROI.
                    // Paginated page (PDF): re-rasterize at the
                    // new zoom so text stays crisp past 1×.
                    state.maybe_reraster_page();
                    state.last_key = Some(label);
                    state.window.request_redraw();
                    return Break(());
                }
            }
            match action {
                Some(Action::ReturnNav) if !event.repeat => {
                    state.set_focus(PaneFocus::NavTree);
                }
                // ADR 0022: `c` captures the visible image ROI and
                // sends it to the LLM pane. `capture_roi` no-ops
                // with a status hint when the preview isn't a
                // croppable image.
                //
                // Deliberately unguarded on modifiers: Ctrl+C lands
                // here too (it is not a PNG zoom/pan binding, so
                // the block above falls through), and users reach
                // for the universal copy chord out of habit. Both
                // spellings are the same action and both move focus
                // to the LLM pane once the crop paste lands — the
                // focus move itself lives in the ImageCropped arm,
                // not here, because the crop is async and may fail.
                Some(Action::CaptureRegion) if !event.repeat => {
                    state.capture_roi();
                }
                // `e` enters edit mode for the cursored
                // annotation, if there is one. Per the
                // 2026-05-15T21:32Z spec: modal text input,
                // minimal scope, no auto-clobber on save.
                // `y` (vim "yank") copies fenced code blocks in
                // the current markdown preview to the system
                // clipboard. Multiple blocks are joined with a
                // blank line so a "copy everything" call still
                // pastes cleanly into another editor. No-op
                // when the preview isn't markdown or carries no
                // code blocks.
                Some(Action::CopyCode) if !event.repeat => {
                    let sources = &state.preview_md.code_block_sources;
                    if !sources.is_empty() {
                        let joined = sources.join("\n");
                        let n = sources.len();
                        match arboard::Clipboard::new()
                            .and_then(|mut cb| cb.set_text(joined))
                        {
                            Ok(()) => tracing::info!(
                                blocks = n,
                                "yanked code block(s) to clipboard"
                            ),
                            Err(e) => tracing::warn!(
                                error = %e,
                                "failed to write code blocks to clipboard"
                            ),
                        }
                    }
                }
                // Open-style keys work from the preview pane too
                // (same handlers as NavTree), acting on the file
                // whose preview is SHOWING — pinned/badge-consumed
                // previews can differ from the nav cursor — with
                // fallback to the cursored row.
                Some(Action::OpenExternal) if !event.repeat => {
                    let shown = state
                        .previewed_files_path()
                        .or_else(|| state.cursored_files_path());
                    state.open_path_external(shown);
                }
                Some(Action::OpenDocs) if !event.repeat =>
                {
                    let path = state
                        .previewed_files_path()
                        .or_else(|| state.cursored_files_path())
                        .unwrap_or_default();
                    state.docs_open_external(path);
                }
                Some(Action::OpenExecute) if !event.repeat =>
                {
                    let shown = state
                        .previewed_files_path()
                        .or_else(|| state.cursored_files_path());
                    state.quarto_open_execute(shown);
                }
                Some(Action::EditFile) if !event.repeat => {
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
                Some(Action::ScrollPageUp) => {
                    // 1/3-pane step preserves reading
                    // context — full-page jumps lost the
                    // user's place. Ctrl+u still half-pages
                    // for the "I really want to jump"
                    // case.
                    let page_step = (h / 3).max(1);
                    let new = (state.preview_scroll as i32 - page_step).max(0);
                    state.preview_scroll = new as u16;
                }
                Some(Action::ScrollPageDown) => {
                    let page_step = (h / 3).max(1);
                    let new = (state.preview_scroll as i32 + page_step).max(0);
                    state.preview_scroll = new as u16;
                }
                // Plain ArrowUp / ArrowDown scroll the
                // markdown preview vertically by one row.
                // PNG previews intercept these earlier
                // (Action::PreviewPngPanUp/Down) so this
                // arm only fires for non-PNG content.
                Some(Action::PreviewUp) => {
                    state.preview_scroll = state.preview_scroll.saturating_sub(1);
                }
                Some(Action::PreviewDown) => {
                    state.preview_scroll = state.preview_scroll.saturating_add(1);
                }
                Some(Action::PreviewStart) if !event.repeat => {
                    state.preview_scroll = 0;
                }
                Some(Action::PreviewEnd) if !event.repeat => {
                    // Redraw clamps to (total - visible).
                    state.preview_scroll = u16::MAX;
                }
                Some(Action::PreviewHalfUp) => {
                    let new = (state.preview_scroll as i32 - h / 2).max(0);
                    state.preview_scroll = new as u16;
                }
                Some(Action::PreviewHalfDown) => {
                    let new = (state.preview_scroll as i32 + h / 2).max(0);
                    state.preview_scroll = new as u16;
                }
                _ => {}
            }
        }
        PaneFocus::Llm => {
            // Forward keystrokes to the backend-side tmux pty.
            // Esc, Tab, arrows, Ctrl+letter all reach the
            // terminal so shell editing, tmux prefix
            // (Ctrl+B), and TUI apps work. To leave this
            // pane use Ctrl+Arrow — pane move is handled
            // above before this arm runs.
            //
            // Paste shortcut interception: Ctrl+V / Cmd+V /
            // Shift+Insert read the OS clipboard and forward
            // as one bracketed-paste blob, so the remote LLM
            // CLI sees paste-vs-typing correctly and multi-line
            // text doesn't submit on every embedded newline.
            // Ctrl+Shift+C: copy the current mouse selection
            // to the OS clipboard, then consume the key. We
            // pick this chord (not Ctrl+C) deliberately —
            // Ctrl+C must still reach the pty as 0x03 so the
            // LLM CLI's "cancel current request" path works.
            // No selection? Fall through so a stray
            // Ctrl+Shift+C still hits the pty.
            if !event.repeat
                && action == Some(Action::CopySelection)
                && state.llm_selection.is_some()
            {
                state.copy_llm_selection();
                state.last_key = Some(label);
                state.window.request_redraw();
                return Break(());
            }
            let is_paste_shortcut = !event.repeat && action == Some(Action::Paste);
            if is_paste_shortcut {
                forward_clipboard_paste_to_llm(state);
                state.last_key = Some(label);
                state.window.request_redraw();
                return Break(());
            }
            // PgUp/PgDn page the REMOTE pane's scrollback from
            // the keyboard: tmux owns the ring (our vt100 ring
            // stays empty under tmux's in-place repaints), so
            // the backend enters `copy-mode -e` and pages —
            // exactly what the mouse wheel achieves via SGR
            // events, minus the mouse. Alternate-screen apps
            // (vim/less) get the raw key passed through
            // backend-side so their own paging still works.
            // Shift+PgUp/PgDn skip this and fall through as raw
            // bytes — the escape hatch for a remote app that
            // wants the key itself. Repeats allowed: holding
            // PgUp keeps paging.
            if matches!(action, Some(Action::ScrollPageUp | Action::ScrollPageDown)) {
                let scroll = match action {
                    Some(Action::ScrollPageUp) => Some(true),
                    Some(Action::ScrollPageDown) => Some(false),
                    _ => None,
                };
                if let Some(up) = scroll {
                    // ADR 0042 slice L1b: a capsule pane
                    // pages its OWN scrollback (the emulator's
                    // ring, see `scroll_ring`) — every row is a
                    // capsule on this build. ADR 0042 slice
                    // L1b fix 2: dropped entirely while
                    // `pane_feed == Pending` — routing it
                    // anywhere would be a guess before the
                    // attach resolves.
                    //
                    // ADR 0042 slice L1b fix 4: a capsule
                    // row's ALTERNATE-SCREEN app (vim, less)
                    // must receive the raw key instead of
                    // having it consumed as a local-ring
                    // page, matching the drawer's own
                    // PgUp/PgDn arm — `capsule_alt_screen`
                    // gates the early return below so that
                    // case falls through to the ordinary
                    // `key_to_pty_bytes` forward further
                    // down.
                    let capsule_alt_screen = state.pane_feed == PaneFeed::Capsule
                        && state
                            .pane_attach_term
                            .as_ref()
                            .map(|t| t.screen().alternate_screen())
                            .unwrap_or(false);
                    if !capsule_alt_screen {
                        match state.pane_feed {
                            PaneFeed::Capsule => {
                                let page_step =
                                    (state.pane_rects.llm.height as i32 / 3).max(1);
                                let delta = if up { page_step } else { -page_step };
                                if let Some(t) = state.pane_attach_term.as_mut() {
                                    scroll_ring(t.screen_mut(), delta);
                                }
                            }
                            PaneFeed::Pending => {}
                        }
                        state.last_key = Some(label);
                        state.window.request_redraw();
                        return Break(());
                    }
                    // `capsule_alt_screen`: fall through to the
                    // raw-byte forward below, exactly like the
                    // drawer's own escape hatch.
                }
            }
            let bytes: Option<Vec<u8>> =
                key_to_pty_bytes(&event.logical_key, ctrl, shift, super_);
            if let Some(bytes) = bytes {
                // Any byte we send to the pty snaps the
                // view back to live so what the user is
                // typing is always at the bottom of the
                // LLM pane next to the prompt.
                if let Some(t) = state.pane_attach_term.as_mut() {
                    t.screen_mut().set_scrollback(0);
                }
                // ADR 0042 slice L1b fix 2/3: routed through
                // the ONE session-pane input dispatcher — see
                // `send_pane_input`'s own doc.
                state.send_pane_input(&bytes);
            }
        }
    }
    state.last_key = Some(label);
    state.window.request_redraw();
    Continue(())
}
