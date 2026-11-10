//! Pointer events: cursor moves, clicks and the wheel, each sent to the pane it acts on.

use super::*;
use crate::ui::input::global_keys::confirm_quit_key;
use winit::dpi::PhysicalPosition;
use winit::keyboard::ModifiersState;

pub(in crate::ui) fn cursor_moved(state: &mut State, position: PhysicalPosition<f64>) {
    state.pointer.moved_to((position.x as f32, position.y as f32));
    // Both owners extend an active drag with non-strict mapping to the visible pane edge.
    let end = state.llm_cell_at_px(state.pointer.at_px(), false);
    let agent_changed = state.agent_pane.extend_drag(end);
    let preview_changed = state.shaped.extend_drag(state.pointer.at_px());
    if agent_changed || preview_changed { state.window.request_redraw(); }
    state.ink_moved();
}

pub(in crate::ui) fn mouse_input(state: &mut State, event_loop: &ActiveEventLoop, btn_state: ElementState, button: MouseButton) {
    pointer_continuation(state, btn_state == ElementState::Pressed, button,
        |state, pressed| state.report_presence(pressed),
        |state| {
            if state.focus_prompts.quit_choice().is_none() { return None; }
            let (x, y) = ((state.pointer.at_px().0 - state.canvas.origin_x()) / state.canvas.cell_w().max(1.0),
                (state.pointer.at_px().1 - state.canvas.origin_y()) / state.canvas.cell_h().max(1.0));
            let left_at = (button == MouseButton::Left && x >= 0.0 && y >= 0.0).then_some((x as u16, y as u16));
            Some(quit_prompt_click(state.close_dialog().and_then(|d| d.answers), left_at))
        },
        |state, step| confirm_quit_key(state, event_loop, step),
        |state| {
            let dismissed = state.focus_prompts.dismiss_stay_press();
            if dismissed { state.window.request_redraw(); }
            dismissed
        },
        press_selections,
        |state| { state.agent_pane.release(); state.shaped.release(); state.ink_released(); },
        |state, down| state.ink_right(down));
}

/// Only the geometry and effects needed to route a press into the two selectable text panes.
trait SelectionContext {
    fn agent_cell(&self) -> Option<(u16, u16)>;
    fn preview_cursor(&self) -> Option<cosmic_text::Cursor>;
    /// A left press the drawing takes: it focuses the preview itself and starts no selection.
    fn draw_press(&mut self) -> bool;
    fn focus_selection_pane(&mut self, pane: PaneFocus);
    fn press_agent(&mut self, cell: Option<(u16, u16)>);
    fn press_preview(&mut self, cursor: Option<cosmic_text::Cursor>);
}

fn press_selections(context: &mut impl SelectionContext) {
    if context.draw_press() { context.press_agent(None); return; }
    let cell = context.agent_cell();
    let cursor = context.preview_cursor();
    agent_press_continuation(context, cell, |c| c.focus_selection_pane(PaneFocus::Llm), |c, cell| c.press_agent(cell));
    agent_press_continuation(context, cursor, |c| c.focus_selection_pane(PaneFocus::Preview), |c, cursor| c.press_preview(cursor));
}

impl SelectionContext for State {
    fn agent_cell(&self) -> Option<(u16, u16)> { self.llm_cell_at_px(self.pointer.at_px(), true) }
    fn preview_cursor(&self) -> Option<cosmic_text::Cursor> {
        let pane = self.pane_rects.preview;
        if pane.width == 0 || pane.height == 0 || self.editor.is_editing() || !self.preview_shows().4 { return None; }
        self.shaped.hit(self.pointer.at_px(), true)
    }
    fn draw_press(&mut self) -> bool { self.press_ink() }
    fn focus_selection_pane(&mut self, pane: PaneFocus) { self.set_focus(pane); }
    fn press_agent(&mut self, cell: Option<(u16, u16)>) {
        if self.agent_pane.press(cell) { self.window.request_redraw(); }
    }
    fn press_preview(&mut self, cursor: Option<cosmic_text::Cursor>) {
        if self.shaped.press(cursor) { self.window.request_redraw(); }
    }
}

