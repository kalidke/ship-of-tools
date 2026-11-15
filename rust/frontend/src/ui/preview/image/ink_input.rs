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
            self.preview_says(text);
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
            self.preview_says(text.to_string());
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

/// Why `d` finds no base to draw on, in words for the person. The key resolves only while a picture is shown, so a
/// picture whose node is not an image file (an SVG, a PDF page, a video poster, a plugin's picture) is one the daemon
/// cannot compose into the agent's: the words say what drawing is for and the two ways to draw now; `canvas` is the
/// bound label of `preview.canvas`. Under the refused-hello overlay the connection notice is why; otherwise (no picture
/// shown, or one with no node) there is no image file to draw on.
fn no_base_line(refused: bool, raster_shown: bool, node: Option<&str>, canvas: &str) -> String {
    match node {
        _ if refused => "Can't draw while the connection notice covers the preview.".to_string(),
        Some(n) if raster_shown && !State::is_image_node_id(n) => format!(
            "Can't draw on {}. Drawing (d) marks up an image file (PNG, JPEG, TIFF and the like) for this row's agent, \
             which sees your marks with your next prompt. To draw anyway, open a PNG of it, or press {canvas} for a blank canvas.",
            picture_name(n)),
        _ => "Can't draw here: there is no image file to draw on.".to_string(),
    }
}

/// What the person calls the picture a non-image node shows, by its extension.
fn picture_name(node: &str) -> &'static str {
    let lower = node.to_ascii_lowercase();
    if lower.ends_with(".svg") { "this SVG" }
    else if lower.ends_with(".pdf") { "this PDF page" }
    else if sot_protocol::video_path::video_mime(node).is_some() { "this video frame" }
    else { "this picture" }
}

pub(in crate::ui) fn ink_key(state: &mut State, key: KeyPress<'_>, label: String) -> ControlFlow<()> {
    if key.event.repeat {
        return Continue(());
    }
    state.ink_sync();
    let shown = state.shown_ink_base();
    match key.action {
        Some(Action::PreviewDraw) if shown.is_none() => {
            let canvas = state.bindings.first_label(Action::PreviewCanvas);
            let refused = state.hello_refused.refused(state.selected.host());
            state.preview_says(no_base_line(refused, state.raster.is_shown(), state.raster.node(), &canvas));
        }
        Some(Action::PreviewDraw) => {
            let on = !state.ink.is_on();
            state.ink.set_on(on);
            if !on {
                state.ink.release();
            }
            state.preview_says(if on {
                "drawing on: press and drag on the image; u undo, Delete clear, d off".to_string()
            } else {
                "drawing off".to_string()
            });
        }
        Some(Action::PreviewCanvas) if state.hello_refused.refused(state.selected.host()) => {}
        Some(Action::PreviewCanvas) => {
            let canvas = !state.ink.canvas();
            state.ink.set_canvas(canvas);
            state.preview_says(if canvas {
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
                state.preview_says("drawing cleared: u brings it back".to_string());
            }
        }
        _ => return Continue(()),
    }
    state.last_key = Some(label);
    state.window.request_redraw();
    Break(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAIL: &str = ". Drawing (d) marks up an image file (PNG, JPEG, TIFF and the like) for this row's agent, which sees your marks with your next prompt. To draw anyway, open a PNG of it, or press Shift+D for a blank canvas.";

    #[test]
    fn drawing_over_a_picture_that_is_not_an_image_file_names_the_two_ways_to_draw() {
        for (node, name) in [("files:fig/a.SVG", "this SVG"), ("files:doc.pdf", "this PDF page"), ("files:clip.mp4", "this video frame"),
            ("files:clip.M4V", "this video frame"), ("files:x.bin", "this picture")] {
            assert_eq!(no_base_line(false, true, Some(node), "Shift+D"), format!("Can't draw on {name}{TAIL}"), "{node}");
        }
        assert!(no_base_line(false, true, Some("files:a.svg"), "Ctrl+K").ends_with("press Ctrl+K for a blank canvas."));
    }

    #[test]
    fn the_connection_notice_and_no_picture_have_their_own_words() {
        let notice = "Can't draw while the connection notice covers the preview.";
        assert_eq!(no_base_line(true, true, Some("files:a.png"), "Shift+D"), notice);
        assert_eq!(no_base_line(true, true, Some("files:a.svg"), "Shift+D"), notice);
        let none = "Can't draw here: there is no image file to draw on.";
        assert_eq!(no_base_line(false, true, Some("files:a.png"), "Shift+D"), none);
        assert_eq!(no_base_line(false, true, None, "Shift+D"), none);
        assert_eq!(no_base_line(false, false, Some("files:a.svg"), "Shift+D"), none);
    }
}
