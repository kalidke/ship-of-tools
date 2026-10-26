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
    pointer_continuation(state, btn_state == ElementState::Pressed, button == MouseButton::Left,
        |state, pressed| state.report_presence(pressed),
        |state| {
            if state.focus_prompts.quit_choice().is_none() { return None; }
            let (x, y) = ((state.cursor_px.0 - state.chrome_origin_x) / state.cell_w.max(1.0),
                (state.cursor_px.1 - state.chrome_origin_y) / state.cell_h.max(1.0));
            let left_at = (button == MouseButton::Left && x >= 0.0 && y >= 0.0).then_some((x as u16, y as u16));
            Some(quit_prompt_click(state.close_dialog().and_then(|d| d.answers), left_at))
        },
        |state, step| confirm_quit_key(state, event_loop, step),
        |state| {
            let dismissed = state.focus_prompts.dismiss_stay_press();
            if dismissed { state.window.request_redraw(); }
            dismissed
        },
        |state| {
            let cell = state.llm_cell_at_px(state.cursor_px, true);
            agent_press_continuation(state, cell, |state| state.set_focus(PaneFocus::Llm), |state, cell| {
                if state.agent_pane.press(cell) { state.window.request_redraw(); }
            });
        },
        |state| { state.agent_pane.release(); });
}

/// Presence precedes modal interception; a release ends drag even when a dialog is showing.
pub(in crate::ui) fn pointer_continuation<S>(state: &mut S, pressed: bool, left: bool,
    presence: impl FnOnce(&mut S, bool), question: impl FnOnce(&mut S) -> Option<QuitPromptStep>,
    answer: impl FnOnce(&mut S, QuitPromptStep), dismiss: impl FnOnce(&mut S) -> bool,
    press: impl FnOnce(&mut S), release: impl FnOnce(&mut S)) {
    presence(state, pressed);
    if pressed {
        if let Some(step) = question(state) { answer(state, step); return; }
        if dismiss(state) { return; }
    }
    if left {
        if pressed { press(state); } else { release(state); }
    }
}

/// The same clicked cell selects the pane and begins its selection, in that order. Outside clears selection.
pub(in crate::ui) fn agent_press_continuation<S, C>(state: &mut S, cell: Option<C>,
    focus: impl FnOnce(&mut S), press: impl FnOnce(&mut S, Option<C>)) {
    if cell.is_some() { focus(state); }
    press(state, cell);
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
        WheelGeometry { cell_w: self.cell_w, cell_h: self.cell_h, focus: self.focus_prompts.focus(), residue: self.wheel_residue_y }
    }
    fn scroll_preview_horizontal(&mut self, pixels: f32) {
        self.md_media.scroll_tables(pixels);
        self.window.request_redraw();
    }
    fn store_wheel_residue(&mut self, residue: f32) { self.wheel_residue_y = residue; }
    fn route_wheel_rows(&mut self, rows: i32) { route_wheel_rows(self, rows); }
}

fn route_wheel_rows(state: &mut State, rows_above: i32) {
    if let Some(effect) = state.layout_help.help_wheel(rows_above, state.focus_prompts.focus(), &state.bindings) {
        effect.apply(state, None);
        return;
    }

    // Preview's scroll origin is the top of the doc, REPL
    // and LLM's are the tail — sign flip lives in each
    // pane's apply step so a single positive `rows_above`
    // feels like "show content above" everywhere.
    match state.focus_prompts.focus() {
        PaneFocus::Repl if state.layout_help.drawer() == DrawerContent::Terminal => {
            wheel_terminal(state, rows_above);
        }
        PaneFocus::Repl => {
            state.repl_model.scroll_rows(rows_above);
            state.window.request_redraw();
        }
        PaneFocus::Preview => {
            state.shaped.scroll_by(-rows_above);
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
