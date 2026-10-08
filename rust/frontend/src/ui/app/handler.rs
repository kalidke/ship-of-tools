//! The event loop's callbacks: `impl ApplicationHandler for App` (resumed, window_event, about_to_wait, new_events).

use super::*;
use crate::ui::input::mouse::{cursor_moved, mouse_input, mouse_wheel};
use crate::ui::input::keypress::keyboard_input;

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let evt_rx = match self.evt_rx.take() {
            Some(rx) => rx,
            None => {
                tracing::error!("evt_rx already consumed");
                event_loop.exit();
                return;
            }
        };
        match State::new(event_loop, evt_rx, &self.cli, self.conns.clone(), self.leases.clone()) {
            Ok(mut state) => {
                // Spawn one transport task per host once the window exists,
                // since each task needs an Arc<Window> to call
                // request_redraw on incoming frames (ADR 0042 L2a). Every
                // host's sender clones the same `evt_tx` — fan-in, tagged at
                // each transport's own send, not through a forwarding task.
                if let (Some(rt), Some(evt_tx), Some(transports)) = (
                    self.rt.as_ref(),
                    self.evt_tx.take(),
                    self.pending_transports.take(),
                ) {
                    crate::net::hosts::spawn_transports(rt, transports, &evt_tx, &state.window, &state.leases, &mut state.hosts);
                    // ADR 0035: spawn the proxy manager whenever there's a
                    // runtime at all (i.e. at least one host connection is
                    // configured) — the manager just waits for listeners and
                    // costs nothing idle. It arms per port only when THAT
                    // port's owning host actually connects remotely and
                    // advertises the proxy, gated at ensure-time by
                    // `proxy_capable_hosts` (per host, set from each host's
                    // own Connected evt), NOT the CLI shape. Each listener now
                    // carries its own target daemon address + token
                    // (`ensure_proxy_for_url` resolves both from
                    // `host_resolved_dial`/`host_transports` for the row's
                    // OWNING host), so the manager itself no longer bakes in
                    // one daemon address — the ADR 0042 L2a "tied to the CLI
                    // `--tcp` flag alone, default_host only" scope note is
                    // superseded: per-host proxying for every other host is
                    // now in scope (this is the cross-host figure fix). The
                    // manager owns the async accept loop; the GPU thread hands
                    // it synchronously-bound listeners so a port is listening
                    // before the browser launches.
                    let (ltx, lrx) = tokio::sync::mpsc::unbounded_channel();
                    crate::pages::spawn_proxy_manager(rt, lrx);
                    state.proxy_listener_tx = Some(ltx);
                }
                // If `--start-mode modules` was set, queue a project.scan
                // request now so the chrome's initial render is the
                // unified Modules/Types tree. Mostly for `--capture`,
                // where we can't inject `m` mid-run.
                if state.mode == Mode::Modules {
                    let generation = state
                        .next_project_scan_gen(state.active_host.clone(), state.active_workspace_id.clone());
                    if let Err(e) = state.send(OutgoingReq::ProjectScan {
                        workspace_id: state.active_workspace_id.clone(),
                        generation,
                    }) {
                        tracing::warn!(error = %e, "drop initial project.scan request");
                    }
                }
                state.window.request_redraw();
                // Self-relaunch watcher (ADR 0017): poll for the sentinel
                // file that the build-and-relaunch helper drops. On first
                // sight, flag it and wake the window; `window_event` then
                // exits with code 75 so the supervisor respawns us. A
                // background thread (not the control-flow timer) keeps the
                // interactive `Wait` power profile intact.
                if let Some(sentinel) = relaunch_sentinel_path().filter(|_| !state.ephemeral) {
                    crate::relaunch::spawn_watcher(sentinel, state.relaunch_flag.clone(), state.window.clone());
                }
                // FE control-command watcher (ADR 0019): poll the fe-commands
                // dir for JSON command files dropped by an in-terminal agent
                // or the user. Parse + enqueue each, delete the file, and wake
                // the window so `window_event` drains the queue on the main
                // thread. Persistent (no break) — unlike the one-shot relaunch
                // watcher above.
                // Both watchers DELETE what they read, so a harness FE would
                // eat the primary FE's relaunch sentinel / control commands —
                // ephemeral instances don't arm them (B8).
                if let Some(cmd_dir) = fe_commands_dir().filter(|_| !state.ephemeral) {
                    let _ = std::fs::create_dir_all(&cmd_dir);
                    spawn_command_watcher(cmd_dir, state.fe_commands.clone(), state.window.clone());
                }
                self.state = Some(state);
            }
            Err(e) => {
                #[cfg(all(test, feature = "test-window-progress"))]
                eprintln!("window-progress not runnable here: native State startup failed: {e}");
                tracing::error!(error = %e, "failed to bring up wgpu surface");
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        relaunch_if_requested(state, event_loop);
        // FE control commands (ADR 0019): drain whatever the watcher enqueued
        // and dispatch on the main thread — same code paths as the keybinds.
        // Cheap no-op when the queue is empty.
        state.drain_fe_commands();
        match event {
            WindowEvent::CloseRequested => state.request_quit(event_loop, ExitReason::WindowClose),
            WindowEvent::Resized(size) => {
                state.resize(size);
                state.persist_resume_state();
                state.window.request_redraw();
            }
            WindowEvent::Moved(_) => {
                state.persist_resume_state();
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                state.resize(state.window.inner_size());
                state.window.request_redraw();
            }
            WindowEvent::ModifiersChanged(mods) => {
                // Cache the active modifier state. winit 0.30 doesn't ride
                // modifiers on KeyEvent, so the KeyboardInput arm consults
                // this for Ctrl+Arrow pane navigation.
                self.modifiers = mods.state();
            }
            WindowEvent::Focused(focused) => {
                // winit can drop a Ctrl/Shift/Alt release event when the
                // window loses focus mid-keystroke (alt-tab, lock
                // screen, etc.), which leaves `self.modifiers` stuck.
                // The next arrow key then triggers Ctrl+Arrow pane move
                // instead of tree nav, and the user reasonably reports
                // "nav broken". Clear the modifier cache on every
                // focus transition so we re-learn from the next
                // ModifiersChanged event.
                if !focused {
                    self.modifiers = winit::keyboard::ModifiersState::empty();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                cursor_moved(state, position);
            }
            WindowEvent::MouseInput {
                state: btn_state,
                button,
                ..
            } => {
                mouse_input(state, btn_state, button);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                mouse_wheel(state, self.modifiers, delta);
            }
            WindowEvent::RedrawRequested => {
                redraw_requested(state, event_loop);
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
                keyboard_input(state, event_loop, self.modifiers, event, is_synthetic);
            }
            _ => {}
        }
    }

    /// Called by winit after a batch of events is processed, before the loop
    /// goes to sleep. If a frame was deferred by the FRAME_BUDGET cap in
    /// `RedrawRequested`, schedule a wake-up at the next frame boundary so
    /// the deferred draw still lands — just on cadence instead of per-event.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if state.capture_path.is_some() {
            return;
        }
        // A leaving window exits only here, once its acks are in, with its
        // own exit code: the redraw exit skips it (`redraw_exits`).
        if state.should_exit {
            let now = std::time::Instant::now();
            match state.leaving.as_mut().map(|l| l.poll(now)) {
                Some(crate::lease::LeaveStep::Wait(t)) => {
                    event_loop.set_control_flow(ControlFlow::WaitUntil(t));
                    // A line owed to a frame whose redraw was throttled: go on
                    // to the frame-budget reschedule below, so that frame
                    // draws now and starts the hold (`Leaving::presented`).
                    if !(state.dirty && state.leaving.as_ref().is_some_and(|l| l.owes_frame())) {
                        return;
                    }
                }
                Some(crate::lease::LeaveStep::Show) => {
                    state.window.request_redraw();
                    event_loop.set_control_flow(ControlFlow::WaitUntil(now + crate::lease::NOT_ENDED_PRESENT_WAIT));
                    return;
                }
                Some(crate::lease::LeaveStep::Exit) | None => {
                    let code = state.leaving.as_ref().map_or(0, |l| l.exit_code);
                    state.finish_exit(event_loop, code);
                    return;
                }
            }
        }
        if state.help_peek_expired() {
            state.help.peek = None;
            state.window.request_redraw();
        }
        // Notify toast expiry: once the sticky window elapses, restore the
        // normal connection status so the toast doesn't linger until the next
        // event. The idle/flash tick below brings us back here within ~1s.
        if let Some(until) = state.notify_sticky_until {
            if std::time::Instant::now() >= until {
                state.notify_sticky_until = None;
                state.rebuild_connection_status();
                state.window.request_redraw();
            }
        }
        // The not-ended line's own expiry, same pattern as the toast.
        if let Some((_, until)) = state.not_ended_shown {
            if std::time::Instant::now() >= until {
                state.not_ended_shown = None;
                state.window.request_redraw();
            }
        }
        // Nav-spill expiry: same pattern as the toast — once the spill
        // window elapses, repaint so the nav column springs back to its
        // preset width. The ~1s idle tick bounds how late that lands.
        if let Some(until) = state.nav_spill_until {
            if std::time::Instant::now() >= until {
                state.nav_spill_until = None;
                state.window.request_redraw();
            }
        }
        if !state.dirty {
            // Fullscreen VRR/OLED brightness-flicker fix (2026-07-12, a VRR/OLED
            // ultrawide OLED). In borderless fullscreen DWM composition
            // disengages, so the panel's adaptive-sync refresh follows OUR
            // present cadence directly. The on-demand idle path below presents
            // ~1 frame/sec, which drives a VRR OLED down to a 1-10 Hz refresh —
            // exactly the band where low-framerate compensation doubles frames
            // unevenly and the panel's brightness pumps visibly. Keep a steady
            // vsync-paced cadence while fullscreen so the panel stays pinned at
            // its native refresh: request the next frame now and let
            // PresentMode::Fifo block in present() until vsync, which self-paces
            // the loop (no busy-spin) and never outruns the display. Costs
            // continuous GPU while fullscreen — acceptable for a static TUI and
            // scoped to fullscreen only; windowed keeps the efficient on-demand
            // tick below (DWM already composites it at a steady rate).
            //
            // Opt-in: `[display] fullscreen_vsync_pin` — default false,
            // because most panels are fixed-refresh and the pin only burns
            // power for no visible benefit. There is no VRR/adaptive-sync
            // detection API worth trusting, so a VRR/OLED panel that pumps
            // brightness in borderless fullscreen opts in explicitly —
            // measured ~28% of a core + ~10% iGPU on a 1440x900 laptop
            // panel. Off (the default), fullscreen falls through to the
            // same on-demand idle path windowed uses below.
            if state.settings.fullscreen_vsync_pin && state.window.fullscreen().is_some() {
                state.window.request_redraw();
                event_loop.set_control_flow(ControlFlow::Wait);
                return;
            }
            // Idle: nothing animating, but the top-right clock still needs to
            // tick. Schedule a single wake at the next ~1s boundary so the
            // chrome repaints and re-reads `Local::now()`. One wake per second,
            // no busy-loop. `new_events` turns the resume into a redraw.
            //
            // Exception: while a status-change flash is fading, the 1s clock
            // tick is far too coarse for the FLASH_SECS (0.6s) fade — it would
            // jump in one or two steps. Drop to a ~80ms cadence (≈8 frames
            // over the fade, smooth enough to read as a blink) only while a
            // flash is live; `redraw` prunes finished flashes so we fall back
            // to the 1s idle tick automatically once none remain.
            let fading_help = state.help.peek.as_ref().is_some_and(|p| p.started.elapsed() >= help::PEEK_HOLD);
            let interval = if fading_help {
                std::time::Duration::from_millis(16)
            } else if state.flash_starts.is_empty() {
                std::time::Duration::from_secs(1)
            } else {
                std::time::Duration::from_millis(80)
            };
            let mut deadline = std::time::Instant::now() + interval;
            if let Some(peek) = &state.help.peek {
                let fade_at = peek.started + help::PEEK_HOLD;
                if fade_at > std::time::Instant::now() { deadline = deadline.min(fade_at); }
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            return;
        }
        match state.last_frame_at {
            Some(t) => {
                let elapsed = t.elapsed();
                if elapsed >= FRAME_BUDGET {
                    state.dirty = false;
                    state.window.request_redraw();
                } else {
                    let deadline = std::time::Instant::now() + (FRAME_BUDGET - elapsed);
                    event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
                }
            }
            None => {
                state.dirty = false;
                state.window.request_redraw();
            }
        }
    }

    /// When the WaitUntil deadline set by `about_to_wait` fires, request the
    /// deferred draw and drop back to Wait so we don't busy-loop.
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if !matches!(cause, StartCause::ResumeTimeReached { .. }) {
            return;
        }
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if state.capture_path.is_some() {
            return;
        }
        // The deadline fired: either a deferred dirty frame is due, or it's the
        // idle clock tick (`!dirty`). Either way request a redraw so the chrome
        // repaints with a fresh `Local::now()`. `about_to_wait` will arm the
        // next 1s wake afterwards, so we don't busy-loop here.
        state.dirty = false;
        state.window.request_redraw();
        event_loop.set_control_flow(ControlFlow::Wait);
    }
}

