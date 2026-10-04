//! What a key does in the drawer: clear and paste, then the Terminal's pty or the REPL's scroll, history and input.

use crate::ui::*;
use std::ops::ControlFlow::{self, Break, Continue};
use crate::ui::input::keypress::KeyPress;

pub(in crate::ui) fn drawer_pane_key(state: &mut State, key: KeyPress<'_>, label: String) -> ControlFlow<()> {
    let KeyPress { event, action, .. } = key;
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
                    state.send(crate::net::transport::OutgoingReq::ReplInterrupt {
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
