//! The layers a key meets before the focused pane: the quit prompt, help, and the keys every pane shares.

use super::*;
use std::ops::ControlFlow::{self, Break, Continue};
use crate::ui::input::keypress::KeyPress;

pub(in crate::ui) fn confirm_quit_key(state: &mut State, event_loop: &ActiveEventLoop, key: KeyPress<'_>) -> ControlFlow<()> {
    let KeyPress { event, action, .. } = key;
    // The Ctrl+Q prompt owns the keyboard while it is open: it
    // reads every key before any global binding (`prompt_takes_key`).
    if let Some(NavPrompt::ConfirmQuit { keep }) = &state.nav_prompt {
        let tab = matches!(event.logical_key, Key::Named(NamedKey::Tab));
        match prompt_takes_key(*keep, tab, action, event.repeat) {
            QuitPromptStep::Stay { keep } => {
                state.nav_prompt = Some(NavPrompt::ConfirmQuit { keep });
                state.window.request_redraw();
            }
            QuitPromptStep::Cancel => state.cancel_nav_prompt(),
            QuitPromptStep::Leave(i) => state.leave(event_loop, i, 0),
            QuitPromptStep::Ignore => {}
        }
        return Break(());
    }
    Continue(())
}

pub(in crate::ui) fn help_key(state: &mut State, key: KeyPress<'_>, context: help::Context) -> ControlFlow<()> {
    let KeyPress { event, action, ctrl, super_, .. } = key;
    if !event.repeat && action == Some(Action::ToggleHelpDrawer) {
        if state.drawer == DrawerContent::Help { state.close_help_drawer(); }
        else { state.open_help_drawer(context); }
        return Break(());
    }
    if action == Some(Action::ToggleHelp) {
        tracing::debug!(repeat = event.repeat, peek = state.help.peek.is_some(), ?context, "context help requested");
        if event.repeat { return Break(()); }
        if state.drawer == DrawerContent::Help && state.focus == PaneFocus::Repl {
            state.close_help_drawer();
        } else if let Some(peek) = state.help.peek.take() {
            state.open_help_drawer(peek.context);
        } else {
            state.help.peek = Some(help::Peek { context, started: std::time::Instant::now() });
            state.window.request_redraw();
        }
        return Break(());
    }
    if state.help.peek.take().is_some() {
        state.window.request_redraw();
        if event.logical_key == Key::Named(NamedKey::Escape) { return Break(()); }
    }
    // Browsing Help consumes its own input; no typed search leaks into Julia.
    if state.drawer == DrawerContent::Help && state.focus == PaneFocus::Repl
        && !action.is_some_and(|a| matches!(a.spec().scope,
            crate::ui::input::keybindings::Scope::Global | crate::ui::input::keybindings::Scope::Workspace |
            crate::ui::input::keybindings::Scope::Restore))
    {
        tracing::debug!(?event.logical_key, ?action, "help drawer key");
        match &event.logical_key {
            _ if action == Some(Action::HelpClose) => state.close_help_drawer(),
            _ if action == Some(Action::HelpUp) => state.help.move_selection(-1, &state.bindings),
            _ if action == Some(Action::HelpDown) => state.help.move_selection(1, &state.bindings),
            _ if action == Some(Action::HelpPageUp) => state.help.move_selection(-8, &state.bindings),
            _ if action == Some(Action::HelpPageDown) => state.help.move_selection(8, &state.bindings),
            _ if action == Some(Action::HelpScope) && !event.repeat => { state.help.all_panes = !state.help.all_panes; state.help.selected = 0; }
            Key::Named(NamedKey::Backspace) => { state.help.query.pop(); state.help.selected = 0; }
            _ if action == Some(Action::HelpManual) && !event.repeat => {
                if let Some(a) = state.help.selected_action(&state.bindings) {
                    if let Err(e) = open_url_in_browser(help::manual_url(a)) {
                        state.status = format!("Open help manual failed: {e}");
                    }
                }
            }
            // Generated fresh from `state.bindings`, no fs source of its
            // own -- same temp-file-then-browser route as the Quarto
            // quick-render and sourceless-preview `o` (open_html_in_browser).
            _ if action == Some(Action::HelpCheatSheet) && !event.repeat => {
                let html = help::cheat_sheet_html(&state.bindings);
                if let Err(e) = open_html_in_browser(html.as_bytes()) {
                    tracing::warn!(error = %e, "help cheat sheet: open_html_in_browser failed");
                    state.status = format!("Print cheat sheet failed: {e}");
                } else {
                    state.status = "cheat sheet · opened in browser".to_string();
                }
            }
            Key::Character(c) if !ctrl && !super_ => { state.help.query.push_str(c); state.help.selected = 0; }
            Key::Named(NamedKey::Space) => { state.help.query.push(' '); state.help.selected = 0; }
            _ => {}
        }
        state.window.request_redraw();
        return Break(());
    }
    Continue(())
}

