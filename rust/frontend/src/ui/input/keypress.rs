//! One keypress, from the key event to the layer that takes it.

use super::*;
use std::ops::ControlFlow::{self, Break, Continue};
use winit::event::KeyEvent;
use winit::keyboard::ModifiersState;
use crate::ui::input::global_keys::{confirm_quit_key, drawer_chords, help_key, layout_chords, navigation_chords, scroll_line_chords, table_scroll_key, window_chords};
use crate::ui::nav::keys::nav_tree_key;
use crate::ui::drawer::keys::drawer_pane_key;
use crate::ui::preview::keys::preview_key;

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
            drawer_pane_key(state, key, label.clone())?;
        }
        PaneFocus::Preview => {
            preview_key(state, key, label.clone())?;
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
