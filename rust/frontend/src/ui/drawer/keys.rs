//! What a key does in the drawer: clear and paste, then the Terminal's pty or the REPL's scroll, history and input.

use crate::ui::*;
use std::ops::ControlFlow::{self, Break, Continue};
use crate::ui::input::keypress::KeyPress;

pub(in crate::ui) fn drawer_pane_key(state: &mut State, key: KeyPress<'_>, label: String) -> ControlFlow<()> {
    let KeyPress { event, action, .. } = key;
    // Ctrl+L — clear the REPL drawer scrollback (the
    // universal REPL-clear; decided 2026-07-03). Clears the log, its decoded
    // inline figures, and the scroll offset; the julia
    // process and its state are untouched (`r` on a .jl
    // is the process-restart gesture). Terminal drawer
    // unaffected — its pty owns Ctrl+L natively.
    if !event.repeat
        && action == Some(Action::ReplClear)
    {
        let effects = state.repl_model.clear_scrollback();
        state.apply_repl_effects(effects);
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
        if state.layout_help.drawer() == DrawerContent::Terminal {
            forward_clipboard_paste_to_local_term(state);
        } else if let Some(text) = read_clipboard_text() {
            // REPL input is an editable buffer, not a pty
            // — no bracketed-paste envelope. Normalize to
            // `\n`; embedded newlines stay in the buffer
            // (Shift+Enter inserts them too) and the user
            // submits with Enter.
            state.repl_model.edit_draft(DraftEdit::Paste(&text));
        }
        state.last_key = Some(label);
        state.window.request_redraw();
        return Break(());
    }
    terminal_key(state, key, label)?;
    repl_scroll_key(state, key)?;
    repl_input_key(state, key);
    Continue(())
}

fn terminal_key(state: &mut State, key: KeyPress<'_>, label: String) -> ControlFlow<()> {
    let KeyPress { event, action, ctrl, shift, super_, .. } = key;
    // G4: when the drawer is showing the local terminal,
    // every keystroke is forwarded to its PTY and the
    // REPL input/history/scrollback handling below is
    // bypassed entirely. Drawer toggles (Ctrl+T / Ctrl+J)
    // and Ctrl+Arrow are intercepted globally before this
    // arm, so they still exit/switch the pane; Tab falls
    // through to the PTY for shell completion.
    if state.layout_help.drawer() == DrawerContent::Terminal {
        // Plain PageUp/PageDown page our scrollback ring —
        // same convention as the LLM pane, so claude in
        // the drawer scrolls like claude in the LLM pane.
        // Alternate-screen apps (vim/less) page themselves,
        // so they get the raw key; Shift+PgUp/PgDn is the
        // escape hatch that hands a primary-screen app the
        // plain key. One-third-pane step like the REPL pane.
        let h = state.pane_rects.repl.height as i32;
        let page_step = (h / 3).max(1);
        let alt_screen = state
            .local_term
            .as_ref()
            .map(|t| t.screen().alternate_screen())
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
            state.last_key = Some(label);
            state.window.request_redraw();
        }
        return Break(());
    }
    Continue(())
}

fn repl_scroll_key(state: &mut State, key: KeyPress<'_>) -> ControlFlow<()> {
    let KeyPress { event, action, .. } = key;
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
            state.repl_model.scroll_rows(page_step);
            state.window.request_redraw();
            return Break(());
        }
        _ if action == Some(Action::ScrollPageDown) => {
            state.repl_model.scroll_rows(-page_step);
            state.window.request_redraw();
            return Break(());
        }
        _ if action == Some(Action::PreviewHalfUp) => {
            state.repl_model.scroll_rows(h / 2);
            state.window.request_redraw();
            return Break(());
        }
        _ if action == Some(Action::PreviewHalfDown) => {
            state.repl_model.scroll_rows(-(h / 2));
            state.window.request_redraw();
            return Break(());
        }
        // Ctrl+C interrupts a running eval (repl.interrupt).
        // Only dispatched when something is actually in
        // flight, naming every eval in flight: the shim
        // interrupts the running eval only if it is one of
        // them, and the error+done frames stream back to
        // finalize the entry. With nothing running, Ctrl+C
        // clears the input line (standard REPL UX) instead of
        // typing a literal 'c'.
        _ if action == Some(Action::ReplInterrupt) => {
            let target = state.repl_target();
            let plan = state.repl_model.prepare_interrupt(target);
            let success = if let Some(plan) = &plan {
                match state.send_to(&plan.host, plan.request()) {
                    Ok(()) => true,
                    Err(e) => {
                        tracing::warn!(error = %e, "drop repl.interrupt - channel closed");
                        false
                    }
                }
            } else { false };
            let effects = state.repl_model.complete_interrupt(plan, success);
            state.apply_repl_effects(effects);
            return Break(());
        }
        // Up/Down walk REPL history. Allowed to repeat
        // so hold-to-walk feels natural. Returns
        // early so the input-buffer match below
        // doesn't also see the keypress.
        _ if action == Some(Action::ReplHistoryPrev) => {
            if state.repl_model.history_back() {
                state.window.request_redraw();
            }
            return Break(());
        }
        _ if action == Some(Action::ReplHistoryNext) => {
            if state.repl_model.history_forward() {
                state.window.request_redraw();
            }
            return Break(());
        }
        _ => {}
    }
    Continue(())
}

fn repl_input_key(state: &mut State, key: KeyPress<'_>) {
    let KeyPress { event, action, super_, .. } = key;
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
            state.repl_model.edit_draft(DraftEdit::Newline);
        }
        _ if action == Some(Action::ReplSubmit) && !event.repeat => {
            state.submit_repl_input();
            // Snap back to live when the user
            // commits a line — the new entry is at
            // the tail and the user expects to see
            // its output.
            state.repl_model.scroll_to_tail();
        }
        Key::Named(NamedKey::Backspace) => {
            state.repl_model.edit_draft(DraftEdit::Backspace);
        }
        Key::Named(NamedKey::Space) => {
            state.repl_model.edit_draft(DraftEdit::Space);
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
            } else {
                state.repl_model.edit_draft(DraftEdit::Typed(s.as_str()));
            }
        }
        _ => {}
    }
}
