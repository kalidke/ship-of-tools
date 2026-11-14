//! The shaped preview: the flowed text buffer, rect, scroll, selection and definition-line anchor, the refused-hello
//! overlay, and the highlighter every markdown shape uses.

use crate::ui::flowed_selection::{FlowedSelection, FlowedText};
use crate::ui::motion::ScrollMotion;
use crate::ui::render::text::EXTRA_TOP_PAD_PX;
use crate::ui::*;
use crate::ui::preview::markdown::TextOrigin;
use cosmic_text::Cursor;
use std::time::{Duration, Instant};

/// What the preview pane's flowed text is: one buffer, shaped for the rect the last layout pass gave it. A new
/// installation clears selection and resets the scroll to the top; the anchor scrolls a code
/// preview to the definition a modules row names, once per request.
pub(in crate::ui) struct ShapedPreview {
    md: MarkdownPreview,
    /// Pixel rect of the markdown pane from the most recent layout pass, so a resize re-shapes only on a real change.
    rect: ScreenRect,
    /// Tree-sitter-backed syntax highlighter for fenced code blocks. Constructed once, since
    /// `HighlightConfiguration::new` compiles the per-language highlight query.
    highlight: crate::ui::preview::markdown::highlight::HighlightService,
    /// Definition line (1-indexed) the in-flight `preview.get` should anchor to once it lands, from the selected
    /// modules-mode row's `line` payload. `None` for files-mode rows. Consumed once by the reply.
    anchor_line: Option<u32>,
    /// The definition line the shown code preview is anchored to, so a cursor move between items of the same file
    /// re-anchors without a re-fetch, and only on a real change (never fighting a manual scroll).
    anchored_to: Option<u32>,
    /// Scroll offset in body lines from the top, whole at rest and fractional while a person's scroll eases;
    /// clamped at layout to the content's end. Image-only previews ignore it.
    scroll: f32,
    /// What a person's scroll still owes `scroll`, spent at each layout.
    motion: ScrollMotion,
    /// An End asked and not yet resolved: its distance is known only against the layout's end.
    end_asked: Option<(Instant, Duration)>,
    /// Whether the last layout drew the editor, whose buffer is not the one an owed distance was asked on.
    edit_shown: bool,
    /// Buffer positions survive scrolling and resizing; a fresh buffer retires them.
    selection: FlowedSelection,
    /// The refused-hello overlay shaped for the active host (ADR 0030 §2, ADR 0042 L2a), rebuilt on set, on resize
    /// and on an active-host switch.
    fatal: Option<MarkdownPreview>,
}

impl ShapedPreview {
    pub(in crate::ui) fn new(
        md: MarkdownPreview, highlight: crate::ui::preview::markdown::highlight::HighlightService, rect: ScreenRect,
    ) -> Self {
        Self { md, rect, highlight, anchor_line: None, anchored_to: None, scroll: 0.0,
            motion: ScrollMotion::default(), end_asked: None, edit_shown: false,
            selection: FlowedSelection::default(), fatal: None }
    }

    pub(in crate::ui) fn md(&self) -> &MarkdownPreview { &self.md }
    pub(in crate::ui) fn rect(&self) -> ScreenRect { self.rect }
    pub(in crate::ui) fn highlighter(&self) -> &crate::ui::preview::markdown::highlight::HighlightService {
        &self.highlight
    }
    pub(in crate::ui) fn fatal(&self) -> Option<&MarkdownPreview> { self.fatal.as_ref() }
    /// The position in body lines, for the tests; the drawn value is `scroll_px`.
    #[cfg(test)]
    pub(in crate::ui) fn scroll(&self) -> f32 { self.scroll }

    /// A freshly shaped buffer replaces the shown one: selection goes, and scroll returns to the top.
    pub(in crate::ui) fn install(&mut self, md: MarkdownPreview) {
        self.clear_selection();
        self.md = md;
        self.moves_at_once(0.0);
    }

