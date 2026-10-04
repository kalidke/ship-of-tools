//! The event loop's callbacks: `impl ApplicationHandler for App` (resumed, window_event, about_to_wait, new_events).

use super::*;

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
                    // ADR 0045 decision 1: captured BEFORE the loop below
                    // consumes `transports` — the session pane's capsule
                    // attach (`spawn_pane_attach_term`) reads this to build
                    // that row's own daemon dial.
                    state.host_transports = transports
                        .iter()
                        .map(|(host, config, _)| (host.clone(), config.clone()))
                        .collect();
                    for (host, config, req_rx) in transports {
                        let gate = state.link_gates.entry(host.clone()).or_default().clone();
                        crate::transport::spawn(
                            rt,
                            host,
                            config,
                            evt_tx.clone(),
                            req_rx,
                            state.window.clone(),
                            state.reconnect_now.clone(),
                            gate,
                            state.leases.clone(),
                        );
                    }
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
                    crate::proxy_listen::spawn_proxy_manager(rt, lrx);
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
                    let flag = state.relaunch_flag.clone();
                    let waker = state.window.clone();
                    if let Err(e) = std::thread::Builder::new()
                        .name("sot-relaunch-watch".to_string())
                        .spawn(move || loop {
                            std::thread::sleep(std::time::Duration::from_millis(400));
                            if sentinel.exists() {
                                // Read BEFORE removing: content picks 75 (plain
                                // relaunch) vs 76 (converge — relaunch-sot.ps1
                                // -Converge). Unreadable/empty content fails
                                // open to a plain relaunch.
                                // PowerShell 5.1's `-Encoding utf8` (the
                                // writer's ASCII path is preferred now, but a
                                // stale/foreign writer can still emit one)
                                // prepends a UTF-8 BOM (U+FEFF), which
                                // `trim_start()` does NOT strip (it's not
                                // Unicode whitespace) -- strip it explicitly
                                // first so a BOM-prefixed "converge" doesn't
                                // decode as a plain relaunch.
                                let is_converge = std::fs::read_to_string(&sentinel)
                                    .map(|s| {
                                        s.trim_start_matches('\u{feff}')
                                            .trim_start()
                                            .to_ascii_lowercase()
                                            .starts_with("converge")
                                    })
                                    .unwrap_or(false);
                                let _ = std::fs::remove_file(&sentinel);
                                flag.store(
                                    if is_converge { 76 } else { 75 },
                                    std::sync::atomic::Ordering::Relaxed,
                                );
                                waker.request_redraw();
                                break;
                            }
                        })
                    {
                        tracing::warn!(error = %e, "failed to spawn relaunch watcher");
                    }
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
                    let queue = state.fe_commands.clone();
                    let waker = state.window.clone();
                    if let Err(e) = std::thread::Builder::new()
                        .name("sot-fe-command-watch".to_string())
                        .spawn(move || loop {
                            std::thread::sleep(std::time::Duration::from_millis(400));
                            let entries = match std::fs::read_dir(&cmd_dir) {
                                Ok(e) => e,
                                Err(_) => continue,
                            };
                            // Sort by filename so a burst is processed roughly
                            // FIFO (writers can prefix a counter/timestamp).
                            let mut paths: Vec<std::path::PathBuf> = entries
                                .filter_map(|e| e.ok().map(|e| e.path()))
                                .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
                                .collect();
                            paths.sort();
                            let mut woke = false;
                            for path in paths {
                                let bytes = match std::fs::read(&path) {
                                    Ok(b) => b,
                                    Err(_) => continue,
                                };
                                // Delete first so a malformed file can't loop
                                // forever on the next tick.
                                let _ = std::fs::remove_file(&path);
                                match serde_json::from_slice::<FeCommand>(&bytes) {
                                    Ok(cmd) => {
                                        if let Ok(mut q) = queue.lock() {
                                            q.push_back(cmd);
                                            woke = true;
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            error = %e,
                                            path = %path.display(),
                                            "bad fe-command file dropped"
                                        );
                                    }
                                }
                            }
                            if woke {
                                waker.request_redraw();
                            }
                        })
                    {
                        tracing::warn!(error = %e, "failed to spawn fe-command watcher");
                    }
                }
                self.state = Some(state);
            }
            Err(e) => {
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
                state.cursor_px = (position.x as f32, position.y as f32);
                // Extend the LLM-pane selection while the user is dragging.
                // Drag uses non-strict cell mapping so the user can pull
                // outside the pane to extend selection to the edge.
                if state.llm_drag_active {
                    if let Some(end) = state.llm_cell_at_px(state.cursor_px, false) {
                        if let Some((start, _)) = state.llm_selection {
                            state.llm_selection = Some((start, end));
                            state.window.request_redraw();
                        }
                    }
                }
            }
            WindowEvent::MouseInput {
                state: btn_state,
                button,
                ..
            } => {
                // A real click, regardless of which button or what it does
                // below — presence reporting (design point A) precedes and
                // is independent of the click's own handling.
                state.report_presence();
                if button == MouseButton::Left {
                    match btn_state {
                        ElementState::Pressed => {
                            // Mouse-down inside the LLM pane starts a new
                            // selection at the clicked cell and grabs
                            // focus so subsequent keys (Ctrl+Shift+C copy)
                            // land in the LLM arm. Outside the pane: clear
                            // any existing selection so a click elsewhere
                            // dismisses the highlight.
                            if let Some(cell) = state.llm_cell_at_px(state.cursor_px, true) {
                                state.set_focus(PaneFocus::Llm);
                                state.llm_selection = Some((cell, cell));
                                state.llm_drag_active = true;
                                state.window.request_redraw();
                            } else if state.llm_selection.is_some() {
                                state.llm_selection = None;
                                state.window.request_redraw();
                            }
                        }
                        ElementState::Released => {
                            // Mouse-up just ends the drag — selection
                            // stays painted so the user has time to hit
                            // Ctrl+Shift+C.
                            state.llm_drag_active = false;
                        }
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Convert the platform delta into fractional rows, then
                // accumulate. Precision touchpads emit small sub-row
                // pixel deltas that would otherwise truncate to zero
                // and feel dead. Standard wheel ticks (LineDelta y=1)
                // step three rows, matching the common TUI cadence.
                let (delta_rows, raw_px_y, raw_px_x): (f32, f32, f32) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => {
                        (y * 3.0, y * state.cell_h * 3.0, x * state.cell_w * 3.0)
                    }
                    MouseScrollDelta::PixelDelta(pos) => {
                        (pos.y as f32 / state.cell_h, pos.y as f32, pos.x as f32)
                    }
                };
                // Shift+wheel-Y *or* a horizontal-axis wheel event in
                // Preview focus → wide-table horizontal scroll. Take
                // it before the vertical accumulator runs so a held
                // Shift doesn't also walk preview_scroll. Sign: wheel
                // up shifts the table left (reveal more right-side
                // content). Clamp happens in redraw.
                let shift = self.modifiers.shift_key();
                if state.focus == PaneFocus::Preview {
                    let h_px = if shift && raw_px_y.abs() > 0.0 {
                        -raw_px_y
                    } else if raw_px_x.abs() > 0.0 {
                        raw_px_x
                    } else {
                        0.0
                    };
                    if h_px.abs() > 0.0 {
                        state.md_table_scroll_px = (state.md_table_scroll_px + h_px).max(0.0);
                        state.window.request_redraw();
                        return;
                    }
                }
                state.wheel_residue_y += delta_rows;
                let rows_above = state.wheel_residue_y.trunc() as i32;
                state.wheel_residue_y -= rows_above as f32;
                tracing::info!(
                    delta_rows,
                    rows_above,
                    residue = state.wheel_residue_y,
                    ?state.focus,
                    "wheel"
                );
                if rows_above == 0 {
                    return;
                }
                if state.drawer == DrawerContent::Help && state.focus == PaneFocus::Repl {
                    state.help.move_selection(-(rows_above as isize), &state.bindings);
                    state.window.request_redraw();
                    return;
                }

                // Preview's scroll origin is the top of the doc, REPL
                // and LLM's are the tail — sign flip lives in each
                // pane's apply step so a single positive `rows_above`
                // feels like "show content above" everywhere.
                match state.focus {
                    PaneFocus::Repl if state.drawer == DrawerContent::Terminal => {
                        // The drawer is the local terminal. If the running app
                        // grabbed the mouse (vim/less/htop), forward the wheel
                        // as an SGR sequence so it scrolls its own view; else
                        // walk our vt100 scrollback ring (the emulator owns
                        // the offset). Sign: positive rows_above = up =
                        // older = larger offset, matching the REPL pane.
                        #[cfg(windows)]
                        let attach_mouse_on =
                            state.attach_term.as_ref().map(|t| t.mouse_tracking_on());
                        #[cfg(not(windows))]
                        let attach_mouse_on: Option<bool> = None;
                        let mouse_on = state
                            .local_term
                            .as_ref()
                            .map(|t| t.mouse_tracking_on())
                            .or(attach_mouse_on)
                            .unwrap_or(false);
                        if mouse_on {
                            let button = if rows_above > 0 { 64 } else { 65 };
                            let n = rows_above.unsigned_abs().min(8);
                            let seq = format!("\x1b[<{button};1;1M");
                            if let Some(t) = state.local_term.as_mut() {
                                for _ in 0..n {
                                    t.send_input(seq.as_bytes());
                                }
                            }
                            #[cfg(windows)]
                            if let Some(t) = state.attach_term.as_mut() {
                                for _ in 0..n {
                                    t.send_input(seq.as_bytes());
                                }
                            }
                        } else {
                            scroll_drawer_ring(state, rows_above);
                        }
                        state.window.request_redraw();
                    }
                    PaneFocus::Repl => {
                        let new = (state.repl_scroll as i32 + rows_above).max(0);
                        state.repl_scroll = new as u16;
                        state.window.request_redraw();
                    }
                    PaneFocus::Preview => {
                        let new = (state.preview_scroll as i32 - rows_above).max(0);
                        state.preview_scroll = new as u16;
                        state.window.request_redraw();
                    }
                    PaneFocus::Llm => {
                        // ADR 0042 slice L1b fix 2: drop scroll entirely
                        // while backend resolution is unknown — routing
                        // it to either backend would be a guess (see
                        // `PaneFeed`'s own doc), and the daemon fallback
                        // below would otherwise reach whatever tmux pty
                        // it still has open from the PREVIOUS row.
                        if state.pane_feed == PaneFeed::Pending {
                            return;
                        }
                        // ADR 0042 slice L1b: a capsule pane keeps REAL
                        // local scrollback (its own `vt100-ctt` parser,
                        // same as the drawer's attach client) — unlike
                        // tmux's in-place-repaint model below, there's no
                        // remote ring to forward to, so this mirrors the
                        // drawer's own Repl-focus wheel arm: forward as
                        // SGR only when the remote app grabbed the mouse,
                        // else walk the local ring via `scroll_ring`
                        // (the emulator owns it). No throttle — this is a
                        // local call on an already-open connection, not a
                        // wire round trip through the daemon.
                        if let Some(t) = state.pane_attach_term.as_mut() {
                            if t.mouse_tracking_on() {
                                let button = if rows_above > 0 { 64 } else { 65 };
                                let n = rows_above.unsigned_abs().min(8);
                                let seq = format!("\x1b[<{button};1;1M");
                                for _ in 0..n {
                                    t.send_input(seq.as_bytes());
                                }
                            } else {
                                scroll_ring(t.screen_mut(), rows_above);
                            }
                            state.window.request_redraw();
                            return;
                        }
                        // No live capsule client and not `Pending` (a
                        // logic bug — every row is a capsule on this
                        // build, there is no tmux fallback to forward
                        // wheel events to). Drop the residue and no-op.
                        state.wheel_residue_y = 0.0;
                    }
                    PaneFocus::NavTree => {
                        // Nav is cursor-driven; wheel-scroll without
                        // moving the cursor would desync the two. No-op
                        // until there's a richer story for it.
                    }
                }
            }
            WindowEvent::RedrawRequested => {
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
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
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
                let ctrl = self.modifiers.control_key();
                let alt = self.modifiers.alt_key();
                let shift = self.modifiers.shift_key();
                let super_ = self.modifiers.super_key();
                let base_key = event.key_without_modifiers();
                let context = state.help_context();
                let action = state.bindings.resolve(&event.logical_key, Some(&base_key),
                    Modifiers { ctrl, alt, shift, super_ }, context.consumes_text(), |a| context.allows(a));
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
                    return;
                }
                if !event.repeat && action == Some(Action::ToggleHelpDrawer) {
                    if state.drawer == DrawerContent::Help { state.close_help_drawer(); }
                    else { state.open_help_drawer(context); }
                    return;
                }
                if action == Some(Action::ToggleHelp) {
                    tracing::debug!(repeat = event.repeat, peek = state.help.peek.is_some(), ?context, "context help requested");
                    if event.repeat { return; }
                    if state.drawer == DrawerContent::Help && state.focus == PaneFocus::Repl {
                        state.close_help_drawer();
                    } else if let Some(peek) = state.help.peek.take() {
                        state.open_help_drawer(peek.context);
                    } else {
                        state.help.peek = Some(help::Peek { context, started: std::time::Instant::now() });
                        state.window.request_redraw();
                    }
                    return;
                }
                if state.help.peek.take().is_some() {
                    state.window.request_redraw();
                    if event.logical_key == Key::Named(NamedKey::Escape) { return; }
                }
                // Browsing Help consumes its own input; no typed search leaks into Julia.
                if state.drawer == DrawerContent::Help && state.focus == PaneFocus::Repl
                    && !action.is_some_and(|a| matches!(a.spec().scope,
                        crate::keybindings::Scope::Global | crate::keybindings::Scope::Workspace |
                        crate::keybindings::Scope::Restore))
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
                    return;
                }

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
                    state.reconnect_now.notify_waiters();
                    state.last_key = Some(label);
                    state.window.request_redraw();
                    return;
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
                    return;
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
                        return;
                    }
                    if action == Some(Action::FontScaleDown) {
                        state.apply_text_scale(state.text_scale_mult - 0.1);
                        state.persist_resume_state();
                        state.last_key = Some(label);
                        state.window.request_redraw();
                        return;
                    }
                    if action == Some(Action::FontScaleReset) {
                        state.apply_text_scale(1.0);
                        state.persist_resume_state();
                        state.last_key = Some(label);
                        state.window.request_redraw();
                        return;
                    }
                }
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
                            DrawerContent::Help => preset.drawer.or(Some(crate::settings::Slot::Repl)),
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
                        return;
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
                        return;
                    }
                    if action == Some(Action::WorkspaceCyclePrev) {
                        state.cycle_workspace(-1, true);
                        state.last_key = Some(label);
                        return;
                    }
                }
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
                    if action == Some(Action::MaximizePane) {
                        state.maximized = true;
                        state.last_key = Some(label);
                        state.window.request_redraw();
                        return;
                    }
                    if state.maximized
                        && action == Some(Action::RestoreLayout)
                    {
                        state.maximized = false;
                        state.last_key = Some(label);
                        state.window.request_redraw();
                        return;
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
                        return;
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
                        return;
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
                        return;
                    }
                    // Ctrl+J: toggle the REPL drawer (ADR 0014 layout
                    // rework). VS Code's panel-toggle convention; reads
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
                                crate::transport::OutgoingReq::MonitorSubscribe,
                            );
                            let _ = state.send_to(
                                &monitor_host,
                                crate::transport::OutgoingReq::MonitorHistory {
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
                                crate::transport::OutgoingReq::MonitorUnsubscribe,
                            );
                            state.monitor_view.subscribed = false;
                        }
                        state.last_key = Some(label);
                        state.window.request_redraw();
                        return;
                    }
                }
                // Alt+Up / Alt+Down: fine-grained one-row scroll in the
                // focused pane. Shared across REPL and Preview here so
                // the rule reads in one place. NavTree is cursor-driven
                // (manual scroll would desync) and LLM passes alt+arrow
                // through to the pty so tmux/shell keep alt-keybinds —
                // both fall through to the per-pane match below.
                if matches!(action, Some(Action::ScrollLineUp | Action::ScrollLineDown)) {
                    let row_step: i32 = 1;
                    match (state.focus, action) {
                        (PaneFocus::Repl, Some(Action::ScrollLineUp)) => {
                            state.repl_scroll = state.repl_scroll.saturating_add(row_step as u16);
                            state.window.request_redraw();
                            return;
                        }
                        (PaneFocus::Repl, Some(Action::ScrollLineDown)) => {
                            state.repl_scroll = state.repl_scroll.saturating_sub(row_step as u16);
                            state.window.request_redraw();
                            return;
                        }
                        (PaneFocus::Preview, Some(Action::ScrollLineUp)) => {
                            state.preview_scroll =
                                state.preview_scroll.saturating_sub(row_step as u16);
                            state.window.request_redraw();
                            return;
                        }
                        (PaneFocus::Preview, Some(Action::ScrollLineDown)) => {
                            state.preview_scroll =
                                state.preview_scroll.saturating_add(row_step as u16);
                            state.window.request_redraw();
                            return;
                        }
                        _ => {}
                    }
                }
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
                        Some(Action::TableLeft) => { state.md_table_scroll_px = (state.md_table_scroll_px - step).max(0.0); state.window.request_redraw(); return; }
                        Some(Action::TableRight) => { state.md_table_scroll_px += step; state.window.request_redraw(); return; }
                        Some(Action::TableReset) => { state.md_table_scroll_px = 0.0; state.window.request_redraw(); return; }
                        _ => {}
                    }
                }
                // Focus-dispatched handling. NavTree = tree nav + mode
                // switches; Repl = code typing + Enter to submit. Preview
                // and Llm are passive today — Escape returns focus to the
                // tree so the user is never stranded with no input target.
                match state.focus {
                    PaneFocus::NavTree => {
                        // Sessions-mode create-session input (B4): when
                        // the prompt is active, key events route into the
                        // label buffer instead of the usual nav shortcuts.
                        // Enter confirms, Esc cancels, Backspace pops one
                        // char, plain Char appends. Other keys ignored
                        // (no arrows / no q-to-exit) so the user isn't
                        // surprised by mode-switch shortcuts inside what
                        // visually looks like text input.
                        // Workspace picker is active (ADR 0014). The
                        // NavTree key handler routes navigation into the
                        // picker's directory tree instead of the regular
                        // Sessions list. Up/Down moves cursor; Right
                        // drills into the cursored sub-dir; Left/Backspace
                        // ascends to parent; Enter commits the cursored
                        // directory as the new workspace (with the ccb
                        // agent), Shift+Enter commits it as a bare session
                        // (no LLM agent); Esc cancels. q is intentionally
                        // *not* a quit shortcut here so the user can still
                        // type single chars later.
                        if state.workspace_picker.is_some() {
                            // Up/Down repeats so hold-to-scroll feels
                            // right. Enter on the cursored sub-directory
                            // is the "this is the one" gesture and commits
                            // it as the workspace root (Shift+Enter for a
                            // bare, agent-less session). Right is the
                            // no-commit preview path (drill in without
                            // selecting). Left / Backspace walks back to
                            // the parent. Esc cancels.
                            // Commit is keymap-driven (.sot/keybindings.toml:
                            // session.create / session.create_bare) for no-recompile
                            // reconfig. The resolver distinguishes Enter from Shift+Enter.
                            if !event.repeat
                                && action == Some(Action::SessionCreateCodex)
                            {
                                state.picker_confirm_selected("codex");
                                return;
                            }
                            if !event.repeat
                                && action == Some(Action::SessionCreateBare)
                            {
                                state.picker_confirm_selected("none");
                                return;
                            }
                            if !event.repeat
                                && action == Some(Action::SessionCreate)
                            {
                                state.picker_confirm_selected("claude");
                                return;
                            }
                            // Per-session accounts (owner-simplified brief,
                            // 2026-09-15): Tab cycles the account choice.
                            // No-op (via picker_cycle_account) when the
                            // choice is hidden (0 or 1 discovered accounts).
                            if !event.repeat
                                && action == Some(Action::SessionAccountNext)
                            {
                                state.picker_cycle_account();
                                return;
                            }
                            match action {
                                Some(Action::NavDown) => {
                                    state.picker_cursor_down();
                                    return;
                                }
                                Some(Action::NavUp) => {
                                    state.picker_cursor_up();
                                    return;
                                }
                                Some(Action::NavExpand) if !event.repeat => {
                                    state.picker_drill_in();
                                    return;
                                }
                                Some(Action::NavCollapse | Action::PickerParent)
                                    if !event.repeat =>
                                {
                                    state.picker_ascend();
                                    return;
                                }
                                Some(Action::Cancel) if !event.repeat => {
                                    state.picker_cancel();
                                    return;
                                }
                                _ => {
                                    return;
                                }
                            }
                        }
                        // NavTree text prompt active (Ctrl+N new-file-or-
                        // folder, and future delete-confirm). Like the
                        // picker, it steals every keystroke so the user can
                        // type a name without nav shortcuts firing: printable
                        // chars append (an embedded path separator is
                        // rejected at the source; a single trailing `/` is
                        // allowed as the "make it a directory" marker),
                        // Backspace pops, Enter confirms, Esc cancels, and
                        // any other nav key is swallowed so arrows / mode
                        // switches don't disturb the tree mid-type.
                        if state.nav_prompt.is_some() {
                            // ConfirmDelete is a y/N gate, not a text field:
                            // 'y'/'Y' confirms, everything else (incl.
                            // 'n'/'N'/Esc) cancels. CreateFile keeps its
                            // text-input behaviour below — branch on variant.
                            if matches!(state.nav_prompt, Some(NavPrompt::ConfirmDelete { .. })) {
                                match &event.logical_key {
                                    _ if action == Some(Action::DeleteConfirm) && !event.repeat =>
                                    {
                                        state.confirm_delete_file();
                                        return;
                                    }
                                    _ => {
                                        // 'n'/'N'/Esc/any other key → cancel.
                                        state.cancel_nav_prompt();
                                        return;
                                    }
                                }
                            }
                            match &event.logical_key {
                                _ if action == Some(Action::Confirm) && !event.repeat => {
                                    // Route Enter to whichever text prompt is open.
                                    if matches!(
                                        state.nav_prompt,
                                        Some(NavPrompt::ScaleEntry { .. })
                                    ) {
                                        state.confirm_scale_entry();
                                    } else {
                                        state.confirm_create_file();
                                    }
                                    return;
                                }
                                _ if action == Some(Action::Cancel) && !event.repeat => {
                                    state.cancel_nav_prompt();
                                    return;
                                }
                                Key::Named(NamedKey::Backspace) => {
                                    state.nav_prompt_backspace();
                                    return;
                                }
                                Key::Character(s) => {
                                    // A character key with a modifier other
                                    // than Shift (Ctrl/Alt/Super) isn't text
                                    // — swallow it rather than typing the
                                    // letter. Plain + Shift chars append.
                                    if !ctrl && !alt && !super_ {
                                        for c in s.chars() {
                                            state.nav_prompt_push_char(c);
                                        }
                                    }
                                    return;
                                }
                                _ => {
                                    return;
                                }
                            }
                        }
                        // NavTree focus: Ctrl+Q is the *only* way to exit
                        // the interactive window. Plain q and Esc no longer
                        // quit — Esc is used constantly in the LLM pane
                        // (vim, readline interrupt, claude prompt cancel)
                        // and the user routinely double-taps it; making
                        // it lethal turned every reflex into a quit risk.
                        // Ctrl+Q is scoped to NavTree only so it doesn't
                        // collide with terminal flow-control (XOFF) in
                        // the BL pty. Capture mode sets `should_exit` on
                        // its own and never sees user input.
                        if !event.repeat
                            && action == Some(Action::Quit)
                        {
                            state.request_quit(event_loop, ExitReason::QuitKey);
                            return;
                        }
                        // Ctrl+C: copy the cursored row's file path to the
                        // OS clipboard. Only fires for `files:`-prefixed
                        // node ids (Files mode + Modules-mode rows that
                        // reuse the synthesized files: id for previews);
                        // sessions / picker / workspace rows pass through.
                        // Ctrl+C is reserved-as-interrupt in the LLM pty
                        // but in NavTree there's no pty, so the universal
                        // copy convention reads cleanly here.
                        if !event.repeat
                            && action == Some(Action::CopyPath)
                            && state.copy_navtree_path()
                        {
                            state.last_key = Some(label);
                            state.window.request_redraw();
                            return;
                        }
                        // Ctrl+N: open the new-file-or-folder prompt. Files
                        // mode only, and only when the cursor sits on a
                        // `files:` row (begin_create_file no-ops otherwise
                        // and falls through to normal nav). A plain name
                        // reuses `file.write` with empty content; a name
                        // ending in `/` fires `dir.create` instead
                        // (confirm_create_file picks which).
                        if !event.repeat
                            && action == Some(Action::NewFile)
                            && matches!(state.mode, Mode::Files)
                            && state.begin_create_file()
                        {
                            state.last_key = Some(label);
                            state.window.request_redraw();
                            return;
                        }
                        // Ctrl+D: open the delete-confirm prompt. Files mode
                        // only, and only when the cursor sits on a deletable
                        // `files:` file row (begin_delete_file no-ops / pre-
                        // refuses dirs otherwise and falls through to normal
                        // nav). This is the NavTree-focus Ctrl+D; the preview-
                        // focus Ctrl+D (half-page scroll) is a separate block.
                        if !event.repeat
                            && action == Some(Action::DeleteFile)
                            && matches!(state.mode, Mode::Files)
                            && state.begin_delete_file()
                        {
                            state.last_key = Some(label);
                            state.window.request_redraw();
                            return;
                        }
                        match action {
                            Some(Action::NavDown) => {
                                state.tree.move_down();
                            }
                            Some(Action::NavUp) => {
                                state.tree.move_up();
                            }
                            Some(Action::NavExpand) | Some(Action::NavOpen)
                                if !event.repeat =>
                            {
                                // Enter in Sessions mode dispatches to the
                                // right action based on row kind:
                                //   session_create → open the label prompt (B4)
                                //   session / pane → attach BL to that session (B3)
                                //   anything else  → fall through to expand
                                // Right keeps the pure-expand behaviour so
                                // users can explore the panes list without
                                // re-targeting the BL pane.
                                let is_enter =
                                    action == Some(Action::NavOpen);
                                if is_enter && matches!(state.mode, Mode::Hosts) {
                                    // ADR 0015: persist the selected host
                                    // so the next launcher run targets
                                    // it. We don't tear down the live
                                    // transport — that would require a
                                    // sentinel-file protocol with the
                                    // launcher. ADR 0042 L2a: every host is
                                    // already a live connection, so Enter
                                    // just navigates the Sessions-mode
                                    // cursor to that host's node (ADR
                                    // 0015's relaunch flow is deleted).
                                    state.pick_host_under_cursor();
                                    return;
                                }
                                if is_enter && matches!(state.mode, Mode::Sessions) {
                                    let row = state.tree.rows.get(state.tree.selected);
                                    let kind = row.map(|r| r.node.kind.clone());
                                    match kind.as_deref() {
                                        Some("session_create") => {
                                            let host = row
                                                .and_then(|r| r.node.payload.get("host"))
                                                .and_then(|v| v.as_str())
                                                .map(str::to_string)
                                                .unwrap_or_else(|| state.active_host.clone());
                                            state.begin_create_session(host);
                                            return;
                                        }
                                        Some("session") | Some("pane") => {
                                            if let Some(session_name) =
                                                state.selected_session_name()
                                            {
                                                // ADR 0014: route the swap
                                                // through the unified entry
                                                // point. The slug is the
                                                // session name with the
                                                // `sot-be-` prefix
                                                // stripped (the backend's
                                                // resolve() accepts either
                                                // a workspace_id or a slug).
                                                let slug = session_name
                                                    .strip_prefix("sot-be-")
                                                    .map(|s| s.to_string());
                                                if slug.is_some() {
                                                    // ADR 0042 L2a: the
                                                    // cursored row's OWN
                                                    // host — both `session`
                                                    // and `pane` rows carry
                                                    // `payload.host`,
                                                    // stamped at the reply
                                                    // that built them.
                                                    let host = state
                                                        .selected_session_host()
                                                        .unwrap_or_else(|| {
                                                            state.active_host.clone()
                                                        });
                                                    // Sessions-Enter is
                                                    // person-driven: clear
                                                    // this row's blue.
                                                    state.switch_to_workspace(
                                                        host,
                                                        slug,
                                                        Some(session_name),
                                                        true,
                                                    );
                                                } else {
                                                    // Foreign tmux session
                                                    // surfaced by an older
                                                    // backend that hadn't
                                                    // filtered them out —
                                                    // just retarget BL, on
                                                    // the cursored row's
                                                    // OWN host (this row
                                                    // was never switched
                                                    // to, so active_host
                                                    // alone would be wrong
                                                    // — same reasoning as
                                                    // the branch above).
                                                    let host = state
                                                        .selected_session_host()
                                                        .unwrap_or_else(|| {
                                                            state.active_host.clone()
                                                        });
                                                    state.attach_session_to_bl(host, session_name);
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                state.try_expand_selected();
                            }
                            Some(Action::NavCollapse) if !event.repeat => {
                                if !state.collapse_selected_row() {
                                    if let Some(p) = state.tree.parent_of_selected() {
                                        state.tree.selected = p;
                                    }
                                }
                            }
                            // Mode switches are keymap-driven (mode.files /
                            // mode.modules / mode.sessions / mode.hosts) via
                            // match guards, so the default single-char chords
                            // (f/m/s/h) stay literal text everywhere else — this
                            // arm only runs inside the nav-focus match.
                            Some(Action::ModeFiles) if !event.repeat =>
                            {
                                state.enter_mode(Mode::Files);
                            }
                            Some(Action::ModeModules) if !event.repeat =>
                            {
                                state.enter_mode(Mode::Modules);
                            }
                            Some(Action::ModeSessions) if !event.repeat =>
                            {
                                state.enter_mode(Mode::Sessions);
                            }
                            // C2 pin-and-leave: `p` toggles pin on the
                            // cursor row. Only meaningful in Files mode
                            // — `toggle_pin` filters rows whose id
                            // doesn't start with `files:`.
                            Some(Action::TogglePin) if !event.repeat => {
                                state.toggle_pin();
                            }
                            // ADR 0015 — `h` enters Mode::Hosts, populating
                            // the nav tree from `conns`. No backend
                            // round-trip needed: the `--dial` set is
                            // resolved at startup and lives entirely on
                            // the frontend side. Cursor on the
                            // currently-selected host is the natural way in.
                            Some(Action::ModeHosts) if !event.repeat =>
                            {
                                state.enter_mode(Mode::Hosts);
                            }
                            // `.` toggles hidden dotfiles in Files mode
                            // (nav-focus-gated via the keymap so it stays
                            // literal text in the pty/editor/prompts). Sends
                            // nav.toggle_hidden + re-fetches the files tree.
                            Some(Action::ToggleHidden) if !event.repeat =>
                            {
                                if state.workspace_picker.is_some() {
                                    state.picker_toggle_hidden();
                                } else {
                                    state.toggle_hidden_files();
                                }
                            }
                            // Capital D (Shift+d) in Sessions mode →
                            // destroy the cursor row's workspace. Two-
                            // press confirm via `was_destroy_pending`:
                            // first press arms with the target id, the
                            // status line tells the user; second press
                            // on the same row fires `workspace.destroy`.
                            // Cursor move, mode switch, or any other
                            // key clears the arm (handled by the
                            // snapshot-and-clear at the top of this
                            // handler). A default TMUX row is rejected
                            // backend-side (surfaces as a status error);
                            // a default CAPSULE row instead ends its
                            // run and keeps the row (backend-side too —
                            // see `WorkspaceDestroyed`'s `kept` branch).
                            Some(Action::SessionDestroy) if !event.repeat =>
                            {
                                let Some(row) = state.tree.rows.get(state.tree.selected) else {
                                    return;
                                };
                                if row.node.kind != "session" {
                                    return;
                                }
                                let target_id = row
                                    .node
                                    .payload
                                    .get("workspace_id")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string);
                                // ADR 0042 L2a: destroy targets the ROW's
                                // own host, not `active_host` — routed via
                                // `send_to`.
                                let target_host = row
                                    .node
                                    .payload
                                    .get("host")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string)
                                    .unwrap_or_else(|| state.active_host.clone());
                                let target_label = row
                                    .node
                                    .payload
                                    .get("label")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or(row.node.label.as_str())
                                    .to_string();
                                let Some(target_id) = target_id else {
                                    state.status =
                                        "destroy: row has no workspace_id (refresh `s` and retry)"
                                            .to_string();
                                    state.window.request_redraw();
                                    return;
                                };
                                let target: WsKey = (target_host.clone(), target_id.clone());
                                if was_destroy_pending.as_ref() == Some(&target) {
                                    if let Err(e) = state.send_to(
                                        &target_host,
                                        crate::transport::OutgoingReq::WorkspaceDestroy {
                                            workspace_id: target_id.clone(),
                                        },
                                    ) {
                                        tracing::warn!(error = %e, "drop workspace.destroy");
                                        state.status = format!(
                                            "destroy '{target_label}' failed · channel closed"
                                        );
                                    } else {
                                        state.status = format!("destroying '{target_label}'…");
                                    }
                                    state.window.request_redraw();
                                } else {
                                    state.pending_destroy_target = Some(target);
                                    state.status =
                                        format!("press {} again to destroy '{target_label}' · any other key cancels", state.bindings.first_label(Action::SessionDestroy));
                                    state.window.request_redraw();
                                }
                            }
                            // `o` opens the cursored row in an external
                            // tool: text/html previews → temp file + OS
                            // browser; .jl files → backend `pluto.open`
                            // (header-checked on the backend, returns
                            // `not_pluto_flavored` for raw .jl). Routed
                            // by the cursored row's path, not preview
                            // mime — the JuliaSource plugin renders .jl
                            // as tokens-JSON.
                            Some(Action::OpenExternal) if !event.repeat => {
                                let cursored = state.cursored_files_path();
                                state.open_path_external(cursored);
                            }
                            // `W` (Shift+W): open the project's built Documenter
                            // site in the OS browser with full CSS/JS/sub-page
                            // fidelity (ADR 0024). Backend serves `docs/build`
                            // over a forwarded loopback port. Sends the cursored
                            // path so a built docs page deep-links; otherwise the
                            // backend opens the index. `W` works from any mode.
                            Some(Action::OpenDocs) if !event.repeat =>
                            {
                                let path = state.cursored_files_path().unwrap_or_default();
                                state.docs_open_external(path);
                            }
                            // `O` (Shift+O): full render WITH code execution
                            // for a cursored `.qmd`, then open in the browser.
                            // Slower + needs the language kernels on the backend
                            // host; `o` is the fast no-execute path.
                            Some(Action::OpenExecute) if !event.repeat =>
                            {
                                let cursored = state.cursored_files_path();
                                state.quarto_open_execute(cursored);
                            }
                            // `d`: download the cursored file row to the local
                            // OS downloads dir (OS-independent), non-clobbering.
                            // Transport streams chunks; dir rows are a no-op.
                            Some(Action::Download) if !event.repeat =>
                            {
                                state.start_download();
                            }
                            // `u`: pick a local file via the native OS dialog
                            // and upload it to the cursored nav folder (the dir
                            // itself for a dir row, else the file's parent).
                            Some(Action::Upload) if !event.repeat =>
                            {
                                state.start_upload();
                            }
                            // Priority J: `r` resets the workspace's
                            // persistent REPL into the file's closest-
                            // ancestor Project.toml then include()s the
                            // file. `R` (Shift+r) just include()s in the
                            // existing REPL — no env change. Both gate
                            // on a `.jl` cursored row; non-.jl rows are
                            // a no-op. Output flows back through the
                            // existing repl frame stream into the REPL
                            // drawer. Future: mirror the last image
                            // frame to the preview pane (TODO row 161).
                            Some(Action::RunFresh | Action::RunCurrent) if !event.repeat =>
                            {
                                let Some(abs) = state.cursored_files_path() else {
                                    return;
                                };
                                if !abs.ends_with(".jl") {
                                    tracing::debug!(path = %abs,
                                        "`r`/`R` ignored — not a .jl file");
                                    return;
                                }
                                let fresh = action == Some(Action::RunFresh);
                                let basename = abs
                                    .rsplit(['/', '\\'])
                                    .next()
                                    .unwrap_or(abs.as_str())
                                    .to_string();
                                // `r` resets the REPL *process* on the backend
                                // (fresh `julia --project=…`), so reset the
                                // drawer window to match — the old scrollback
                                // belongs to a now-dead session. `R` keeps the
                                // existing session and its scrollback. History
                                // derives from `repl_log`, so clearing the log
                                // clears it too; the eval counter keeps
                                // monotonically rising to avoid eval_id reuse
                                // with any still-draining replies.
                                if fresh {
                                    state.repl_log.clear();
                                    state.repl_scroll = 0;
                                    state.repl_pkg_mode = false;
                                    state.history_pos = None;
                                    state.history_saved = None;
                                }
                                // Pre-register a `repl_log` entry exactly the way
                                // `submit_repl_input` does for repl.eval, so the
                                // ReplRunFileDone reply can splice frames in by
                                // eval_id and the drawer scrollback shows the
                                // run's output alongside everything else.
                                state.repl_eval_counter = state.repl_eval_counter.saturating_add(1);
                                let eval_id = state.repl_eval_counter;
                                let owner_host = state.active_host.clone();
                                let workspace_key = state.active_ws_key();
                                state
                                    .eval_id_workspace
                                    .insert((owner_host, eval_id), workspace_key.clone());
                                if state.repl_log.len() >= 256 {
                                    let excess = state.repl_log.len() - 255;
                                    state.repl_log.drain(0..excess);
                                }
                                let synthetic_code = format!("{} {}", if fresh { "r" } else { "R" }, abs);
                                state.repl_log.push(ReplEntry {
                                    eval_id,
                                    code: synthetic_code,
                                    frames: Vec::new(),
                                    elapsed_ms: 0,
                                    in_flight: true,
                                    pkg_mode: false,
                                    origin: None,
                                });
                                if let Err(e) =
                                    state.send(crate::transport::OutgoingReq::ReplRunFile {
                                        eval_id,
                                        path: abs.clone(),
                                        fresh,
                                        workspace_id: state.active_workspace_id.clone(),
                                    })
                                {
                                    tracing::warn!(error = %e,
                                        "failed to dispatch repl.run_file");
                                    if let Some(entry) =
                                        state.repl_log.iter_mut().find(|e| e.eval_id == eval_id)
                                    {
                                        entry.in_flight = false;
                                        entry.frames.push(sot_protocol::ReplFrame::Error {
                                            message: format!("transport channel closed: {e}"),
                                            stacktrace: Vec::new(),
                                        });
                                    }
                                    state.status = format!(
                                        "repl.run_file '{basename}' failed · channel closed"
                                    );
                                } else if fresh {
                                    state.status =
                                        format!("running '{basename}' (resetting REPL …)");
                                } else {
                                    state.status = format!("running '{basename}' (existing repl)");
                                }
                                // Auto-open (or switch to) the REPL drawer so
                                // the run's output is visible (settings-gated,
                                // default on). If the Terminal drawer is up we
                                // swap it for the REPL since that's where the
                                // output lands. Keep NavTree focus so `r`/`R`
                                // stay usable — unlike Ctrl+J this does not
                                // steal focus.
                                if state.settings.repl_auto_open_drawer_on_run
                                    && state.drawer != DrawerContent::Repl
                                    && state
                                        .settings
                                        .resolve_preset(state.monitor_aspect)
                                        .drawer
                                        .is_some()
                                {
                                    state.drawer = DrawerContent::Repl;
                                }
                                state.window.request_redraw();
                            }
                            _ => {}
                        }
                    }
                    PaneFocus::Repl => {
                        // Ctrl+L — clear the REPL drawer scrollback (the
                        // universal REPL-clear; maintainer note, 2026-07-03, "how do I
                        // clear the repl"). Clears the log, its decoded
                        // inline figures, and the scroll offset; the julia
                        // process and its state are untouched (`r` on a .jl
                        // is the process-restart gesture). Terminal drawer
                        // unaffected — its pty owns Ctrl+L natively.
                        if !event.repeat
                            && action == Some(Action::ReplClear)
                        {
                            state.repl_log.clear();
                            state.repl_images.clear();
                            state.repl_image_slots.clear();
                            state.repl_scroll = 0;
                            state.status = "repl · scrollback cleared".to_string();
                            state.window.request_redraw();
                            return;
                        }
                        // Paste shortcut (Ctrl+V / Cmd+V / Shift+Insert):
                        // read the OS clipboard. The Terminal drawer gets it
                        // as a bracketed-paste blob on its pty (like the LLM
                        // pane); the Julia REPL drawer gets it appended to
                        // its input buffer. Intercepted before the
                        // terminal/REPL split below, where Ctrl+V would
                        // otherwise send a bare 0x16 to the pty or type a
                        // literal "v" into the buffer.
                        let is_paste_shortcut = !event.repeat && action == Some(Action::Paste);
                        if is_paste_shortcut {
                            if state.drawer == DrawerContent::Terminal {
                                forward_clipboard_paste_to_local_term(state);
                            } else if let Some(text) = read_clipboard_text() {
                                // REPL input is an editable buffer, not a pty
                                // — no bracketed-paste envelope. Normalize to
                                // `\n`; embedded newlines stay in the buffer
                                // (Shift+Enter inserts them too) and the user
                                // submits with Enter.
                                let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
                                state.repl_input.push_str(&normalized);
                                state.repl_scroll = 0;
                            }
                            state.last_key = Some(label);
                            state.window.request_redraw();
                            return;
                        }
                        // G4: when the drawer is showing the local terminal,
                        // every keystroke is forwarded to its PTY and the
                        // REPL input/history/scrollback handling below is
                        // bypassed entirely. Drawer toggles (Ctrl+T / Ctrl+J)
                        // and Ctrl+Arrow are intercepted globally before this
                        // arm, so they still exit/switch the pane; Tab falls
                        // through to the PTY for shell completion.
                        if state.drawer == DrawerContent::Terminal {
                            // Plain PageUp/PageDown page our scrollback ring —
                            // same convention as the LLM pane, so claude in
                            // the drawer scrolls like claude in the LLM pane.
                            // Alternate-screen apps (vim/less) page themselves,
                            // so they get the raw key; Shift+PgUp/PgDn is the
                            // escape hatch that hands a primary-screen app the
                            // plain key. One-third-pane step like the REPL pane.
                            let h = state.pane_rects.repl.height as i32;
                            let page_step = (h / 3).max(1);
                            #[cfg(windows)]
                            let attach_alt_screen = state
                                .attach_term
                                .as_ref()
                                .map(|t| t.screen().alternate_screen());
                            #[cfg(not(windows))]
                            let attach_alt_screen: Option<bool> = None;
                            let alt_screen = state
                                .local_term
                                .as_ref()
                                .map(|t| t.screen().alternate_screen())
                                .or(attach_alt_screen)
                                .unwrap_or(false);
                            if !alt_screen {
                                match &event.logical_key {
                                    _ if action == Some(Action::ScrollPageUp) => {
                                        scroll_drawer_ring(state, page_step);
                                        state.window.request_redraw();
                                        return;
                                    }
                                    _ if action == Some(Action::ScrollPageDown) => {
                                        scroll_drawer_ring(state, -page_step);
                                        state.window.request_redraw();
                                        return;
                                    }
                                    _ => {}
                                }
                            }
                            if let Some(bytes) = key_to_pty_bytes(&event.logical_key, ctrl, shift, super_) {
                                // Typing snaps back to the live tail so the
                                // cursor/prompt is visible (standard emulator
                                // behaviour).
                                if let Some(t) = state.local_term.as_mut() {
                                    t.send_input(&bytes);
                                    t.screen_mut().set_scrollback(0);
                                }
                                #[cfg(windows)]
                                if let Some(t) = state.attach_term.as_mut() {
                                    t.send_input(&bytes);
                                    t.screen_mut().set_scrollback(0);
                                }
                                state.last_key = Some(label);
                                state.window.request_redraw();
                            }
                            return;
                        }
                        // Scrollback navigation intercepts before any
                        // input-buffer arms, so PgUp/PgDn / Ctrl+u/d
                        // don't try to also type into repl_input. Held
                        // keys repeat (no `!event.repeat` guard) so
                        // hold-to-scroll feels natural. Sign flip vs
                        // preview: REPL's scroll origin is the tail,
                        // so PgUp grows the offset (older). PgUp/PgDn
                        // step is one-third of the pane (not a full
                        // page) so two rows of context survive the
                        // scroll; full-page jumps were dropping the
                        // user out of where they were reading.
                        let h = state.pane_rects.repl.height as i32;
                        let page_step = (h / 3).max(1);
                        match &event.logical_key {
                            _ if action == Some(Action::ScrollPageUp) => {
                                let new = (state.repl_scroll as i32 + page_step).max(0);
                                state.repl_scroll = new as u16;
                                state.window.request_redraw();
                                return;
                            }
                            _ if action == Some(Action::ScrollPageDown) => {
                                let new = (state.repl_scroll as i32 - page_step).max(0);
                                state.repl_scroll = new as u16;
                                state.window.request_redraw();
                                return;
                            }
                            _ if action == Some(Action::PreviewHalfUp) => {
                                let new = (state.repl_scroll as i32 + h / 2).max(0);
                                state.repl_scroll = new as u16;
                                state.window.request_redraw();
                                return;
                            }
                            _ if action == Some(Action::PreviewHalfDown) => {
                                let new = (state.repl_scroll as i32 - h / 2).max(0);
                                state.repl_scroll = new as u16;
                                state.window.request_redraw();
                                return;
                            }
                            // Ctrl+C interrupts a running eval (repl.interrupt).
                            // Only dispatched when something is actually in
                            // flight: the backend schedules an InterruptException
                            // into the eval task and the error+done frames stream
                            // back to finalize the entry (no eval_id -- the kernel
                            // interrupts its CURRENT_EVAL). With nothing running,
                            // Ctrl+C clears the input line (standard REPL UX)
                            // instead of typing a literal 'c'.
                            _ if action == Some(Action::ReplInterrupt) => {
                                if state.repl_log.iter().any(|e| e.in_flight) {
                                    if let Err(e) =
                                        state.send(crate::transport::OutgoingReq::ReplInterrupt {
                                            workspace_id: state.active_workspace_id.clone(),
                                        })
                                    {
                                        tracing::warn!(error = %e, "drop repl.interrupt - channel closed");
                                    } else {
                                        tracing::info!("repl.interrupt dispatched (Ctrl+C)");
                                        state.status = "interrupting...".to_string();
                                    }
                                } else {
                                    state.repl_input.clear();
                                    state.repl_pkg_mode = false;
                                }
                                state.repl_scroll = 0;
                                state.window.request_redraw();
                                return;
                            }
                            // Up/Down walk REPL history. Allowed to repeat
                            // so hold-to-walk feels natural. Returns
                            // early so the input-buffer match below
                            // doesn't also see the keypress.
                            _ if action == Some(Action::ReplHistoryPrev) => {
                                if let Some(prev) = state.history_step_back() {
                                    state.repl_input = prev;
                                    state.repl_scroll = 0;
                                    state.window.request_redraw();
                                }
                                return;
                            }
                            _ if action == Some(Action::ReplHistoryNext) => {
                                if let Some(next) = state.history_step_forward() {
                                    state.repl_input = next;
                                    state.repl_scroll = 0;
                                    state.window.request_redraw();
                                }
                                return;
                            }
                            _ => {}
                        }
                        match &event.logical_key {
                            // Escape returns focus to the tree pane; it
                            // does NOT exit the app from inside the REPL
                            // — exit only happens from tree focus, which
                            // is the safer default for an input pane.
                            _ if action == Some(Action::ReturnNav) && !event.repeat => {
                                state.set_focus(PaneFocus::NavTree);
                            }
                            // Shift+Enter inserts a literal newline into
                            // the input buffer instead of submitting —
                            // mirrors the convention used by Slack /
                            // Discord / VS Code REPLs and a handful of
                            // shells. Repeat is allowed so hold-down
                            // appends multiple blank lines.
                            _ if action == Some(Action::ReplNewline) => {
                                state.repl_input.push('\n');
                                state.repl_scroll = 0;
                            }
                            _ if action == Some(Action::ReplSubmit) && !event.repeat => {
                                state.submit_repl_input();
                                // Snap back to live when the user
                                // commits a line — the new entry is at
                                // the tail and the user expects to see
                                // its output.
                                state.repl_scroll = 0;
                            }
                            Key::Named(NamedKey::Backspace) => {
                                // Backspace at start of empty input in
                                // pkg mode leaves pkg mode — mirrors
                                // the standard Julia REPL UX.
                                if state.repl_input.is_empty() && state.repl_pkg_mode {
                                    state.repl_pkg_mode = false;
                                } else {
                                    state.repl_input.pop();
                                }
                                state.repl_scroll = 0;
                            }
                            Key::Named(NamedKey::Space) => {
                                state.repl_input.push(' ');
                                state.repl_scroll = 0;
                            }
                            Key::Character(s) => {
                                // A Command chord that didn't resolve to an
                                // action above must not leak into the Julia
                                // input either (macOS: winit delivers
                                // Cmd+<letter> as a plain Character with
                                // `super_` set — the same invariant
                                // `key_to_pty_bytes` enforces for the ptys).
                                if cfg!(target_os = "macos") && super_ {
                                    // consumed, not typed
                                } else if s.as_str() == "]"
                                    && state.repl_input.is_empty()
                                    && !state.repl_pkg_mode
                                {
                                    // `]` at start of empty input enters
                                    // pkg mode and consumes the keypress —
                                    // again mirroring the standard REPL.
                                    state.repl_pkg_mode = true;
                                    state.repl_scroll = 0;
                                } else {
                                    // Append the typed string verbatim.
                                    // winit honours shift/IME so
                                    // casing/accents already arrive
                                    // correctly. Filter only control
                                    // chars so stray sequences don't
                                    // leak into the buffer.
                                    for c in s.chars() {
                                        if !c.is_control() {
                                            state.repl_input.push(c);
                                        }
                                    }
                                    state.repl_scroll = 0;
                                }
                            }
                            _ => {}
                        }
                    }
                    PaneFocus::Preview => {
                        // Edit mode hijacks all keys — typing into the
                        // editable annotation body takes precedence
                        // over scroll keys. Ctrl+S saves; Esc discards
                        // (commit 3 adds the dirty-confirm modal).
                        if let Some(edit) = state.edit_state.as_mut() {
                            // 1/3-pane step here too so the editor's
                            // cursor doesn't blow past visible context
                            // on PgUp/PgDn.
                            let page_rows = (state.pane_rects.preview.height as usize / 3).max(1);
                            // When the discard-confirm modal is up,
                            // the key handler is just y/n/Esc. Any
                            // other key dismisses the modal and
                            // returns to editing without consuming
                            // the character (felt safer than letting
                            // a stray keystroke leak into the buffer
                            // during a confirmation).
                            if edit.confirm_discard {
                                match &event.logical_key {
                                    // `y` or a second Esc → discard edits and
                                    // leave the editor (Esc-to-confirm-exit,
                                    // chosen 2026-06-09: first Esc raises this
                                    // prompt, a second Esc exits). `n` or any
                                    // other key cancels back to editing.
                                    _ if action == Some(Action::DiscardConfirm) && !event.repeat =>
                                    {
                                        state.edit_state = None;
                                        state.preview_edit = None;
                                    }
                                    _ => {
                                        edit.confirm_discard = false;
                                    }
                                }
                                state.window.request_redraw();
                                return;
                            }
                            // Stale banner intercepts before edit keys
                            // too — r reloads from disk (discards
                            // edits), k keeps the banner dismissed so
                            // the user can keep editing. The next save
                            // will fail again until the underlying
                            // file changes or the user reloads.
                            if edit.stale_banner {
                                match &event.logical_key {
                                    _ if action == Some(Action::StaleReload) && !event.repeat =>
                                    {
                                        // Reload from disk, discarding edits. For
                                        // a file edit re-fire file.read (the
                                        // FileRead handler replaces the buffer +
                                        // clears the banner via a fresh
                                        // edit_state); for a concept edit re-fire
                                        // concept.read.
                                        let ws = state.active_workspace_id.clone();
                                        if let Some(node_id) = edit.file_node_id.clone() {
                                            state.pending_file_edit = Some(node_id.clone());
                                            if let Err(e) = state.send(OutgoingReq::FileRead {
                                                node_id,
                                                workspace_id: ws,
                                            }) {
                                                tracing::warn!(error = %e,
                                                    "drop file.read for stale reload");
                                            }
                                        } else {
                                            let target = edit.target.clone();
                                            let generation = state.next_concept_gen();
                                            if let Err(e) = state.send(OutgoingReq::ConceptRead {
                                                target,
                                                workspace_id: ws,
                                                generation,
                                            }) {
                                                tracing::warn!(error = %e,
                                                    "drop concept.read for stale reload");
                                            }
                                        }
                                    }
                                    _ if action == Some(Action::StaleKeep) && !event.repeat =>
                                    {
                                        edit.stale_banner = false;
                                    }
                                    _ => {
                                        // Any other key: ignored.
                                        // Banner stays up until the
                                        // user picks r or k.
                                    }
                                }
                                state.rebuild_edit_preview();
                                state.window.request_redraw();
                                return;
                            }
                            // Track whether the buffer changed so we
                            // only rebuild `preview_edit` when needed.
                            // Cheap either way (few-KB shape), but it
                            // keeps the trace log clean of redundant
                            // rebuilds during cursor-only navigation.
                            let mut buf_changed = false;
                            match &event.logical_key {
                                _ if action == Some(Action::Cancel) && !event.repeat => {
                                    // Dirty buffer → confirm modal.
                                    // Clean buffer → discard right
                                    // away (no value in asking when
                                    // there are no edits to lose).
                                    if edit.is_dirty() {
                                        edit.confirm_discard = true;
                                    } else {
                                        state.edit_state = None;
                                        state.preview_edit = None;
                                        // Re-fetch the underlying preview so
                                        // it reflects content saved during
                                        // this edit session — the cached
                                        // preview is the pre-edit render.
                                        // Clearing the fired-guard lets
                                        // maybe_fire_preview re-issue
                                        // preview.get for the still-selected
                                        // node.
                                        state.preview_node_id_fired = None;
                                        state.maybe_fire_preview();
                                    }
                                    state.window.request_redraw();
                                    return;
                                }
                                _ if action == Some(Action::EditSave) => {
                                    let content = edit.full_content();
                                    let ws = state.active_workspace_id.clone();
                                    if let Some(node_id) = edit.file_node_id.clone() {
                                        // General-file save → file.write, gated
                                        // on the version we read (conflict-aware).
                                        let expected_version = edit.file_version.clone();
                                        if let Err(e) = state.send(OutgoingReq::FileWrite {
                                            node_id,
                                            content,
                                            expected_version,
                                            workspace_id: ws,
                                        }) {
                                            tracing::warn!(error = %e,
                                                "drop file.write — channel closed");
                                        }
                                    } else {
                                        // Concept-annotation save (existing path).
                                        let target = edit.target.clone();
                                        let expected = edit.expected_ast_hash.clone();
                                        if let Err(e) = state.send(OutgoingReq::ConceptWrite {
                                            target,
                                            content,
                                            expected_ast_hash: expected,
                                            workspace_id: ws,
                                        }) {
                                            tracing::warn!(error = %e,
                                                "drop concept.write — channel closed");
                                        }
                                    }
                                }
                                _ if action == Some(Action::EditUndo) => {
                                    if edit.buf.undo() {
                                        buf_changed = true;
                                    }
                                }
                                _ if action == Some(Action::EditRedo) => {
                                    if edit.buf.redo() {
                                        buf_changed = true;
                                    }
                                }
                                // Ctrl+C: copy the active selection to the OS
                                // clipboard. Consumed even with no selection so
                                // it never types a literal "c" into the buffer.
                                _ if action == Some(Action::EditCopy) =>
                                {
                                    if let Some(sel) = edit.buf.selected_text() {
                                        let text = sel.to_string();
                                        match arboard::Clipboard::new()
                                            .and_then(|mut cb| cb.set_text(text.clone()))
                                        {
                                            Ok(()) => tracing::info!(
                                                bytes = text.len(),
                                                "editor.copy → clipboard"
                                            ),
                                            Err(e) => tracing::warn!(
                                                error = %e,
                                                "clipboard write failed; editor copy dropped"
                                            ),
                                        }
                                    }
                                }
                                // Ctrl+X: cut — copy the selection, then delete
                                // it as one undo step. No selection → consumed
                                // no-op (never types an "x").
                                _ if action == Some(Action::EditCut) =>
                                {
                                    if let Some(sel) = edit.buf.selected_text() {
                                        let text = sel.to_string();
                                        if let Err(e) = arboard::Clipboard::new()
                                            .and_then(|mut cb| cb.set_text(text))
                                        {
                                            tracing::warn!(
                                                error = %e,
                                                "clipboard write failed; editor cut still deletes"
                                            );
                                        }
                                        edit.buf.delete_selection();
                                        buf_changed = true;
                                    }
                                }
                                // Paste (Ctrl+V / Cmd+V): insert the OS
                                // clipboard as one atomic undo step. Without
                                // this arm Ctrl+V falls through to the generic
                                // Character arm below and types a literal "v".
                                // Normalize line endings to the buffer's `\n`
                                // convention (Enter inserts `\n`).
                                _ if action == Some(Action::EditPaste) =>
                                {
                                    if let Some(text) = read_clipboard_text() {
                                        edit.buf.insert_str(
                                            &text.replace("\r\n", "\n").replace('\r', "\n"),
                                        );
                                        buf_changed = true;
                                    }
                                }
                                _ if action == Some(Action::EditNewline) => {
                                    edit.buf.insert_char('\n');
                                    buf_changed = true;
                                }
                                _ if action == Some(Action::EditBackspace) => {
                                    edit.buf.backspace();
                                    buf_changed = true;
                                }
                                _ if action == Some(Action::EditDelete) => {
                                    edit.buf.delete();
                                    buf_changed = true;
                                }
                                // Motion keys: `set_selecting(shift)` extends a
                                // selection on Shift+motion and drops it on a
                                // plain motion. (Shift+Arrow no longer cycles
                                // workspaces here — that's gated to non-edit
                                // mode at the top of the key handler.)
                                _ if action == Some(Action::EditLeft) => {
                                    edit.buf.set_selecting(shift);
                                    edit.buf.move_left();
                                }
                                _ if action == Some(Action::EditRight) => {
                                    edit.buf.set_selecting(shift);
                                    edit.buf.move_right();
                                }
                                _ if action == Some(Action::EditUp) => {
                                    edit.buf.set_selecting(shift);
                                    edit.buf.move_up();
                                }
                                _ if action == Some(Action::EditDown) => {
                                    edit.buf.set_selecting(shift);
                                    edit.buf.move_down();
                                }
                                _ if action == Some(Action::EditStart) => {
                                    edit.buf.set_selecting(shift);
                                    edit.buf.move_buf_start();
                                }
                                _ if action == Some(Action::EditFinish) => {
                                    edit.buf.set_selecting(shift);
                                    edit.buf.move_buf_end();
                                }
                                _ if action == Some(Action::EditHome) => {
                                    edit.buf.set_selecting(shift);
                                    edit.buf.move_line_start();
                                }
                                _ if action == Some(Action::EditEnd) => {
                                    edit.buf.set_selecting(shift);
                                    edit.buf.move_line_end();
                                }
                                _ if action == Some(Action::EditPageUp) => {
                                    edit.buf.set_selecting(shift);
                                    edit.buf.move_up_rows(page_rows);
                                }
                                _ if action == Some(Action::EditPageDown) => {
                                    edit.buf.set_selecting(shift);
                                    edit.buf.move_down_rows(page_rows);
                                }
                                Key::Named(NamedKey::Space) => {
                                    edit.buf.insert_char(' ');
                                    buf_changed = true;
                                }
                                Key::Character(s) => {
                                    // Same invariant as the Julia input line
                                    // and `key_to_pty_bytes`: a Command
                                    // chord that resolved to no editor
                                    // action must not insert text either.
                                    let is_command =
                                        cfg!(target_os = "macos") && super_;
                                    for c in s.chars() {
                                        if !is_command && !c.is_control() {
                                            edit.buf.insert_char(c);
                                            buf_changed = true;
                                        }
                                    }
                                }
                                _ => {}
                            }
                            // Cursor moves count as a content change for
                            // the preview because the injected `█` is
                            // part of the rendered string — rebuild
                            // unconditionally for now (cheap; can
                            // optimise later if profiling shows it).
                            let _ = buf_changed;
                            state.rebuild_edit_preview();
                            state.window.request_redraw();
                            return;
                        }
                        // Page transport for paginated previews (ADR 0021):
                        // n/p and PgDn/PgUp re-fire preview.get for the
                        // *shown* node at page ± 1 (clamped). Driven purely
                        // by the reply's page extras — the chrome never
                        // knows it's a PDF. Consumed even at the clamp edges
                        // so a stray press on page 1/N doesn't leak into
                        // other handlers; on NON-paginated previews PgUp/
                        // PgDn fall through to the text-scroll arms below.
                        // No autorepeat: each page is a fresh pdftoppm run.
                        if let Some((page, count)) = state.preview_page {
                            if count > 1 && !event.repeat {
                                {
                                    let next = match action {
                                        Some(Action::PageNext) => Some(page.saturating_add(1).min(count)),
                                        Some(Action::PagePrev) => Some(page.saturating_sub(1).max(1)),
                                        _ => None,
                                    };
                                    if let Some(np) = next {
                                        if np != page {
                                            if let Some(node_id) =
                                                state.preview_node_id_fired.clone()
                                            {
                                                // New page opens at fit; drop
                                                // any pending zoom re-raster.
                                                state.preview_page_raster_pending = None;
                                                let (fit_w, fit_h) = state.preview_fit_px();
                                                let generation = state.next_preview_gen();
                                                if let Err(e) = state.send(
                                                    crate::transport::OutgoingReq::PreviewGet {
                                                        node_id,
                                                        workspace_id: state
                                                            .active_workspace_id
                                                            .clone(),
                                                        page: Some(np),
                                                        fit_w,
                                                        fit_h,
                                                        generation,
                                                    },
                                                ) {
                                                    tracing::warn!(error = %e,
                                                        "drop page-turn preview.get — channel closed");
                                                }
                                            }
                                        }
                                        state.last_key = Some(label);
                                        state.window.request_redraw();
                                        return;
                                    }
                                }
                            }
                        }
                        // Esc → tree; PgUp/PgDn / Ctrl+u / Ctrl+d /
                        // Home / End scroll the preview's flowed text.
                        // Held keys repeat for hold-to-scroll. Viewport
                        // size is taken from the chrome cell height of
                        // the pane — close enough to a body line for
                        // the user not to notice the small mismatch
                        // with the mouse-wheel row math, and it keeps
                        // all four panes on the same rule.
                        let h = state.pane_rects.preview.height as i32;
                        // PNG-pane zoom/pan routed through `KeyBindings`
                        // so `.sot/keybindings.toml` can rebind each
                        // action. Defaults: zoom in/out is Shift+Arrow
                        // up/down (plus `+`/`=`/`-`); reset is `r` or
                        // `0`; pan is the bare arrows. Order matters —
                        // ZoomIn checked before PanUp so Shift+ArrowUp
                        // doesn't double-fire (the Chord matcher ignores
                        // surplus shift for the `=`/`+` compatibility
                        // case, so the same key can match both action
                        // lists; first-hit wins). Pan step is 10% of
                        // the pane size per press so the perceived
                        // increment is constant regardless of zoom; the
                        // render-time clamp keeps the canvas covering
                        // the pane.
                        if let Some(img_px) = state.preview_png.as_ref().map(|q| q.size_px) {
                            const ZOOM_STEP: f32 = 1.25;
                            const PAN_FRAC: f32 = 0.1;
                            // Subtract the reserved figure-caption band before
                            // ANY of this: the image lives in `image_rect`, not
                            // the whole pane, and png_zoom_max keys off pane
                            // height (fit = min(w/iw, h/ih)). Computing the
                            // ceiling against the unreduced pane made the
                            // reachable max zoom depend on whether a caption
                            // happened to be set — silently 2.9%–6.4% short on a
                            // height-constrained image, since the render path
                            // re-clamps with the correct (larger) ceiling, so it
                            // degraded to under-zoom rather than a misdraw.
                            // Extracted so it's testable — see
                            // `preview_image_pane_px`. Reads the band height the
                            // last frame published; the one-frame lag is
                            // inherent (the band isn't known until the caption
                            // is shaped) and harmless, since the render pass
                            // re-clamps zoom against its own current ceiling.
                            let pane_rect = preview_image_pane_px(
                                (
                                    state.pane_rects.preview.width,
                                    state.pane_rects.preview.height,
                                ),
                                state.cell_w,
                                state.cell_h,
                                state.caption_band_px,
                            );
                            let (pane_w, pane_h) = (pane_rect.w, pane_rect.h);
                            // Zoom ceiling is per-image: how big a single
                            // source pixel may get on screen (16×16 px), not
                            // a fixed multiple of fit-to-pane. A dense raster
                            // whose native pixels are sub-screen-pixel at fit
                            // gets generous headroom; a tiny already-magnified
                            // image is held near fit.
                            let zoom_max = png_zoom_max(pane_w, pane_h, img_px);
                            let mut handled = true;
                            if !event.repeat
                                && action == Some(Action::PreviewPngReset)
                            {
                                state.preview_png_zoom = 1.0;
                                state.preview_png_pan_px = (0.0, 0.0);
                            } else if action == Some(Action::PreviewPngZoomIn) {
                                // Zoom sequence: 1.0 → 1.25 → 2 → 3 → 4 → …
                                // up to the per-image ceiling (`zoom_max`).
                                // Once we're past 1.5×, step in integer
                                // multiples of fit so increments stay
                                // predictable and avoid the moiré beating of
                                // fractional zoom against the nearest-
                                // neighbour sampler grid — which keeps dense
                                // scientific rasters reading crisply per-pixel
                                // (user ask 2026-05-22). The final value is
                                // clamped to the ceiling, so the last step may
                                // land on a fractional zoom that puts a source
                                // pixel at exactly 16 screen px.
                                let cur = state.preview_png_zoom;
                                let raw_next = if cur < 1.5 {
                                    let raw = cur * ZOOM_STEP;
                                    if raw >= 1.5 {
                                        2.0
                                    } else {
                                        raw
                                    }
                                } else {
                                    cur + 1.0
                                };
                                let next = raw_next.clamp(1.0, zoom_max);
                                state.scale_png_pan_for_zoom(cur, next);
                                state.preview_png_zoom = next;
                            } else if action == Some(Action::PreviewPngZoomOut) {
                                // Mirror of zoom-in: integer-step down
                                // from ≥ 2, then drop back through 1.25
                                // → 1.0. Hitting 2.0 → 1.25 is the
                                // discrete jump out of integer mode so
                                // the user lands cleanly on the
                                // multiplicative step below 1.5×.
                                let cur = state.preview_png_zoom;
                                let next = if cur > 1.5 {
                                    let raw = cur - 1.0;
                                    if raw < 2.0 {
                                        1.25
                                    } else {
                                        raw
                                    }
                                } else {
                                    (cur / ZOOM_STEP).max(1.0)
                                };
                                state.scale_png_pan_for_zoom(cur, next);
                                state.preview_png_zoom = next;
                                if state.preview_png_zoom <= 1.0 {
                                    state.preview_png_pan_px = (0.0, 0.0);
                                }
                            } else if action == Some(Action::PreviewPngPanLeft) {
                                state.preview_png_pan_px.0 += pane_w * PAN_FRAC;
                            } else if action == Some(Action::PreviewPngPanRight) {
                                state.preview_png_pan_px.0 -= pane_w * PAN_FRAC;
                            } else if action == Some(Action::PreviewPngPanUp) {
                                state.preview_png_pan_px.1 += pane_h * PAN_FRAC;
                            } else if action == Some(Action::PreviewPngPanDown) {
                                state.preview_png_pan_px.1 -= pane_h * PAN_FRAC;
                            } else if action == Some(Action::PreviewScalebarToggle)
                            {
                                // ADR 0034 Ctrl+S. With a scale present this
                                // flips the overlay; with NONE it opens the
                                // pixel-size prompt (§4 live entry) instead of
                                // no-opping, so an uncalibrated raster is one
                                // keystroke from a real bar.
                                if state.preview_scale.is_some() {
                                    state.scalebar_on = !state.scalebar_on;
                                } else if !state.begin_scale_entry() {
                                    state.status = "scalebar · no image previewed".to_string();
                                }
                            } else {
                                handled = false;
                            }
                            if handled {
                                // View carry is written through from the
                                // render pass (`preview_png_cache`) — a save
                                // here would record LAST frame's ROI.
                                // Paginated page (PDF): re-rasterize at the
                                // new zoom so text stays crisp past 1×.
                                state.maybe_reraster_page();
                                state.last_key = Some(label);
                                state.window.request_redraw();
                                return;
                            }
                        }
                        match action {
                            Some(Action::ReturnNav) if !event.repeat => {
                                state.set_focus(PaneFocus::NavTree);
                            }
                            // ADR 0022: `c` captures the visible image ROI and
                            // sends it to the LLM pane. `capture_roi` no-ops
                            // with a status hint when the preview isn't a
                            // croppable image.
                            //
                            // Deliberately unguarded on modifiers: Ctrl+C lands
                            // here too (it is not a PNG zoom/pan binding, so
                            // the block above falls through), and users reach
                            // for the universal copy chord out of habit. Both
                            // spellings are the same action and both move focus
                            // to the LLM pane once the crop paste lands — the
                            // focus move itself lives in the ImageCropped arm,
                            // not here, because the crop is async and may fail.
                            Some(Action::CaptureRegion) if !event.repeat => {
                                state.capture_roi();
                            }
                            // `e` enters edit mode for the cursored
                            // annotation, if there is one. Per the
                            // 2026-05-15T21:32Z spec: modal text input,
                            // minimal scope, no auto-clobber on save.
                            // `y` (vim "yank") copies fenced code blocks in
                            // the current markdown preview to the system
                            // clipboard. Multiple blocks are joined with a
                            // blank line so a "copy everything" call still
                            // pastes cleanly into another editor. No-op
                            // when the preview isn't markdown or carries no
                            // code blocks.
                            Some(Action::CopyCode) if !event.repeat => {
                                let sources = &state.preview_md.code_block_sources;
                                if !sources.is_empty() {
                                    let joined = sources.join("\n");
                                    let n = sources.len();
                                    match arboard::Clipboard::new()
                                        .and_then(|mut cb| cb.set_text(joined))
                                    {
                                        Ok(()) => tracing::info!(
                                            blocks = n,
                                            "yanked code block(s) to clipboard"
                                        ),
                                        Err(e) => tracing::warn!(
                                            error = %e,
                                            "failed to write code blocks to clipboard"
                                        ),
                                    }
                                }
                            }
                            // Open-style keys work from the preview pane too
                            // (same handlers as NavTree), acting on the file
                            // whose preview is SHOWING — pinned/badge-consumed
                            // previews can differ from the nav cursor — with
                            // fallback to the cursored row.
                            Some(Action::OpenExternal) if !event.repeat => {
                                let shown = state
                                    .previewed_files_path()
                                    .or_else(|| state.cursored_files_path());
                                state.open_path_external(shown);
                            }
                            Some(Action::OpenDocs) if !event.repeat =>
                            {
                                let path = state
                                    .previewed_files_path()
                                    .or_else(|| state.cursored_files_path())
                                    .unwrap_or_default();
                                state.docs_open_external(path);
                            }
                            Some(Action::OpenExecute) if !event.repeat =>
                            {
                                let shown = state
                                    .previewed_files_path()
                                    .or_else(|| state.cursored_files_path());
                                state.quarto_open_execute(shown);
                            }
                            Some(Action::EditFile) if !event.repeat => {
                                // Concept-annotation edit takes priority when one
                                // is loaded for the cursored node (content is
                                // already in `state.concept`).
                                let mut entered = false;
                                if let (Some(target), Some(info)) =
                                    (state.concept_target_fired.clone(), state.concept.as_ref())
                                {
                                    if info.target == target && info.exists {
                                        // Split out frontmatter so the
                                        // editable buffer holds the body
                                        // only; the header renders
                                        // read-only above the edit area
                                        // and is preserved verbatim on
                                        // save.
                                        let (header, body) = split_frontmatter(&info.content);
                                        state.edit_state = Some(EditState {
                                            target,
                                            expected_ast_hash: info.synced_against.clone(),
                                            header,
                                            original: body.clone(),
                                            buf: EditBuffer::new(body),
                                            confirm_discard: false,
                                            stale_banner: false,
                                            file_node_id: None,
                                            file_version: None,
                                        });
                                        state.rebuild_edit_preview();
                                        entered = true;
                                    }
                                }
                                // Otherwise, if the preview is showing a general
                                // file, edit the file itself: fetch its raw text
                                // via file.read and enter edit mode when the reply
                                // lands (see the FileRead handler). `pending_file_edit`
                                // matches the reply to this request.
                                if !entered && state.edit_state.is_none() {
                                    if let Some(node_id) = state.preview_node_id_fired.clone() {
                                        if node_id.starts_with("files:") {
                                            let ws = state.active_workspace_id.clone();
                                            if let Err(e) = state.send(OutgoingReq::FileRead {
                                                node_id: node_id.clone(),
                                                workspace_id: ws,
                                            }) {
                                                tracing::warn!(error = %e, "drop file.read for edit-enter");
                                            } else {
                                                state.pending_file_edit = Some(node_id);
                                            }
                                        }
                                    }
                                }
                            }
                            Some(Action::ScrollPageUp) => {
                                // 1/3-pane step preserves reading
                                // context — full-page jumps lost the
                                // user's place. Ctrl+u still half-pages
                                // for the "I really want to jump"
                                // case.
                                let page_step = (h / 3).max(1);
                                let new = (state.preview_scroll as i32 - page_step).max(0);
                                state.preview_scroll = new as u16;
                            }
                            Some(Action::ScrollPageDown) => {
                                let page_step = (h / 3).max(1);
                                let new = (state.preview_scroll as i32 + page_step).max(0);
                                state.preview_scroll = new as u16;
                            }
                            // Plain ArrowUp / ArrowDown scroll the
                            // markdown preview vertically by one row.
                            // PNG previews intercept these earlier
                            // (Action::PreviewPngPanUp/Down) so this
                            // arm only fires for non-PNG content.
                            Some(Action::PreviewUp) => {
                                state.preview_scroll = state.preview_scroll.saturating_sub(1);
                            }
                            Some(Action::PreviewDown) => {
                                state.preview_scroll = state.preview_scroll.saturating_add(1);
                            }
                            Some(Action::PreviewStart) if !event.repeat => {
                                state.preview_scroll = 0;
                            }
                            Some(Action::PreviewEnd) if !event.repeat => {
                                // Redraw clamps to (total - visible).
                                state.preview_scroll = u16::MAX;
                            }
                            Some(Action::PreviewHalfUp) => {
                                let new = (state.preview_scroll as i32 - h / 2).max(0);
                                state.preview_scroll = new as u16;
                            }
                            Some(Action::PreviewHalfDown) => {
                                let new = (state.preview_scroll as i32 + h / 2).max(0);
                                state.preview_scroll = new as u16;
                            }
                            _ => {}
                        }
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
                            return;
                        }
                        let is_paste_shortcut = !event.repeat && action == Some(Action::Paste);
                        if is_paste_shortcut {
                            forward_clipboard_paste_to_llm(state);
                            state.last_key = Some(label);
                            state.window.request_redraw();
                            return;
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
                                    return;
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
