//! Clipboard paste into a pane: the OS clipboard read and the bracketed-paste forwarders.

use super::*;

/// Read the OS clipboard as text. Returns `None` (logging at warn) on
/// clipboard failure or empty contents so callers fall through without
/// panicking. winit does not deliver paste events on Windows (see
/// `Cargo.toml`), so every paste path goes through an explicit clipboard
/// read.
pub(in crate::ui) fn read_clipboard_text() -> Option<String> {
    let mut cb = match arboard::Clipboard::new() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "clipboard.new failed — paste dropped");
            return None;
        }
    };
    match cb.get_text() {
        Ok(t) if !t.is_empty() => Some(t),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(error = %e, "clipboard.get_text failed — paste dropped");
            None
        }
    }
}

/// Wrap clipboard text in the bracketed-paste envelope. Newlines are
/// normalized to `\r` (matching the Enter handlers that drive the ptys) so a
/// paste behaves exactly like the user retyping the text; the
/// `\e[200~ ... \e[201~` envelope tells the receiving CLI "this is one
/// paste, don't run line-by-line".
pub(in crate::ui) fn bracketed_paste_bytes(text: &str) -> Vec<u8> {
    let normalized: String = text.replace("\r\n", "\r").replace('\n', "\r");
    let mut bytes: Vec<u8> = Vec::with_capacity(normalized.len() + 12);
    bytes.extend_from_slice(b"\x1b[200~");
    bytes.extend_from_slice(normalized.as_bytes());
    bytes.extend_from_slice(b"\x1b[201~");
    bytes
}

/// Read the OS clipboard and forward it to the session pane as one
/// bracketed-paste blob, via `send_pane_input` (capsule client / pending
/// buffer / daemon `pty.write`, keyed on `pane_feed` — ADR 0042 slice
/// L1b). Returns `false` only when the OS clipboard itself couldn't be
/// read; once bytes exist, `send_pane_input` never signals failure back
/// (a closed channel is logged internally, same as every other
/// `pty.write` call site in this file).
pub(in crate::ui) fn forward_clipboard_paste_to_llm(state: &mut State) -> bool {
    let Some(text) = read_clipboard_text() else {
        return false;
    };
    let bytes = bracketed_paste_bytes(&text);
    if let Some(t) = state.pane_attach_term.as_mut() {
        t.screen_mut().set_scrollback(0);
    }
    // ADR 0042 slice L1b fix 2/3: routed through the ONE session-pane
    // input dispatcher (capsule client / pending buffer / daemon
    // `pty.write`, keyed on `pane_feed`) — see `send_pane_input`'s own
    // doc.
    state.send_pane_input(&bytes);
    true
}

/// Read the OS clipboard and forward it to the local Terminal drawer's pty as
/// one bracketed-paste blob. Mirrors `forward_clipboard_paste_to_llm` but
/// targets the in-process `local_term` rather than the remote pty.
pub(in crate::ui) fn forward_clipboard_paste_to_local_term(state: &mut State) {
    let Some(text) = read_clipboard_text() else {
        return;
    };
    let bytes = bracketed_paste_bytes(&text);
    if let Some(t) = state.local_term.as_mut() {
        t.send_input(&bytes);
        t.screen_mut().set_scrollback(0);
    } else {
        #[cfg(windows)]
        if let Some(t) = state.attach_term.as_mut() {
            t.send_input(&bytes);
            t.screen_mut().set_scrollback(0);
        }
    }
}