    /// The drawn offset in pixels: the position times the body line, on a whole physical pixel while it is between
    /// lines, so text, code panels, media and the hit test all move together.
    pub(in crate::ui) fn scroll_px(&self) -> f32 {
        let px = self.scroll * self.md.line_height().max(1.0);
        if self.scroll.fract() == 0.0 { px } else { px.round() }
    }

    /// The text renderer clips at the last whole visual line, after its top padding.
    fn clip_bottom(&self) -> f32 {
        (self.rect.y + EXTRA_TOP_PAD_PX
            + self.md.whole_line_bottom(self.scroll_px(), self.rect.h - EXTRA_TOP_PAD_PX))
            .min(self.rect.y + self.rect.h)
    }

    /// The buffer and where it paints, lent to the selection owner. Media reservations hold no selectable text.
    fn flowed(&self) -> FlowedText<'_> {
        FlowedText {
            buffer: &self.md.buffer,
            rect: self.rect,
            origin_y: self.rect.y + EXTRA_TOP_PAD_PX - self.scroll_px(),
            clip_bottom: self.clip_bottom(),
            reserved: (!self.md.media_blocks.is_empty()).then_some(crate::ui::preview::markdown::MATH_PLACEHOLDER),
            shown: None,
        }
    }

    /// A drag clamps to the visible text; a press must be inside the flowed rect. The separate media buffers are not
    /// hit here.
    pub(in crate::ui) fn hit(&self, at: (f32, f32), strict: bool) -> Option<Cursor> {
        FlowedSelection::hit(&self.flowed(), at, strict)
    }

    /// A press replaces the old range, including an outside press, which also ends a previous drag.
    pub(in crate::ui) fn press(&mut self, cursor: Option<Cursor>) -> bool { self.selection.press(cursor) }

    pub(in crate::ui) fn extend_drag(&mut self, at: (f32, f32)) -> bool {
        if !self.selection.dragging() { return false; }
        let end = self.hit(at, false);
        self.selection.extend_drag(end)
    }

    pub(in crate::ui) fn release(&mut self) { self.selection.release(); }

    pub(in crate::ui) fn clear_selection(&mut self) { self.selection.clear(); }

    pub(in crate::ui) fn selected_text(&self) -> Option<String> { self.selection.text(&self.flowed()) }

    /// The released, non-empty selection as the file's text: a plain or token buffer's own `(line, byte)` range and the
    /// text it covers, rendered markdown's source lines whole with the file's text there, nothing for a rendered
    /// buffer with no map. Nothing during a drag.
    pub(in crate::ui) fn released_selection(&self) -> Option<(((usize, usize), (usize, usize)), String)> {
        if self.selection.dragging() { return None; }
        let range = self.selection.range()?;
        match self.md.origin() {
            TextOrigin::File => Some((range, self.selected_text()?)),
            TextOrigin::Rendered(map) => map.file_selection(range),
            TextOrigin::Unshared => None,
        }
    }

    pub(in crate::ui) fn selection_rects(&self) -> Vec<ScreenRect> {
        if self.selection.is_empty() { return Vec::new(); }
        self.selection.rects(&self.flowed())
    }

    pub(in crate::ui) fn show_fatal(&mut self, fatal: Option<MarkdownPreview>) { self.fatal = fatal; }
    pub(in crate::ui) fn clear_fatal(&mut self) { self.fatal = None; }

    /// Julia fences the last shape left untokenized, taken once for `markdown.tokenize`.
    pub(in crate::ui) fn take_pending_token_fences(&mut self) -> Vec<(String, u64, String)> {
        std::mem::take(&mut self.md.pending_token_fences)
    }

    /// The layout pass's rect: the buffer is resized only when the rect changed by more than half a pixel.
    pub(in crate::ui) fn layout(&mut self, rect: ScreenRect, font_system: &mut glyphon::FontSystem) {
        let size_changed = (rect.w - self.rect.w).abs() > 0.5 || (rect.h - self.rect.h).abs() > 0.5;
        if size_changed {
            self.md.resize(font_system, rect.w, rect.h);
        }
        self.rect = rect;
    }

    /// A person asked to move by `delta` body lines at `now`: they arrive over `length`, or at once when it is zero.
    /// The top stops it.
    pub(in crate::ui) fn scroll_by(&mut self, delta: i32, now: Instant, length: Duration) {
        self.ask(delta as f32, now, length);
    }
    fn ask(&mut self, delta: f32, now: Instant, length: Duration) {
        let at_once = self.motion.ask(delta, now, length);
        self.scroll = (self.scroll + at_once).max(0.0);
    }
    /// A person asked for the top; the distance is what lies between the asked position and it.
    pub(in crate::ui) fn scroll_to_top(&mut self, now: Instant, length: Duration) { self.scroll_to(0.0, now, length); }
    /// A person asked for the far end; the next layout knows where it is.
    pub(in crate::ui) fn scroll_to_end(&mut self, now: Instant, length: Duration) {
        if length.is_zero() {
            self.moves_at_once(u16::MAX as f32);
        } else {
            self.end_asked = Some((now, length));
        }
    }

    /// A person's ask for the whole distance to the whole line `target`, counting what is already owed.
    fn scroll_to(&mut self, target: f32, now: Instant, length: Duration) {
        // The last ask wins: an End not yet resolved is this ask's to replace.
        self.end_asked = None;
        self.ask(target - self.scroll - self.motion.owed(), now, length);
    }

    /// Another rule sets the position (a new file, a reply's anchor, an end the layout will clamp): nothing is owed.
    fn moves_at_once(&mut self, to: f32) {
        self.motion.stop();
        self.end_asked = None;
        self.scroll = to;
    }

    /// One layout: an End asked resolves against `max_scroll`, a frame at `now` spends what is owed (at most
    /// `visible` lines of it eased), the end and the top stop it, and a rest lands on a whole line. Returns whether
    /// frames must keep coming. The editor's buffer and the preview's are not the same ground, so a change of which
    /// is drawn ends what is owed.
    pub(in crate::ui) fn advance(&mut self, now: Instant, max_scroll: u16, visible: f32, show_edit: bool) -> bool {
        if self.edit_shown != show_edit {
            self.edit_shown = show_edit;
            self.moves_at_once(self.scroll);
        }
        let max = max_scroll as f32;
        if let Some((at, length)) = self.end_asked.take() {
            self.scroll_to(max, at, length);
        }
        let next = self.scroll + self.motion.spend(now, visible);
        if next > max || next < 0.0 {
            self.motion.stop();
        }
        self.scroll = next.min(max).max(0.0);
        if !self.motion.moving() {
            self.scroll = self.scroll.round();
        }
        self.motion.moving()
    }

    /// A `preview.get` was sent: its reply anchors to `line`, if any.
    pub(in crate::ui) fn arm_anchor(&mut self, line: Option<u32>) { self.anchor_line = line; }
    pub(in crate::ui) fn clear_anchor(&mut self) { self.anchor_line = None; }

    /// The cursor moved within the shown file: on a real change of target line, a code preview scrolls to the new
    /// definition. Returns whether it scrolled.
    pub(in crate::ui) fn reanchor(&mut self, line: Option<u32>, is_code: bool, now: Instant, length: Duration) -> bool {
        if line == self.anchored_to {
            return false;
        }
        let mut scrolled = false;
        if let Some(l) = line {
            if l > 0 && is_code {
                self.scroll_to(self.md.anchor_scroll_for_def_line(l as usize) as f32, now, length);
                scrolled = true;
            }
        }
        self.anchored_to = line;
        scrolled
    }

    /// A preview reply landed: its armed anchor is consumed, and scrolls to the definition only when `applies`
    /// (a code reply for the request that armed it, with a real line).
    pub(in crate::ui) fn settle_anchor(&mut self, applies: bool) {
        match self.anchor_line.take() {
            Some(def_line) if def_line > 0 && applies => {
                self.moves_at_once(self.md.anchor_scroll_for_def_line(def_line as usize) as f32);
                self.anchored_to = Some(def_line);
            }
            _ => self.anchored_to = None,
        }
    }
}

#[cfg(test)]
#[path = "shaped_tests.rs"]
mod tests;
