//! The layers a key meets before the focused pane: the quit prompt, help, and the keys every pane shares.

use super::*;
use std::ops::ControlFlow::{self, Break, Continue};
use crate::ui::input::keypress::KeyPress;

#[derive(Default, Debug, PartialEq, Eq)]
pub(in crate::ui) struct LayoutEffect {
    pub(super) focus: Option<PaneFocus>,
    pub(super) monitor_sync: bool,
    pub(super) redraw: bool,
}
/// Effect continuation shared by the window and portable recorders. Label feedback follows Monitor sync.
pub(in crate::ui) trait LayoutPort {
    fn layout_focus(&mut self, focus: PaneFocus);
    fn layout_monitor_sync(&mut self);
    fn layout_label(&mut self, label: String);
    fn layout_redraw(&mut self);
}
impl LayoutEffect {
    pub(in crate::ui) fn apply(self, port: &mut impl LayoutPort, label: Option<String>) {
        if let Some(focus) = self.focus { port.layout_focus(focus); }
        if self.monitor_sync { port.layout_monitor_sync(); }
        if let Some(label) = label { port.layout_label(label); }
        if self.redraw { port.layout_redraw(); }
    }
}

#[derive(Default)]
pub(in crate::ui) struct HelpKeyEffect {
    pub(super) consumed: bool,
    pub(super) dismissed_peek: bool,
    pub(super) layout: Option<LayoutEffect>,
    pub(super) browser: Option<HelpBrowser>,
    pub(super) browsing_redraw: bool,
}
pub(in crate::ui) enum HelpBrowser { Manual(Action), CheatSheet(String) }
pub(in crate::ui) trait HelpBrowserPort: LayoutPort {
    fn help_manual(&mut self, url: &str) -> std::result::Result<(), String>;
    fn help_html(&mut self, html: &[u8]) -> std::result::Result<(), String>;
    fn help_status(&mut self, status: String);
}
/// Browser/status work runs before the browsing redraw, including on failure.
pub(in crate::ui) fn finish_help_key(effect: HelpKeyEffect, port: &mut impl HelpBrowserPort) -> std::ops::ControlFlow<()> {
    if effect.dismissed_peek { port.layout_redraw(); }
    if let Some(layout) = effect.layout { layout.apply(port, None); }
    match effect.browser {
        Some(HelpBrowser::Manual(action)) => {
            if let Err(e) = port.help_manual(help::manual_url(action)) {
                port.help_status(format!("Open help manual failed: {e}"));
            }
        }
        Some(HelpBrowser::CheatSheet(html)) => {
            if let Err(e) = port.help_html(html.as_bytes()) {
                tracing::warn!(target: "sot::ui::input::global_keys", error = %e, "help cheat sheet: open_html_in_browser failed");
                port.help_status(format!("Print cheat sheet failed: {e}"));
            } else { port.help_status("cheat sheet · opened in browser".to_string()); }
        }
        None => {}
    }
    if effect.browsing_redraw { port.layout_redraw(); }
    if effect.consumed { std::ops::ControlFlow::Break(()) } else { std::ops::ControlFlow::Continue(()) }
}

pub(in crate::ui) trait AgentRevealPort: LayoutPort { fn layout_paste(&mut self, bytes: &[u8]); }
pub(in crate::ui) fn finish_agent_reveal(effect: LayoutEffect, port: &mut impl AgentRevealPort, paste: Option<&[u8]>) {
    effect.apply(port, None);
    if let Some(bytes) = paste { port.layout_paste(bytes); }
}

pub(in crate::ui) fn confirm_quit_key(state: &mut State, event_loop: &ActiveEventLoop, step: QuitPromptStep) {
    let (effect, leave) = state.focus_prompts.answer_quit(step);
    state.apply_prompt_effect(effect);
    if let Some(intent) = leave { state.leave(event_loop, intent, 0); }
}

