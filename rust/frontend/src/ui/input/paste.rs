//! The OS clipboard: the window's one handle on it, its read and write, and the bracketed-paste forwarders.

use super::*;
use std::cell::RefCell;

thread_local! {
    /// The window's one clipboard handle, made at its first use and kept for the process's life: on Linux (X11) the
    /// text a window copies is served by the handle that set it, so a handle dropped at once leaves nothing to paste
    /// unless a clipboard manager took it (arboard's rule). Every copy and paste runs on the UI thread, through it.
    static CLIPBOARD: RefCell<Option<arboard::Clipboard>> = const { RefCell::new(None) };
}

fn with_clipboard<T>(use_it: impl FnOnce(&mut arboard::Clipboard) -> Result<T, arboard::Error>) -> Result<T, arboard::Error> {
    CLIPBOARD.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Some(arboard::Clipboard::new()?);
        }
        use_it(slot.as_mut().expect("the handle was just made"))
    })
}

/// Put `text` on the OS clipboard through the window's one handle; a handle whose write failed is made again at the
/// next use.
pub(in crate::ui) fn write_clipboard_text(text: String) -> Result<(), arboard::Error> {
    let written = with_clipboard(|cb| cb.set_text(text));
    if written.is_err() {
        CLIPBOARD.with(|cell| cell.borrow_mut().take());
    }
    written
}

/// Read the OS clipboard as text. Returns `None` (logging at warn) on
/// clipboard failure or empty contents so callers fall through without
/// panicking. winit does not deliver paste events on Windows (see
/// `Cargo.toml`), so every paste path goes through an explicit clipboard
/// read.
pub(in crate::ui) fn read_clipboard_text() -> Option<String> {
    match with_clipboard(|cb| cb.get_text()) {
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
    state.agent_pane.reset_scroll();
    // The pane owner sends to its selected client or counts this call as discarded.
    state.send_pane_input(&bytes);
    true
}

/// Read the OS clipboard and forward it to the local Terminal drawer's pty as
/// one bracketed-paste blob. Mirrors `forward_clipboard_paste_to_llm` but
/// targets the in-process local terminal rather than the remote pty.
pub(in crate::ui) fn forward_clipboard_paste_to_local_term(state: &mut State) {
    let Some(text) = read_clipboard_text() else {
        return;
    };
    let bytes = bracketed_paste_bytes(&text);
    state.term_drawer.type_input(&bytes);
}