fn relaunch_if_requested(state: &mut State, event_loop: &ActiveEventLoop) {
    // Self-relaunch (ADR 0017): the watcher thread set this when the
    // sentinel appeared — 75 for a plain relaunch, 76 for a converge
    // (relaunch-sot.ps1 -Converge; the supervisor re-runs its
    // self-update prelude and freshness pass before respawning). Persist
    // geometry, then exit with that code. Abrupt exit is fine: state is
    // saved on events, and the OS reclaims the window/GPU surface.
    let relaunch_code = state
        .relaunch_flag
        .swap(0, std::sync::atomic::Ordering::Relaxed);
    if relaunch_code != 0 {
        tracing::info!(
            exit_code = relaunch_code,
            "relaunch requested; exiting for supervisor respawn"
        );
        state.persist_resume_state();
        // The exit hands the OS foreground to the about-to-spawn
        // replacement (`finish_exit`, ADR 0017). The daemon keeps the
        // sessions for a minute (Handover) while the new window opens.
        if matches!(
            exit_intent(ExitReason::Relaunch(relaunch_code as i32), state.leaving.as_ref().map(|l| l.intent)),
            ExitStep::Leave { .. }
        ) {
            state.leave(event_loop, LeaveIntent::Handover, relaunch_code as i32);
        }
    }
}

