//! What a key does in the agent pane: copy, paste and paging, else bytes to its session.

use super::*;
use std::ops::ControlFlow::{self, Break, Continue};
use crate::ui::input::keypress::KeyPress;

pub(in crate::ui) fn agent_pane_key(state: &mut State, key: KeyPress<'_>, label: String) -> ControlFlow<()> {
    let KeyPress { event, action, ctrl, shift, super_, .. } = key;
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
    agent_pane_page_key(state, key, label)?;
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
    Continue(())
}

fn agent_pane_page_key(state: &mut State, key: KeyPress<'_>, label: String) -> ControlFlow<()> {
    let KeyPress { action, .. } = key;
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
    Continue(())
}
