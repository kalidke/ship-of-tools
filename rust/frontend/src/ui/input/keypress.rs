//! One keypress, from the key event to the layer that takes it.

use super::*;
use std::ops::ControlFlow::{self, Continue};
use winit::event::KeyEvent;
use winit::keyboard::ModifiersState;
use crate::ui::input::global_keys::{confirm_quit_key, drawer_chords, help_key, layout_chords, navigation_chords, scroll_line_chords, table_scroll_key, window_chords};
use crate::ui::nav::keys::nav_tree_key;
use crate::ui::drawer::keys::drawer_pane_key;
use crate::ui::preview::keys::preview_key;
use crate::ui::agent_pane::keys::agent_pane_key;

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
    let KeyPress { event, ctrl, alt, shift, super_, .. } = key;
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
            agent_pane_key(state, key, label.clone())?;
        }
    }
    state.last_key = Some(label);
    state.window.request_redraw();
    Continue(())
}