/// Presence precedes modal interception; a release ends a drag, of either button, even when a dialog is showing.
pub(in crate::ui) fn pointer_continuation<S>(state: &mut S, pressed: bool, button: MouseButton,
    presence: impl FnOnce(&mut S, bool), question: impl FnOnce(&mut S) -> Option<QuitPromptStep>,
    answer: impl FnOnce(&mut S, QuitPromptStep), dismiss: impl FnOnce(&mut S) -> bool,
    press: impl FnOnce(&mut S), release: impl FnOnce(&mut S), right: impl FnOnce(&mut S, bool)) {
    presence(state, pressed);
    if pressed {
        if let Some(step) = question(state) { answer(state, step); return; }
        if dismiss(state) { return; }
    }
    match button {
        MouseButton::Left if pressed => press(state),
        MouseButton::Left => release(state),
        MouseButton::Right => right(state, pressed),
        _ => {}
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
    /// The pane shows neither the text nor the editor (the canvas, an image, the overlay): the wheel has nothing to scroll.
    preview_hidden: bool,
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
    if geometry.focus == PaneFocus::Preview && !geometry.preview_hidden {
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
    if rows_above == 0 || geometry.focus == PaneFocus::Preview && geometry.preview_hidden {
        return;
    }
    context.route_wheel_rows(rows_above);
}

impl WheelContext for State {
    fn report_wheel_presence(&mut self) { self.report_presence(true); }
    fn wheel_geometry(&mut self) -> WheelGeometry {
        let shows = self.preview_shows();
        WheelGeometry { cell_w: self.canvas.cell_w(), cell_h: self.canvas.cell_h(), focus: self.focus_prompts.focus(), residue: self.pointer.residue(), preview_hidden: !(shows.3 || shows.4) }
    }
    fn scroll_preview_horizontal(&mut self, pixels: f32) {
        self.md_media.scroll_tables(pixels);
        self.window.request_redraw();
    }
    fn store_wheel_residue(&mut self, residue: f32) { self.pointer.store_residue(residue); }
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
        PaneFocus::Repl => match help::DrawerInput::of(state.layout_help.drawer()) {
            help::DrawerInput::Terminal => wheel_terminal(state, rows_above),
            help::DrawerInput::Repl => {
                state.repl_model.scroll_rows(rows_above);
                state.window.request_redraw();
            }
            // Only the Chat tenant takes the wheel; Monitor and Help scroll nothing it can reach, and never the REPL.
            help::DrawerInput::Inert => {
                let _ = drawer::chat::chat_apply(state, drawer::chat::chat_wheel(state.layout_help.drawer(), rows_above));
            }
        },
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
    let mouse_on = state.term_drawer.mouse_tracking_on();
    if mouse_on {
        let button = if rows_above > 0 { 64 } else { 65 };
        let n = rows_above.unsigned_abs().min(8);
        let seq = format!("\x1b[<{button};1;1M");
        state.term_drawer.forward_input(seq.as_bytes(), n);
    } else {
        scroll_drawer_ring(state, rows_above);
    }
    state.window.request_redraw();
}

fn wheel_agent_pane(state: &mut State, rows_above: i32) {
    match state.agent_pane.wheel(rows_above) {
        WheelEffect::Pending => {},
        WheelEffect::ClearResidue => state.pointer.clear_residue(),
        WheelEffect::Redraw => state.window.request_redraw(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SelectionProbe {
        shaped: crate::ui::preview::shaped::ShapedPreview,
        at: (f32, f32),
        agent: Option<(u16, u16)>,
        focus: PaneFocus,
        text_shown: bool,
        focused_before_press: bool,
        drawing: bool,
        pressed_agent: Vec<Option<(u16, u16)>>,
    }
    impl SelectionContext for SelectionProbe {
        fn agent_cell(&self) -> Option<(u16, u16)> { self.agent }
        fn preview_cursor(&self) -> Option<cosmic_text::Cursor> {
            self.text_shown.then(|| self.shaped.hit(self.at, true)).flatten()
        }
        fn draw_press(&mut self) -> bool {
            if self.drawing { self.focus = PaneFocus::Preview; }
            self.drawing
        }
        fn focus_selection_pane(&mut self, pane: PaneFocus) { self.focus = pane; }
        fn press_agent(&mut self, cell: Option<(u16, u16)>) { self.pressed_agent.push(cell); }
        fn press_preview(&mut self, cursor: Option<cosmic_text::Cursor>) {
            self.focused_before_press = cursor.is_some() && self.focus == PaneFocus::Preview;
            self.shaped.press(cursor);
        }
    }

    #[test]
    fn preview_press_focuses_and_selects_and_agent_press_clears_it() {
        use crate::ui::preview::{markdown::{MarkdownPreview, highlight::HighlightService}, shaped::ShapedPreview};
        use crate::ui::render::{quad::ScreenRect, text::EXTRA_TOP_PAD_PX};
        let md = MarkdownPreview::new_plain(&mut glyphon::FontSystem::new(), "alpha\nbravo", 200.0, 1.0);
        let rect = ScreenRect { x: 20.0, y: 30.0, w: 200.0, h: 80.0 };
        let mut probe = SelectionProbe {
            shaped: ShapedPreview::new(md, HighlightService::new().unwrap(), rect),
            at: (rect.x, rect.y + EXTRA_TOP_PAD_PX + 5.0), agent: None, focus: PaneFocus::NavTree,
            text_shown: true, focused_before_press: false, drawing: false, pressed_agent: Vec::new(),
        };
        pointer_continuation(&mut probe, true, MouseButton::Left, |_, _| {}, |_| None, |_, _| {}, |_| false,
            press_selections, |p| p.shaped.release(), |_, _| {});
        assert_eq!(probe.focus, PaneFocus::Preview);
        assert!(probe.focused_before_press);
        let end = (rect.x + rect.w - 1.0, rect.y + EXTRA_TOP_PAD_PX + 25.0);
        assert!(probe.shaped.extend_drag(end));
        pointer_continuation(&mut probe, false, MouseButton::Left, |_, _| {}, |_| None, |_, _| {}, |_| false,
            press_selections, |p| p.shaped.release(), |_, _| {});
        assert_eq!(probe.shaped.selected_text().as_deref(), Some("alpha\nbravo"));
        assert!(!probe.shaped.extend_drag(probe.at));

        probe.at = (0.0, 0.0);
        probe.agent = Some((0, 0));
        press_selections(&mut probe);
        assert_eq!(probe.focus, PaneFocus::Llm);
        assert!(probe.shaped.selected_text().is_none());

        probe.agent = None;
        probe.at = (rect.x, rect.y + EXTRA_TOP_PAD_PX + 5.0);
        probe.text_shown = false;
        press_selections(&mut probe);
        assert_eq!(probe.focus, PaneFocus::Llm, "an image or editor does not start preview selection");
        assert!(!probe.shaped.extend_drag(end));
    }

    #[test]
    fn a_press_the_drawing_takes_focuses_the_preview_and_starts_no_selection() {
        use crate::ui::preview::{markdown::{MarkdownPreview, highlight::HighlightService}, shaped::ShapedPreview};
        use crate::ui::render::{quad::ScreenRect, text::EXTRA_TOP_PAD_PX};
        let md = MarkdownPreview::new_plain(&mut glyphon::FontSystem::new(), "alpha\nbravo", 200.0, 1.0);
        let rect = ScreenRect { x: 20.0, y: 30.0, w: 200.0, h: 80.0 };
        let mut probe = SelectionProbe {
            shaped: ShapedPreview::new(md, HighlightService::new().unwrap(), rect),
            at: (rect.x, rect.y + EXTRA_TOP_PAD_PX + 5.0), agent: Some((0, 0)), focus: PaneFocus::NavTree,
            text_shown: true, focused_before_press: false, drawing: true, pressed_agent: Vec::new(),
        };
        press_selections(&mut probe);
        assert_eq!(probe.focus, PaneFocus::Preview);
        assert_eq!(probe.pressed_agent, [None], "a drawing press clears the agent pane's selection");
        assert!(!probe.focused_before_press && !probe.shaped.extend_drag((rect.x + 100.0, rect.y + 60.0)),
            "a drawing press begins no text selection");
        probe.drawing = false;
        press_selections(&mut probe);
        assert_eq!(probe.pressed_agent, [None, Some((0, 0))], "any other press routes as before");
    }

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
            geometry: WheelGeometry { cell_w: 2.0, cell_h: 4.0, focus: PaneFocus::Llm, residue: 0.0, preview_hidden: false },
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

    #[test]
    fn the_wheel_scrolls_only_what_the_preview_pane_shows() {
        let mut context = WheelProbe {
            geometry: WheelGeometry { cell_w: 2.0, cell_h: 4.0, focus: PaneFocus::Preview, residue: 0.0, preview_hidden: true },
            events: Vec::new(),
        };
        for delta in [MouseScrollDelta::LineDelta(0.0, 1.0), MouseScrollDelta::LineDelta(1.0, 0.0)] {
            dispatch_wheel(&mut context, ModifiersState::empty(), delta);
        }
        assert!(!context.events.iter().any(|e| matches!(e, Event::Rows(_) | Event::Horizontal(_))), "{:?}", context.events);
        context.geometry.preview_hidden = false;
        dispatch_wheel(&mut context, ModifiersState::empty(), MouseScrollDelta::LineDelta(0.0, 1.0));
        assert!(context.events.contains(&Event::Rows(3)));
    }
}
