//! Pointer events: cursor moves, clicks and the wheel, each sent to the pane it acts on.

use super::*;
use crate::ui::input::global_keys::confirm_quit_key;
use winit::dpi::PhysicalPosition;
use winit::keyboard::ModifiersState;

pub(in crate::ui) fn cursor_moved(state: &mut State, position: PhysicalPosition<f64>) {
    state.cursor_px = (position.x as f32, position.y as f32);
    // Extend the LLM-pane selection while the user is dragging.
    // Drag uses non-strict cell mapping so the user can pull
    // outside the pane to extend selection to the edge.
    let end = state.llm_cell_at_px(state.cursor_px, false);
    if state.agent_pane.extend_drag(end) { state.window.request_redraw(); }
}

pub(in crate::ui) fn mouse_input(state: &mut State, event_loop: &ActiveEventLoop, btn_state: ElementState, button: MouseButton) {
    // A real press, whichever button and whatever it does below: presence
    // (design point A) precedes and is independent of the click's own
    // handling. A release is not a press: a window can be sent one for a
    // click that began elsewhere.
    state.report_presence(btn_state == ElementState::Pressed);
    // While the quit question is open, a press answers it and goes no further (`quit_prompt_click`).
    if btn_state == ElementState::Pressed && matches!(state.nav_prompt, Some(NavPrompt::ConfirmQuit { .. })) {
        let (x, y) = ((state.cursor_px.0 - state.chrome_origin_x) / state.cell_w.max(1.0),
            (state.cursor_px.1 - state.chrome_origin_y) / state.cell_h.max(1.0));
        let left_at = (button == MouseButton::Left && x >= 0.0 && y >= 0.0).then_some((x as u16, y as u16));
        let step = quit_prompt_click(state.close_dialog().and_then(|d| d.answers), left_at);
        confirm_quit_key(state, event_loop, step);
        return;
    }
    // A press ends a stayed relaunch's dialog, as a key does, and reaches no pane.
    if btn_state == ElementState::Pressed && dismiss_stay_press(&mut state.relaunch_stayed) {
        state.window.request_redraw();
        return;
    }
    if button == MouseButton::Left {
        match btn_state {
            ElementState::Pressed => {
                // Mouse-down inside the LLM pane starts a new
                // selection at the clicked cell and grabs
                // focus so subsequent keys (Ctrl+Shift+C copy)
                // land in the LLM arm. Outside the pane: clear
                // any existing selection so a click elsewhere
                // dismisses the highlight.
                let cell = state.llm_cell_at_px(state.cursor_px, true);
                if cell.is_some() { state.set_focus(PaneFocus::Llm); }
                if state.agent_pane.press(cell) { state.window.request_redraw(); }
            }
            ElementState::Released => {
                // Mouse-up just ends the drag — selection
                // stays painted so the user has time to hit
                // Ctrl+Shift+C.
                state.agent_pane.release();
            }
        }
    }
}

/// The wheel dispatcher needs only pointer geometry and its routing effects, never general State access.
#[derive(Clone, Copy)]
struct WheelGeometry {
    cell_w: f32,
    cell_h: f32,
    focus: PaneFocus,
    residue: f32,
}
trait WheelContext {
    fn report_wheel_presence(&mut self);
    fn wheel_geometry(&mut self) -> WheelGeometry;
    fn scroll_preview_horizontal(&mut self, pixels: f32);
    fn store_wheel_residue(&mut self, residue: f32);
    fn route_wheel_rows(&mut self, rows: i32);
}

pub(in crate::ui) fn mouse_wheel(state: &mut State, modifiers: ModifiersState, delta: MouseScrollDelta) {
    dispatch_wheel(state, modifiers, delta);
}

fn dispatch_wheel(context: &mut impl WheelContext, modifiers: ModifiersState, delta: MouseScrollDelta) {
    // Presence precedes all routing, including a fractional step that produces no whole row.
    context.report_wheel_presence();
    let geometry = context.wheel_geometry();
    // Convert the platform delta into fractional rows, then
    // accumulate. Precision touchpads emit small sub-row
    // pixel deltas that would otherwise truncate to zero
    // and feel dead. Standard wheel ticks (LineDelta y=1)
    // step three rows, matching the common TUI cadence.
    let (delta_rows, raw_px_y, raw_px_x): (f32, f32, f32) = match delta {
        MouseScrollDelta::LineDelta(x, y) => {
            (y * 3.0, y * geometry.cell_h * 3.0, x * geometry.cell_w * 3.0)
        }
        MouseScrollDelta::PixelDelta(pos) => {
            (pos.y as f32 / geometry.cell_h, pos.y as f32, pos.x as f32)
        }
    };
    // Shift+wheel-Y *or* a horizontal-axis wheel event in
    // Preview focus → wide-table horizontal scroll. Take
    // it before the vertical accumulator runs so a held
    // Shift doesn't also walk preview_scroll. Sign: wheel
    // up shifts the table left (reveal more right-side
    // content). Clamp happens in redraw.
    let shift = modifiers.shift_key();
    if geometry.focus == PaneFocus::Preview {
        let h_px = if shift && raw_px_y.abs() > 0.0 {
            -raw_px_y
        } else if raw_px_x.abs() > 0.0 {
            raw_px_x
        } else {
            0.0
        };
        if h_px.abs() > 0.0 {
            context.scroll_preview_horizontal(h_px);
            return;
        }
    }
    let accumulated = geometry.residue + delta_rows;
    let rows_above = accumulated.trunc() as i32;
    let residue = accumulated - rows_above as f32;
    context.store_wheel_residue(residue);
    tracing::info!(
        delta_rows,
        rows_above,
        residue,
        ?geometry.focus,
        "wheel"
    );
    if rows_above == 0 {
        return;
    }
    context.route_wheel_rows(rows_above);
}