fn redraw_requested(state: &mut State, event_loop: &ActiveEventLoop) {
    // Frame-rate cap: if the previous frame finished less than
    // FRAME_BUDGET ago, defer. `about_to_wait` reschedules at
    // the next frame boundary, so this draw isn't dropped —
    // just collapsed with whatever else arrives in the
    // intervening few ms. Capture mode bypasses the cap so
    // frame_counter ticks up to CAPTURE_FRAME without delay.
    let throttled = state.capture_path.is_none()
        && state
            .last_frame_at
            .map(|t| t.elapsed() < FRAME_BUDGET)
            .unwrap_or(false);
    if throttled {
        state.dirty = true;
    } else {
        state.dirty = false;
        if let Err(e) = state.redraw() {
            tracing::error!(error = %e, "redraw failed");
        }
        // ADR 0019: refresh fe-state.json if the observable state
        // changed this frame (cheap signature no-op otherwise).
        state.maybe_write_fe_state();
        // Portable focus-on-launch: now that the window is shown
        // and has painted once, attempt to take focus + raise.
        // Window managers that refuse focus-stealing (Windows
        // foreground-lock, macOS) get the OS-sanctioned fallback
        // of a user-attention request. One-shot. ADR 0017.
        if state.focus_on_first_frame {
            state.focus_on_first_frame = false;
            state.window.focus_window();
            // Windows blocks SetForegroundWindow for a freshly
            // spawned process (foreground lock), so a relaunched
            // FE lands behind. force_os_foreground escalates
            // (attach-thread → topmost-toggle → minimize/restore)
            // and reports whether we actually took the foreground.
            // Only fall back to a taskbar flash if it didn't.
            // ADR 0017.
            #[cfg(windows)]
            let got_foreground = force_os_foreground(&state.window);
            #[cfg(not(windows))]
            let got_foreground = false;
            if !got_foreground {
                state.window.request_user_attention(Some(
                    winit::window::UserAttentionType::Critical,
                ));
            }
        }
        if redraw_exits(state.should_exit, state.leaving.is_some(), state.capture_path.is_some()) {
            event_loop.exit();
        }
    }
}