pub(in crate::ui) fn help_key(state: &mut State, key: KeyPress<'_>, context: help::Context) -> ControlFlow<()> {
    let KeyPress { event, action, ctrl, super_, alt, shift } = key;
    let effect = state.layout_help.help_key(layout_help::HelpKey {
        logical_key: &event.logical_key, action, repeat: event.repeat, modifiers: Modifiers { ctrl, super_, alt, shift },
    }, context, state.focus_prompts.focus(), &state.bindings, std::time::Instant::now);
    layout_help::finish_help_key(effect, state)
}

/// Ctrl+Q: quit from any pane `Scope::Quit` allows. In the agent pane and the Terminal drawer a program keeps the
/// chord where it reaches the pty (XON, readline's quoted-insert, vim's literal insert), so it goes on and the status
/// says where to press it instead.
pub(in crate::ui) fn quit_chord(state: &mut State, event_loop: &ActiveEventLoop, key: KeyPress<'_>, context: help::Context) -> ControlFlow<()> {
    let KeyPress { event, action, ctrl, alt, shift, super_ } = key;
    let base = event.key_without_modifiers();
    let bound_quit = || state.bindings.resolve(&event.logical_key, Some(&base), Modifiers { ctrl, alt, shift, super_ },
        context.consumes_text(), |a| a == Action::Quit).is_some();
    let route = quit_chord_route(action, event.repeat, context.pane, bound_quit);
    quit_chord_continuation(state, route, |state| state.request_quit(event_loop, ExitReason::QuitKey), |state| {
        let chord = state.bindings.first_label(Action::Quit);
        state.focus_prompts.show_hint(&chord, std::time::Instant::now);
        state.window.request_redraw();
    }, |_| Continue(()))
}

#[derive(Clone, Copy)]
pub(in crate::ui) enum QuitChordRoute { Request, Consume, Hint, Next }

pub(in crate::ui) fn quit_chord_route(action: Option<Action>, repeat: bool, pane: help::Pane,
    bound_quit: impl FnOnce() -> bool) -> QuitChordRoute {
    if action == Some(Action::Quit) {
        return if repeat { QuitChordRoute::Consume } else { QuitChordRoute::Request };
    }
    if !repeat && matches!(pane, help::Pane::Agent | help::Pane::Terminal) && bound_quit() {
        return QuitChordRoute::Hint;
    }
    QuitChordRoute::Next
}

/// A rejected quit chord installs its hint and still reaches the next input layer.
pub(in crate::ui) fn quit_chord_continuation<S>(state: &mut S, route: QuitChordRoute,
    request: impl FnOnce(&mut S), hint: impl FnOnce(&mut S), next: impl FnOnce(&mut S) -> ControlFlow<()>) -> ControlFlow<()> {
    match route {
        QuitChordRoute::Request => { request(state); Break(()) }
        QuitChordRoute::Consume => Break(()),
        QuitChordRoute::Hint => { hint(state); next(state) }
        QuitChordRoute::Next => next(state),
    }
}

/// The status when the quit chord, as the platform and the user's bindings label it, did not close the window: it is
/// not where a program keeps it on every platform (Cmd chords never reach a pty), so it says what the window did.
pub(in crate::ui) fn quit_hint(chord: &str) -> String {
    format!("{chord} does not close the window from this pane; press it in the file or preview pane")
}

