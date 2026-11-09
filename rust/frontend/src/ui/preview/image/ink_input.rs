//! The window's drawing wired to the pointer, the keys and the frame: what the person's hand does reaches `InkLayer`
//! only through here, and every question of whether the base is shown goes to `InkLayer::shown_base`.

use crate::ui::input::keypress::KeyPress;
use crate::ui::preview::image::ink::InkLayer;
use crate::ui::*;
use sot_protocol::ops::InkBase;
use std::ops::ControlFlow::{self, Break, Continue};

impl State {
    pub(in crate::ui) fn shown_ink_base(&self) -> Option<InkBase> {
        InkLayer::shown_base(self.raster.node(), self.editor.is_editing(), self.hello_refused.refused(self.selected.host()))
    }

    /// Before a frame and each pointer event: the layer drops what the shown base no longer carries.
    pub(in crate::ui) fn ink_sync(&mut self) {
        let shown = self.shown_ink_base();
        self.ink.sync(shown.as_ref(), self.raster.served_dims());
    }

    /// A left press that drawing takes (it starts no text selection): focus moves to the preview, then the stroke begins.
    pub(in crate::ui) fn press_ink(&mut self) -> bool {
        self.ink_sync();
        let (shown, p) = (self.shown_ink_base(), self.pointer.at_px());
        let (Some((canvas, image)), Some(src)) = (self.ink.frame(), self.raster.served_dims()) else { return false };
        if !self.ink.reaches(shown.as_ref(), p, &canvas, &image) {
            return false;
        }
        self.set_focus(PaneFocus::Preview);
        if let Some(text) = self.ink.press(shown.as_ref(), p, &canvas, &image, src) {
            self.status.set(text);
        }
        self.window.request_redraw();
        true
    }

    /// Motion: a live left stroke grows, and a right drag erases.
    pub(in crate::ui) fn ink_moved(&mut self) {
        let Some((canvas, image)) = self.ink.frame() else { return };
        if self.ink.live().is_none() && !self.ink.right_down() {
            return;
        }
        self.ink_sync();
        let p = self.pointer.at_px();
        if self.ink.live().is_some() {
            if let Some(text) = self.ink.drag(p, &canvas) {
                self.status.set(text);
            }
        } else if self.ink.right_down() {
            let shown = self.shown_ink_base();
            self.ink.erase(shown.as_ref(), p, &canvas, &image);
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
        self.ink.set_right_down(down);
        match (down, self.ink.frame()) {
            (true, Some((canvas, image))) => {
                let (shown, p) = (self.shown_ink_base(), self.pointer.at_px());
                self.ink.erase(shown.as_ref(), p, &canvas, &image);
            }
            _ => self.ink.erase_end(),
        }
        self.window.request_redraw();
    }

    /// The window lost focus: a live stroke is committed, and a held erase ends.
    pub(in crate::ui) fn ink_focus_lost(&mut self) {
        self.ink.release();
        self.ink.set_right_down(false);
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
