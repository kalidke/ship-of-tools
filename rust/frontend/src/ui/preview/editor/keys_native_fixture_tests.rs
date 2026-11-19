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
            s.rebuild_edit_preview();
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
        self.editor.close();
        self.shaped.scroll_to_top(now, length);
        self.redraw()
    }
}