pub(in crate::ui) fn window_chords(state: &mut State, key: KeyPress<'_>, label: String) -> ControlFlow<()> {
    let KeyPress { event, action, .. } = key;
    // F5: manual reconnect trigger — collapses the
    // transport's current reconnect wait and retries
    // immediately, from the wait's floor (`Redial::reset`).
    // Works from any focus, no modifier, so it's there when
    // wifi comes back and the user doesn't want to wait out a
    // wait that doubles to 5 s on a local socket, 30 s on ssh.
    if !event.repeat
        && action == Some(Action::Reconnect)
    {
        // ADR 0042 L2a (Codex review, PR #163): ONE shared
        // `reconnect_now` Arc<Notify> is cloned into EVERY
        // host's transport::spawn task, so multiple hosts can
        // simultaneously be sitting in their own backoff sleep
        // when F5 fires. `notify_one()` wakes at most ONE of
        // them (arbitrary which); `notify_waiters()` wakes
        // every task CURRENTLY awaiting it, matching "reconnect
        // now" meaning every connection, not a coin flip.
        state.hosts.reconnect_now.notify_waiters();
        state.last_key = Some(label);
        state.window.request_redraw();
        return Break(());
    }
    // F5 handled above (manual reconnect). F11: borderless
    // fullscreen toggle — standard cross-platform key for
    // this, no modifier, no conflict with anything we bind
    // (Ctrl+F clashes with readline forward-char in the
    // LLM shell, so we avoid it).
    if !event.repeat
        && action == Some(Action::ToggleFullscreen)
    {
        let entering_fullscreen = state.window.fullscreen().is_none();
        let new_fs = if entering_fullscreen {
            Some(Fullscreen::Borderless(None))
        } else {
            None
        };
        state.window.set_fullscreen(new_fs);
        // Surface the steady-redraw guard (see about_to_wait)
        // only when it's actually about to kick in — entering
        // fullscreen with the pin on. Nothing on the way out,
        // and nothing when the setting has opted it off.
        if entering_fullscreen && state.settings.fullscreen_vsync_pin {
            state.status.set("fullscreen: steady redraw for VRR panels ([display] fullscreen_vsync_pin = false to disable)");
            state.status.hold();
        }
        state.last_key = Some(label);
        state.window.request_redraw();
        return Break(());
    }
    // Ctrl+= / Ctrl+- / Ctrl+0: global font scale. Intercepted
    // first so they reach this handler even in LLM focus
    // (where most other Ctrl+letter bytes are forwarded to
    // the pty). +0.1 / -0.1 per press, reset to 1.0 on
    // Ctrl+0; clamped to [0.5, 3.0].
    // Font scale is keymap-driven (font.scale_up / _down / _reset).
    // Intercepted before per-pane dispatch so it works even in LLM
    // focus (where most Ctrl+letter bytes forward to the pty).
    if !event.repeat {
        if action == Some(Action::FontScaleUp) {
            state.apply_text_scale(state.text_scale_mult + 0.1);
            state.persist_resume_state();
            state.last_key = Some(label);
            state.window.request_redraw();
            return Break(());
        }
        if action == Some(Action::FontScaleDown) {
            state.apply_text_scale(state.text_scale_mult - 0.1);
            state.persist_resume_state();
            state.last_key = Some(label);
            state.window.request_redraw();
            return Break(());
        }
        if action == Some(Action::FontScaleReset) {
            state.apply_text_scale(1.0);
            state.persist_resume_state();
            state.last_key = Some(label);
            state.window.request_redraw();
            return Break(());
        }
    }
    Continue(())
}