pub(in crate::ui) fn window_chords(state: &mut State, key: KeyPress<'_>, label: String) -> ControlFlow<()> {
    let KeyPress { event, action, .. } = key;
    // F5: manual reconnect trigger — collapses the
    // transport's current backoff sleep and retries
    // immediately. Works from any focus, no modifier, so
    // it's there when wifi comes back and the user
    // doesn't want to wait the up-to-5s backoff cap.
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
            state.status =
                "fullscreen: steady redraw for VRR panels ([display] fullscreen_vsync_pin = false to disable)"
                    .to_string();
            state.notify_sticky_until = Some(std::time::Instant::now() + NOTIFY_STICKY);
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
            let columns = if state.wide_preview {
                preset.wide_preview().columns
            } else {
                preset.columns.clone()
            };
            // The slot redraw lays out in the drawer; Help borrows Repl's when the preset has none.
            let drawer = match state.drawer {
                DrawerContent::Closed => None,
                DrawerContent::Help => preset.drawer.or(Some(crate::ui::persist::settings::Slot::Repl)),
                _ => preset.drawer,
            };
            state.set_focus(state.focus.move_in(dir, &columns, drawer));
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
    if !event.repeat && state.edit_state.is_none() {
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
    if action == Some(Action::MaximizePane) {
        state.maximized = true;
        state.last_key = Some(label);
        state.window.request_redraw();
        return Break(());
    }
    if state.maximized
        && action == Some(Action::RestoreLayout)
    {
        state.maximized = false;
        state.last_key = Some(label);
        state.window.request_redraw();
        return Break(());
    }
    // Esc also exits wide-preview — the same "get me back"
    // gesture as un-maximize. Ordered after the maximize
    // restore so layered states peel one at a time
    // (un-maximize first, un-widen second). Unlike maximize,
    // wide-preview is sticky — the user lives in it — so this
    // is gated to the reading panes with no modal up: a
    // vim/readline Esc in the drawer pty must keep reaching
    // the pty, and picker / prompt / annotation-edit Esc must
    // keep cancelling those first.
    if state.wide_preview
        && !state.maximized
        && state.edit_state.is_none()
        && state.nav_prompt.is_none()
        && state.workspace_picker.is_none()
        && matches!(state.focus, PaneFocus::NavTree | PaneFocus::Preview)
        && action == Some(Action::RestoreLayout)
    {
        state.wide_preview = false;
        state.last_key = Some(label);
        state.window.request_redraw();
        return Break(());
    }
    // Wide-preview toggle (layout.wide_preview, default
    // Alt++ — the shifted neighbour of Alt+= maximize):
    // hide the LLM column and hand its width to the
    // preview. Global like maximize — fires from any focus,
    // including LLM (pane management wins over forwarding
    // the chord to the shell). Focus on the pane being
    // hidden bounces to Preview, same rule as the
    // drawer-close bounce.
    if action == Some(Action::ToggleWidePreview) {
        state.wide_preview = !state.wide_preview;
        if state.wide_preview && state.focus == PaneFocus::Llm {
            state.set_focus(PaneFocus::Preview);
        }
        state.last_key = Some(label);
        state.window.request_redraw();
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
    // `state.drawer` decides which content renders (and, per
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
        state.help_origin = None;
        state.drawer = state.drawer.toggle(slot);
        if state.drawer.is_open() {
            state.set_focus(PaneFocus::Repl);
        } else if state.focus == PaneFocus::Repl {
            state.set_focus(PaneFocus::NavTree);
        }
        // Monitor drawer subscribe/unsubscribe lifecycle (ADR
        // 0020): subscribe + prefill on open, unsubscribe on
        // close. Backend sampling is always-on; this just gates
        // this connection's live stream to when the drawer is up.
        // ADR 0042 L2a: always `monitor_host` (2.1, the
        // declared hub) — the drawer never follows
        // `active_host`.
        if state.drawer == DrawerContent::Monitor && !state.monitor_view.subscribed
        {
            let monitor_host = state.monitor_host();
            let _ = state.send_to(
                &monitor_host,
                crate::net::transport::OutgoingReq::MonitorSubscribe,
            );
            let _ = state.send_to(
                &monitor_host,
                crate::net::transport::OutgoingReq::MonitorHistory {
                    window_s: 300.0,
                    points: 300,
                    until: None,
                    host: None,
                },
            );
            state.monitor_view.subscribed = true;
            state.monitor_dirty = true;
        } else if state.drawer != DrawerContent::Monitor
            && state.monitor_view.subscribed
        {
            let monitor_host = state.monitor_host();
            let _ = state.send_to(
                &monitor_host,
                crate::net::transport::OutgoingReq::MonitorUnsubscribe,
            );
            state.monitor_view.subscribed = false;
        }
        state.last_key = Some(label);
        state.window.request_redraw();
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
        match (state.focus, action) {
            (PaneFocus::Repl, Some(Action::ScrollLineUp)) => {
                state.repl_scroll = state.repl_scroll.saturating_add(row_step as u16);
                state.window.request_redraw();
                return Break(());
            }
            (PaneFocus::Repl, Some(Action::ScrollLineDown)) => {
                state.repl_scroll = state.repl_scroll.saturating_sub(row_step as u16);
                state.window.request_redraw();
                return Break(());
            }
            (PaneFocus::Preview, Some(Action::ScrollLineUp)) => {
                state.preview_scroll =
                    state.preview_scroll.saturating_sub(row_step as u16);
                state.window.request_redraw();
                return Break(());
            }
            (PaneFocus::Preview, Some(Action::ScrollLineDown)) => {
                state.preview_scroll =
                    state.preview_scroll.saturating_add(row_step as u16);
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
    // `md_table_scroll_px` by one body-em (≈ the width of
    // one monospace cell). `0` resets to scroll-left.
    // Plain keys (no modifier) so the binding is one-handed
    // and fast; ignored unless the focus is Preview so the
    // letters stay typeable in LLM/REPL. Only does
    // anything when the current doc actually contains a
    // table wider than the preview pane; otherwise the
    // redraw clamp keeps scroll at 0.
    if state.focus == PaneFocus::Preview
        && state.preview_png.is_none()
        && state.edit_state.is_none()
    {
        let step = state.preview_md.body_em().max(8.0);
        match action {
            Some(Action::TableLeft) => { state.md_table_scroll_px = (state.md_table_scroll_px - step).max(0.0); state.window.request_redraw(); return Break(()); }
            Some(Action::TableRight) => { state.md_table_scroll_px += step; state.window.request_redraw(); return Break(()); }
            Some(Action::TableReset) => { state.md_table_scroll_px = 0.0; state.window.request_redraw(); return Break(()); }
            _ => {}
        }
    }
    Continue(())
}
