use super::*;
use crate::ui::render::text::EXTRA_TOP_PAD_PX;
use anyhow::ensure;

impl State {
    /// The edit view follows its cursor in the window's own layout, where the editor's lines and the preview's differ
    /// in height: a cursor put far below the pane scrolls the view to show its row and one put back at the top
    /// scrolls it back, a cursor in view moves nothing, and a wheel scroll away is undone by the next follow. The
    /// keys cannot be pressed here (a winit key event cannot be built), so the cursor is put and the key path's own
    /// two calls follow, `rebuild_edit_preview` then `follow_edit_cursor`.
    #[allow(dead_code, reason = "runs only inside a native State, from the window_minimized fixtures child; no libtest body can construct one")]
    pub(in crate::ui) fn an_edit_view_follows_its_cursor(&mut self) -> anyhow::Result<()> {
        // The default easing: a follow still lands at once, since a held key's row would stay below the pane for
        // the frames of an eased one.
        self.settings.smooth_scroll_ms = 150;
        let content = (0..300).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n");
        self.editor.arm_file("files:long.txt".into());
        self.editor.open_file("files:long.txt", true, content.clone(), "v".into());
        self.rebuild_edit_preview();
        self.redraw()?;
        let shown = |s: &State| -> anyhow::Result<bool> {
            let place = s.editor.cursor_place().ok_or_else(|| anyhow::anyhow!("the cursor's row is not in the layout"))?;
            let from = s.shaped.scroll_px();
            Ok(place.top >= from && place.top + place.height <= from + s.concept.rect().h - EXTRA_TOP_PAD_PX)
        };
        let mut moved = |s: &mut State, at: usize| -> anyhow::Result<()> {
            s.editor.put_cursor(at);
            // As the key path does: the shaped text is made again only when it is owed.
            if s.editor.needs_reshape() {
                s.rebuild_edit_preview();
            }
            s.follow_edit_cursor();
            s.redraw()
        };
        ensure!(self.shaped.scroll() == 0.0 && shown(self)?, "the edit does not open at its top: {}", self.shaped.scroll());
        self.editor.put_cursor(content.len());
        self.rebuild_edit_preview();
        self.follow_edit_cursor();
        ensure!(self.shaped.scroll() > 0.0, "an eased follow waited for frames: scroll {}", self.shaped.scroll());
        self.settings.smooth_scroll_ms = 0;
        moved(self, content.len())?;
        ensure!(shown(self)? && self.shaped.scroll() > 0.0, "a cursor at the end is out of view: scroll {}", self.shaped.scroll());
        moved(self, 0)?;
        ensure!(shown(self)? && self.shaped.scroll() == 0.0, "a cursor at the start did not scroll back: {}", self.shaped.scroll());
        moved(self, 12)?;
        ensure!(self.shaped.scroll() == 0.0, "a cursor in view scrolled the view: {}", self.shaped.scroll());
        let (now, length) = self.scroll_ask();
        self.shaped.scroll_by(60, now, length);
        self.redraw()?;
        ensure!(self.shaped.scroll() == 60.0 && !shown(self)?, "the wheel did not take the view from the cursor: {}", self.shaped.scroll());
        moved(self, 13)?;
        ensure!(shown(self)?, "the next motion did not bring the view back");
        // A selection over three lines is drawn as bands in the frame, inside the pane, and its growth shapes nothing.
        self.editor.select_for_test(13, 30);
        self.reshape_edit_if_owed();
        self.redraw()?;
        let (pane, scroll) = (self.concept.rect(), self.shaped.scroll_px());
        let bands = self.edit_selection_rects(pane, scroll);
        ensure!(bands.len() >= 3 && bands.iter().all(|b| b.x >= pane.x && b.x + b.w <= pane.x + pane.w && b.y >= pane.y.floor()),
            "the selection's bands are not on its rows in the pane: {bands:?} in {pane:?}");
        self.editor.select_for_test(13, 40);
        ensure!(!self.editor.needs_reshape(), "a selection that grows owes a shaping");
        self.redraw()?;
        moved(self, 13)?;
        // A resize: the frame shapes the edit buffer for the pane's new width before it draws, so the rows a key reads
        // are the rows the screen shows.
        let width = |s: &State| s.editor.shaped().and_then(|shaped| shaped.buffer.size().0);
        let before = width(self);
        let size = self.window.inner_size();
        self.canvas.resize(self.gpu.queue(), size.width - 160, size.height - 80);
        self.redraw()?;
        ensure!(width(self) != before && width(self) == Some(self.concept.rect().w.max(1.0)),
            "the edit buffer kept its old width {before:?} in a pane of {}", self.concept.rect().w);
        self.canvas.resize(self.gpu.queue(), size.width, size.height);
        self.redraw()?;
        ensure!(width(self) == before, "the edit buffer did not follow the pane back: {:?}", width(self));
        self.editor.close();
        self.shaped.scroll_to_top(now, length);
        self.redraw()
    }