pub(in crate::ui) fn navigation_chords(state: &mut State, key: KeyPress<'_>, label: String) -> ControlFlow<()> {
    let KeyPress { event, action, .. } = key;
    // Ctrl+Arrow: spatial pane focus move (4-way grid). The
    // arrow-only case stays per-pane (tree nav / no-op),
    // unmodified.
    if !event.repeat {
        // Spatial pane focus is keymap-driven (focus.pane_*); the
        // default Ctrl+Arrow chords keep it disjoint from plain
        // arrows (per-pane nav) and Shift+Arrow (workspace cycle).
        let dir = if action == Some(Action::FocusPaneRight) {
            Some(SpatialDir::Right)
        } else if action == Some(Action::FocusPaneLeft) {
            Some(SpatialDir::Left)
        } else if action == Some(Action::FocusPaneUp) {
            Some(SpatialDir::Up)
        } else if action == Some(Action::FocusPaneDown) {
            Some(SpatialDir::Down)
        } else {
            None
        };
        if let Some(dir) = dir {
            // move_in walks only laid-out panes, so focus never
            // reaches an invisible pty.
            let preset = state.settings.resolve_preset(state.monitor_aspect);
            let (columns, drawer) = state.layout_help.spatial_slots(preset);
            state.focus_prompts.move_focus(dir, &columns, drawer);
            // Keymap-driven label (Ctrl+Arrow on Windows/Linux,
            // Cmd+Arrow on macOS) instead of a hard-coded
            // "Ctrl+" prefix, which used to print "Ctrl+Left"
            // even once the chord was remapped.
            state.last_key = Some(state.bindings.first_label(
                action.expect("dir implies a resolved focus action"),
            ));
            state.window.request_redraw();
            return Break(());
        }
    }
    // Tab is intentionally NOT a focus switcher: it would
    // steal shell/REPL completion in the terminal panes.
    // Focus changes go through Ctrl+Arrow; Tab falls through
    // to the focused pane (forwarded to the pty as `\t`).
    // Shift+ArrowRight / Shift+ArrowLeft cycles the active
    // workspace forward / backward (ADR 0014 D7). Intercepted
    // globally — including LLM focus — so the user can flip
    // workspaces mid-shell-session without re-focusing the nav
    // pane. No-op when only the default workspace is registered.
    // `!event.repeat` so a held keypress doesn't blast through
    // every workspace; one switch per press. `!ctrl && !alt`
    // keeps it disjoint from Ctrl+Arrow (spatial pane move);
    // plain (unmodified) arrows still fall through to per-pane
    // nav. Trade-off: Shift+Arrow no longer reaches the pty in
    // the LLM / terminal panes (it previously forwarded a bare
    // arrow there).
    // Workspace cycle is keymap-driven (workspace.cycle_next /
    // workspace.cycle_prev). Suppressed in edit mode so it doesn't
    // hijack arrows in the editor; the default Shift+Arrow chords
    // keep it disjoint from Ctrl+Arrow (pane focus) above.
    if !event.repeat && !state.editor.is_editing() {
        if action == Some(Action::WorkspaceCycleNext) {
            state.cycle_workspace(1, true);
            state.last_key = Some(label);
            return Break(());
        }
        if action == Some(Action::WorkspaceCyclePrev) {
            state.cycle_workspace(-1, true);
            state.last_key = Some(label);
            return Break(());
        }
    }
    Continue(())
}

pub(in crate::ui) fn layout_chords(state: &mut State, key: KeyPress<'_>, label: String) -> ControlFlow<()> {
    let KeyPress { action, .. } = key;
    let context = state.help_context();
    let gates = state.layout_gates();
    if let Some(effect) = state.layout_help.layout_action(action, &context, state.focus_prompts.focus(), gates) {
        effect.apply(state, Some(label));
        return Break(());
    }
    // Ctrl+Shift+S: whole-window selfie to a timestamped PNG.
    // Handled here in the global-chord region so it fires from
    // ANY pane — including the terminal/REPL drawers, before
    // keystrokes route into a pty. The readback runs in the
    // render loop on the next frame (request_redraw below).
    if action == Some(Action::Selfie)
    {
        state.selfie_pending = Some(selfie_path());
        state.last_key = Some(label);
        state.window.request_redraw();
        return Break(());
    }
    Continue(())
}

