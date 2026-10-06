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
    let keep = match state.nav_prompt { Some(NavPrompt::ConfirmQuit { keep }) => Some(keep), _ => None };
    input_continuation(state, &event.logical_key, event.repeat, is_synthetic, event.state == ElementState::Pressed,
        keep, |state| prepare_keyboard_input(state, modifiers, &event), |state, route| match route {
            InputRoute::Prompt(step) => confirm_quit_key(state, event_loop, step),
            InputRoute::Next(action, (label, context, was_destroy_pending)) => {
                let key = KeyPress { event: &event, action, ctrl: modifiers.control_key(), alt: modifiers.alt_key(),
                    shift: modifiers.shift_key(), super_: modifiers.super_key() };
                let _ = route_key(state, event_loop, key, label, context, was_destroy_pending);
            }
        });
}

/// The raw-event boundary used by KeyboardInput and headless routing tests.
fn input_continuation<S, P>(
    state: &mut S, logical: &Key, repeat: bool, synthetic: bool, pressed: bool, keep: Option<bool>,
    resolve: impl FnOnce(&mut S) -> (Option<Action>, P), next: impl FnOnce(&mut S, InputRoute<P>),
) {
    if synthetic || !pressed { return; }
    if let Some(keep) = keep {
        let action = resolve(state).0;
        let bound_key = if *logical == Key::Named(NamedKey::Tab) { Key::Named(NamedKey::Tab) }
            else if action == Some(Action::Confirm) { Key::Named(NamedKey::Enter) }
            else { Key::Named(NamedKey::Escape) };
        next(state, InputRoute::Prompt(quit_prompt_step(keep, &bound_key, repeat)));
        return;
    }
    if matches!(logical, Key::Named(NamedKey::Control | NamedKey::Shift |
        NamedKey::Alt | NamedKey::Super | NamedKey::Meta | NamedKey::AltGraph)) { return; }
    let (action, prepared) = resolve(state);
    next(state, InputRoute::Next(action, prepared));
}

enum InputRoute<P> { Prompt(QuitPromptStep), Next(Option<Action>, P) }

fn prepare_keyboard_input(state: &mut State, modifiers: ModifiersState, event: &KeyEvent) -> (Option<Action>, (String, help::Context, Option<WsKey>)) {
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
    (action, (label, context, was_destroy_pending))
}

/// Routes one keypress through the layers in their fixed order; `Break` ends the keypress, `Continue` hands it on.
fn route_key(state: &mut State, event_loop: &ActiveEventLoop, key: KeyPress<'_>, label: String, context: help::Context, was_destroy_pending: Option<WsKey>) -> ControlFlow<()> {
    let KeyPress { event, ctrl, alt, shift, super_, .. } = key;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn route(keep: Option<bool>, logical: Key, m: Modifiers, repeat: bool, synthetic: bool,
        pressed: bool, rebound: bool) -> (Option<QuitPromptStep>, usize, Vec<u8>) {
        let mut bindings = KeyBindings::defaults();
        if rebound { bindings.merge_text("input.confirm = \"F11\""); }
        let context = help::Context { prompt: keep.is_some(), ..Default::default() };
        let mut observed = (None, 0, Vec::new());
        input_continuation(&mut observed, &logical, repeat, synthetic, pressed, keep,
            |_| (bindings.resolve(&logical, Some(&logical), m, context.consumes_text(), |a| context.allows(a)), ()),
            |seen, route| match route {
                InputRoute::Prompt(step) => seen.0 = Some(step),
                InputRoute::Next(_, ()) => {
                    seen.1 += 1;
                    if let Key::Character(c) = &logical { seen.2.extend_from_slice(c.as_bytes()); }
                }
            });
        observed
    }

    fn modifiers() -> Modifiers { Modifiers { ctrl: false, alt: false, shift: false, super_: false } }

    #[test]
    fn quit_other_keys_cancel_and_are_consumed() {
        for keep in [false, true] {
            for key in [Key::Character("typed-fixture-text".into()), Key::Named(NamedKey::F12), Key::Named(NamedKey::Escape)] {
                let (step, calls, bytes) = route(Some(keep), key, modifiers(), false, false, true, false);
                assert_eq!(step, Some(QuitPromptStep::Cancel), "other key must cancel");
                assert_eq!((calls, bytes), (0, vec![]), "trigger reached downstream input");
            }
        }
    }

    #[test]
    fn quit_enter_and_tab_use_identity_with_modifiers_and_rebindings() {
        println!("T1 body entered: ui::input::keypress::tests::quit_enter_and_tab_use_identity_with_modifiers_and_rebindings");
        for keep in [false, true] {
            for rebound in [false, true] {
                for bits in 0..16 {
                    let m = Modifiers { ctrl: bits & 1 != 0, alt: bits & 2 != 0,
                        shift: bits & 4 != 0, super_: bits & 8 != 0 };
                    for (key, want) in [(NamedKey::Tab, QuitPromptStep::Stay { keep: !keep }),
                        (NamedKey::Enter, QuitPromptStep::Leave(if keep { LeaveIntent::Keep } else { LeaveIntent::Close }))] {
                        let (step, calls, bytes) = route(Some(keep), Key::Named(key), m, false, false, true, rebound);
                        println!("T1 fixture observed: routed raw key and downstream recorder");
                        assert_eq!(step, Some(want), "logical Tab/Enter must win");
                        assert_eq!((calls, bytes), (0, vec![]));
                    }
                }
            }
        }
        println!("T1 assertion passed: logical Tab/Enter must win");
    }

    #[test]
    fn quit_modifier_presses_cancel_before_the_filter() {
        for keep in [false, true] {
            for key in [NamedKey::Control, NamedKey::Shift, NamedKey::Alt, NamedKey::Super, NamedKey::Meta, NamedKey::AltGraph] {
                let seen = route(Some(keep), Key::Named(key), modifiers(), false, false, true, false);
                assert_eq!(seen, (Some(QuitPromptStep::Cancel), 0, vec![]), "modifier must cancel before suppression");
            }
        }
    }

    #[test]
    fn raw_admission_and_repeat_controls() {
        for keep in [false, true] {
            for key in [Key::Named(NamedKey::Enter), Key::Named(NamedKey::Control), Key::Character("text".into())] {
                assert_eq!(route(Some(keep), key.clone(), modifiers(), false, true, true, false), (None, 0, vec![]));
                assert_eq!(route(Some(keep), key.clone(), modifiers(), false, false, false, false), (None, 0, vec![]));
                // Modifier repeats are rejected before the prompt at the parent; every accepted repeat ignores.
                let seen = route(Some(keep), key, modifiers(), true, false, true, false);
                assert!(seen.0.is_none() || seen.0 == Some(QuitPromptStep::Ignore));
                assert_eq!((seen.1, seen.2), (0, vec![]));
            }
        }
        assert_eq!(route(None, Key::Named(NamedKey::Control), modifiers(), false, false, true, false), (None, 0, vec![]));
        assert_eq!(route(None, Key::Character("text".into()), modifiers(), false, false, true, false), (None, 1, b"text".to_vec()));
    }
}