impl WheelContext for State {
    fn report_wheel_presence(&mut self) { self.report_presence(true); }
    fn wheel_geometry(&mut self) -> WheelGeometry {
        WheelGeometry { cell_w: self.cell_w, cell_h: self.cell_h, focus: self.focus, residue: self.wheel_residue_y }
    }
    fn scroll_preview_horizontal(&mut self, pixels: f32) {
        self.md_table_scroll_px = (self.md_table_scroll_px + pixels).max(0.0);
        self.window.request_redraw();
    }
    fn store_wheel_residue(&mut self, residue: f32) { self.wheel_residue_y = residue; }
    fn route_wheel_rows(&mut self, rows: i32) { route_wheel_rows(self, rows); }
}

fn route_wheel_rows(state: &mut State, rows_above: i32) {
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
            state.repl_model.scroll_rows(rows_above);
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
    let mouse_on = state
        .local_term
        .as_ref()
        .map(|t| t.mouse_tracking_on())
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
    } else {
        scroll_drawer_ring(state, rows_above);
    }
    state.window.request_redraw();
}

fn wheel_agent_pane(state: &mut State, rows_above: i32) {
    match state.agent_pane.wheel(rows_above) {
        WheelEffect::Pending => {},
        WheelEffect::ClearResidue => state.wheel_residue_y = 0.0,
        WheelEffect::Redraw => state.window.request_redraw(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    enum Event { Presence, Geometry, Residue, Horizontal(f32), Rows(i32) }
    struct WheelProbe {
        geometry: WheelGeometry,
        events: Vec<Event>,
    }
    impl WheelContext for WheelProbe {
        fn report_wheel_presence(&mut self) { self.events.push(Event::Presence); }
        fn wheel_geometry(&mut self) -> WheelGeometry {
            self.events.push(Event::Geometry);
            self.geometry
        }
        fn scroll_preview_horizontal(&mut self, pixels: f32) { self.events.push(Event::Horizontal(pixels)); }
        fn store_wheel_residue(&mut self, residue: f32) {
            self.events.push(Event::Residue);
            self.geometry.residue = residue;
        }
        fn route_wheel_rows(&mut self, rows: i32) { self.events.push(Event::Rows(rows)); }
    }

    #[test]
    fn a_wheel_step_reports_presence_before_fractional_routing() {
        let mut context = WheelProbe {
            geometry: WheelGeometry { cell_w: 2.0, cell_h: 4.0, focus: PaneFocus::Llm, residue: 0.0 },
            events: Vec::new(),
        };
        dispatch_wheel(&mut context, ModifiersState::empty(), MouseScrollDelta::LineDelta(0.0, 0.05));
        assert_eq!(context.events, vec![Event::Presence, Event::Geometry, Event::Residue]);
        assert!((context.geometry.residue - 0.15).abs() < 0.00001);

        context.events.clear();
        dispatch_wheel(&mut context, ModifiersState::empty(), MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 0.4)));
        assert_eq!(context.events, vec![Event::Presence, Event::Geometry, Event::Residue]);
        assert!((context.geometry.residue - 0.25).abs() < 0.00001);

        context.events.clear();
        dispatch_wheel(&mut context, ModifiersState::empty(), MouseScrollDelta::LineDelta(0.0, 1.0));
        assert_eq!(context.events, vec![Event::Presence, Event::Geometry, Event::Residue, Event::Rows(3)]);
        assert!((context.geometry.residue - 0.25).abs() < 0.00001);

        context.events.clear();
        context.geometry = WheelGeometry { focus: PaneFocus::Preview, ..context.geometry };
        dispatch_wheel(&mut context, ModifiersState::empty(), MouseScrollDelta::LineDelta(1.0, 0.0));
        assert_eq!(context.events, vec![Event::Presence, Event::Geometry, Event::Horizontal(6.0)]);
        assert!((context.geometry.residue - 0.25).abs() < 0.00001);
    }
}
