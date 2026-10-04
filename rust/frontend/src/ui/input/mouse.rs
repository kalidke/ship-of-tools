//! Pointer events: cursor moves, clicks and the wheel, each sent to the pane it acts on.

use super::*;
use winit::dpi::PhysicalPosition;
use winit::keyboard::ModifiersState;

pub(in crate::ui) fn cursor_moved(state: &mut State, position: PhysicalPosition<f64>) {
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

pub(in crate::ui) fn mouse_input(state: &mut State, btn_state: ElementState, button: MouseButton) {
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

pub(in crate::ui) fn mouse_wheel(state: &mut State, modifiers: ModifiersState, delta: MouseScrollDelta) {
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
    let shift = modifiers.shift_key();
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
            wheel_terminal(state, rows_above);
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
            wheel_agent_pane(state, rows_above);
        }
        PaneFocus::NavTree => {
            // Nav is cursor-driven; wheel-scroll without
            // moving the cursor would desync the two. No-op
            // until there's a richer story for it.
        }
    }
}

fn wheel_terminal(state: &mut State, rows_above: i32) {
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

fn wheel_agent_pane(state: &mut State, rows_above: i32) {
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