    /// What the footer of the edit says follows what the edit is, through the window's own handlers: Escape on an edited
    /// buffer draws the discard prompt and any other key takes it down, a landed save says clean, and a text changed
    /// while the pane has no width (a full-height drawer) is not shaped to a pixel but when the width returns. The keys
    /// are pressed at the editor (a winit key event cannot be built) and their outcomes handed to the window's own
    /// `apply_editor_outcome`, the one place a key's re-shape is decided.
    #[allow(dead_code, reason = "runs only inside a native State, from the window_minimized fixtures child; no libtest body can construct one")]
    pub(in crate::ui) fn an_edit_footer_says_what_the_edit_is(&mut self) -> anyhow::Result<()> {
        use crate::ui::input::keybindings::Action;
        self.settings.smooth_scroll_ms = 0;
        self.editor.arm_file("files:foot.txt".into());
        self.editor.open_file("files:foot.txt", true, "alpha\nbeta".into(), "v".into());
        self.rebuild_edit_preview();
        self.redraw()?;
        let footer = |s: &State| s.editor.shaped().and_then(|sh| sh.buffer.lines.last().map(|l| l.text().to_string())).unwrap_or_default();
        let mut press = |s: &mut State, action: Option<Action>, typed: Option<&str>| -> anyhow::Result<()> {
            let outcome = s.editor.press_for_test(action, typed, 3).ok_or_else(|| anyhow::anyhow!("the editor took no key"))?;
            super::apply_editor_outcome(s, outcome);
            s.redraw()
        };
        ensure!(footer(self).contains("clean"), "a fresh edit is not clean: {}", footer(self));
        press(self, None, Some("x"))?;
        ensure!(footer(self).contains("modified"), "a typed edit is not modified: {}", footer(self));
        press(self, Some(Action::Cancel), None)?;
        ensure!(footer(self).contains("DISCARD UNSAVED EDITS?"), "Escape drew no prompt: {}", footer(self));
        press(self, Some(Action::EditRight), None)?;
        ensure!(footer(self).contains("modified") && !footer(self).contains("DISCARD"), "the prompt stayed up: {}", footer(self));
        self.on_file_write_done("files:foot.txt".into(), crate::net::transport::FileWriteResult::Ok { path: "foot.txt".into(), version: "v2".into() });
        self.redraw()?;
        ensure!(footer(self).contains("clean"), "a landed save did not say clean: {}", footer(self));
        // A pane with no width: the text is not shaped to a pixel, and is shaped when the width returns.
        let rect = self.concept.rect();
        let canvas = &mut self.canvas;
        let concept = &mut self.concept;
        canvas.shape(|fs| concept.layout(ScreenRect { w: 0.0, ..rect }, fs));
        let before = self.editor.shaped().and_then(|sh| sh.buffer.size().0);
        let outcome = self.editor.press_for_test(None, Some("y"), 3).ok_or_else(|| anyhow::anyhow!("the editor took no key"))?;
        super::apply_editor_outcome(self, outcome);
        // A reply, a row switch's restore and the like re-shape unasked: not to a pixel either.
        self.rebuild_edit_preview();
        ensure!(self.editor.needs_reshape(), "a typed edit in a hidden pane is not owed");
        ensure!(self.editor.shaped().and_then(|sh| sh.buffer.size().0) == before, "a pane with no width was shaped for");
        self.redraw()?;
        ensure!(!self.editor.needs_reshape() && self.editor.cursor_place().is_some() && footer(self).contains("modified"),
            "the width's return did not shape the text: {}", footer(self));
        // A row switched to under a full-height drawer brings its edit back with no shaped text; the frame that gives
        // the width back must shape it before it decides what the pane shows, or it draws the read-only file and asks
        // for no other.
        let parked = self.editor.park();
        self.editor.close();
        self.editor.restore(parked);
        let concept = &mut self.concept;
        self.canvas.shape(|fs| concept.layout(ScreenRect { w: 0.0, ..rect }, fs));
        self.rebuild_edit_preview();
        ensure!(self.editor.is_editing() && self.editor.shaped().is_none(), "the restored edit was shaped to a pane with no width");
        let shows = self.frame_shows(rect);
        ensure!(shows.edit && !shows.md, "the first frame at the pane's width shows the read-only file: {shows:?}");
        self.editor.close();
        self.redraw()
    }
}
