//! The window's drawing wired to the pointer, the keys and the frame: what the person's hand does reaches `InkLayer`
//! only through here, and every question of whether the base is shown goes to `InkLayer::shown_base`.

use crate::ui::input::keypress::KeyPress;
use crate::ui::*;
use sot_protocol::ops::InkBase;
use std::ops::ControlFlow::{self, Break, Continue};

impl State {
    pub(in crate::ui) fn shown_ink_base(&self) -> Option<InkBase> {
        self.ink.shown_base(self.raster.node(), self.editor.is_editing(), self.hello_refused.refused(self.selected.host()))
    }

    fn ink_src(&self, shown: Option<&InkBase>) -> Option<(u32, u32)> {
        crate::ui::preview::image::ink::source_dims(shown, self.raster.served_dims())
    }

    /// Before a frame and each pointer event: the layer drops what the shown base no longer carries.
    pub(in crate::ui) fn ink_sync(&mut self) {
        let shown = self.shown_ink_base();
        self.ink.sync(shown.as_ref(), self.ink_src(shown.as_ref()));
    }

    /// A left press that drawing takes (it starts no text selection): focus moves to the preview, then the stroke begins.
    pub(in crate::ui) fn press_ink(&mut self) -> bool {
        self.ink_sync();
        let (shown, p) = (self.shown_ink_base(), self.pointer.at_px());
        let Some(note) = self.ink.left_press(shown.as_ref(), p, self.ink_src(shown.as_ref())) else { return false };
        self.set_focus(PaneFocus::Preview);
        if let Some(text) = note {
            self.status.set(text);
        }
        self.window.request_redraw();
        true
    }

    /// Motion: a live left stroke grows, and a right drag erases.
    pub(in crate::ui) fn ink_moved(&mut self) {
        if !self.ink.busy() {
            return;
        }
        self.ink_sync();
        let (shown, p) = (self.shown_ink_base(), self.pointer.at_px());
        if let Some(text) = self.ink.moved(shown.as_ref(), p) {
            self.status.set(text.to_string());
        }
        self.window.request_redraw();
    }

    /// The left button is up: the live stroke is committed.
    pub(in crate::ui) fn ink_released(&mut self) {
        self.ink_sync();
        self.ink.release();
        self.window.request_redraw();
    }

    /// The right button went down or up: a drag erases from its press.
    pub(in crate::ui) fn ink_right(&mut self, down: bool) {
        self.ink_sync();
        let (shown, p) = (self.shown_ink_base(), self.pointer.at_px());
        self.ink.right(down, shown.as_ref(), p);
        self.window.request_redraw();
    }

    /// The window lost focus: a live stroke is committed, and a held erase ends.
    pub(in crate::ui) fn ink_focus_lost(&mut self) {
        self.ink_sync();
        self.ink.focus_lost();
        self.window.request_redraw();
    }
}

pub(in crate::ui) fn ink_key(state: &mut State, key: KeyPress<'_>, label: String) -> ControlFlow<()> {
    if key.event.repeat {
        return Continue(());
    }
    state.ink_sync();
    let shown = state.shown_ink_base();
    match key.action {
        Some(Action::PreviewDraw) if shown.is_none() => state.status.set("drawing: no image to draw on".to_string()),
        Some(Action::PreviewDraw) => {
            let on = !state.ink.is_on();
            state.ink.set_on(on);
            if !on {
                state.ink.release();
            }
            state.status.set(if on {
                "drawing on: press and drag on the image; u undo, Delete clear, d off".to_string()
            } else {
                "drawing off".to_string()
            });
        }
        Some(Action::PreviewCanvas) if state.hello_refused.refused(state.selected.host()) => {}
        Some(Action::PreviewCanvas) => {
            let canvas = !state.ink.canvas();
            state.ink.set_canvas(canvas);
            state.status.set(if canvas {
                "canvas: draw with the pointer; u undo, Delete clear, Shift+D back".to_string()
            } else {
                "canvas hidden".to_string()
            });
        }
        Some(Action::PreviewInkUndo) => state.ink.undo(shown.as_ref()),
        Some(Action::PreviewInkClear) => {
            let before = state.ink.revision();
            state.ink.clear(shown.as_ref());
            if state.ink.revision() != before {
                state.status.set("drawing cleared: u brings it back".to_string());
            }
        }
        _ => return Continue(()),
    }
    state.last_key = Some(label);
    state.window.request_redraw();
    Break(())
}