pub(in crate::ui) fn drawer_chords(state: &mut State, key: KeyPress<'_>, label: String) -> ControlFlow<()> {
    let KeyPress { action, .. } = key;
    // Ctrl+J: toggle the REPL drawer. VS Code's panel-toggle convention; reads
    // intuitively as "show me the bottom panel". When
    // the drawer opens, focus moves into it so the user
    // can immediately type. When it closes, focus
    // bounces back to NavTree (the most useful default
    // landing pane).
    // Ctrl+J (Repl) and Ctrl+T (Terminal) are symmetric:
    // each opens its own drawer content, swaps to it if the
    // other is showing, and closes if its own is already
    // showing. Both share the `PaneFocus::Repl` drawer slot;
    // `state.layout_help.drawer()` decides which content renders (and, per
    // G4, where keystrokes route). When the drawer is open
    // focus moves into it; when it closes from the drawer,
    // focus bounces back to NavTree.
    // Drawer toggles are keymap-driven (.sot/keybindings.toml:
    // drawer.repl / drawer.terminal / drawer.monitor) so the
    // chords reconfigure without a recompile. Defaults Ctrl+j /
    // Ctrl+t / Ctrl+m preserve the prior behaviour.
    let drawer_key = if action == Some(Action::ToggleReplDrawer) {
        Some(DrawerContent::Repl)
    } else if action == Some(Action::ToggleTerminalDrawer) {
        Some(DrawerContent::Terminal)
    } else if action == Some(Action::ToggleMonitorDrawer) {
        Some(DrawerContent::Monitor)
    } else {
        None
    };
    if let Some(slot) = drawer_key {
        let effect = state.layout_help.toggle_drawer(slot, state.focus_prompts.focus());
        effect.apply(state, Some(label));
        return Break(());
    }
    Continue(())
}

pub(in crate::ui) fn scroll_line_chords(state: &mut State, key: KeyPress<'_>) -> ControlFlow<()> {
    let KeyPress { action, .. } = key;
    // Alt+Up / Alt+Down: fine-grained one-row scroll in the
    // focused pane. Shared across REPL and Preview here so
    // the rule reads in one place. NavTree is cursor-driven
    // (manual scroll would desync) and LLM passes alt+arrow
    // through to the pty so tmux/shell keep alt-keybinds.
    if matches!(action, Some(Action::ScrollLineUp | Action::ScrollLineDown)) {
        let row_step: i32 = 1;
        match (state.focus_prompts.focus(), action) {
            (PaneFocus::Repl, Some(Action::ScrollLineUp)) => {
                state.repl_model.scroll_line(true);
                state.window.request_redraw();
                return Break(());
            }
            (PaneFocus::Repl, Some(Action::ScrollLineDown)) => {
                state.repl_model.scroll_line(false);
                state.window.request_redraw();
                return Break(());
            }
            (PaneFocus::Preview, Some(Action::ScrollLineUp)) => {
                state.shaped.scroll_up(row_step as u16);
                state.window.request_redraw();
                return Break(());
            }
            (PaneFocus::Preview, Some(Action::ScrollLineDown)) => {
                state.shaped.scroll_down(row_step as u16);
                state.window.request_redraw();
                return Break(());
            }
            _ => {}
        }
    }
    Continue(())
}

pub(in crate::ui) fn table_scroll_key(state: &mut State, key: KeyPress<'_>) -> ControlFlow<()> {
    let KeyPress { action, .. } = key;
    // Wide-table horizontal scroll: h/l step the shared
    // the table scroll by one body-em (≈ the width of
    // one monospace cell). `0` resets to scroll-left.
    // Plain keys (no modifier) so the binding is one-handed
    // and fast; ignored unless the focus is Preview so the
    // letters stay typeable in LLM/REPL. Only does
    // anything when the current doc actually contains a
    // table wider than the preview pane; otherwise the
    // redraw clamp keeps scroll at 0.
    if state.focus_prompts.focus() == PaneFocus::Preview
        && !state.raster.is_shown()
        && !state.editor.is_editing()
    {
        let step = state.shaped.md().body_em().max(8.0);
        match action {
            Some(Action::TableLeft) => { state.md_media.scroll_tables_left(step); state.window.request_redraw(); return Break(()); }
            Some(Action::TableRight) => { state.md_media.scroll_tables_right(step); state.window.request_redraw(); return Break(()); }
            Some(Action::TableReset) => { state.md_media.reset_table_scroll(); state.window.request_redraw(); return Break(()); }
            _ => {}
        }
    }
    Continue(())
}
