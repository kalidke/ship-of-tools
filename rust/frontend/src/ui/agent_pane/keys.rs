//! What a key does in the agent pane: copy, paste and paging, else bytes to its session.

use super::*;
use std::ops::ControlFlow::{self, Break, Continue};
use crate::ui::input::keypress::KeyPress;

pub(in crate::ui) fn agent_pane_key(state: &mut State, key: KeyPress<'_>, label: String) -> ControlFlow<()> {
    let KeyPress { event, action, ctrl, shift, super_, .. } = key;
    // Unhandled keys reach the selected client; Ctrl+Arrow leaves this pane.
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
        && state.agent_pane.selection().is_some()
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
        state.agent_pane.reset_scroll();
        // ADR 0042 slice L1b fix 2/3: routed through
        // the ONE session-pane input dispatcher — see
        // `send_pane_input`'s own doc.
        state.send_pane_input(&bytes);
    }
    Continue(())
}

fn agent_pane_page_key(state: &mut State, key: KeyPress<'_>, label: String) -> ControlFlow<()> {
    let KeyPress { action, .. } = key;
    // Ordinary page actions scroll the client's local ring. Alternate-screen applications
    // receive the original key; shifted page keys already fall through action resolution.
    if matches!(action, Some(Action::ScrollPageUp | Action::ScrollPageDown)) {
        let scroll = match action {
            Some(Action::ScrollPageUp) => Some(true),
            Some(Action::ScrollPageDown) => Some(false),
            _ => None,
        };
        if let Some(up) = scroll {
            if state.agent_pane.page(up, state.pane_rects.llm.height) {
                state.last_key = Some(label);
                state.window.request_redraw();
                return Break(());
            }
            // Alternate screen: fall through to the raw-byte path.
        }
    }
    Continue(())
}
