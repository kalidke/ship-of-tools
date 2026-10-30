//! The shaped preview: the flowed text buffer, rect, scroll, selection and definition-line anchor, the math SVG
//! beside it, the refused-hello overlay, and the highlighter every markdown shape uses.

use crate::ui::render::quad::Quad;
use crate::ui::render::text::EXTRA_TOP_PAD_PX;
use crate::ui::*;
use cosmic_text::Cursor;

/// What the preview pane's flowed text is: one buffer, shaped for the rect the last layout pass gave it. A new
/// installation clears selection, drops the math SVG and resets the scroll to the top; the anchor scrolls a code
/// preview to the definition a modules row names, once per request.
pub(in crate::ui) struct ShapedPreview {
    md: MarkdownPreview,
    /// The math SVG rendered for the shown markdown; `None` until a `math.render` reply rasterizes.
    svg: Option<Quad>,
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
    /// Scroll offset in body lines from the top; clamped at layout to the content's end. Image-only previews
    /// ignore it.
    scroll: u16,
    /// Buffer positions survive scrolling and resizing; a fresh buffer retires them.
    selection: Option<(Cursor, Cursor)>,
    dragging: bool,
    /// The refused-hello overlay shaped for the active host (ADR 0030 §2, ADR 0042 L2a), rebuilt on set, on resize
    /// and on an active-host switch.
    fatal: Option<MarkdownPreview>,
}

impl ShapedPreview {
    pub(in crate::ui) fn new(
        md: MarkdownPreview, highlight: crate::ui::preview::markdown::highlight::HighlightService, rect: ScreenRect,
    ) -> Self {
        Self { md, svg: None, rect, highlight, anchor_line: None, anchored_to: None, scroll: 0,
            selection: None, dragging: false, fatal: None }
    }

    pub(in crate::ui) fn md(&self) -> &MarkdownPreview { &self.md }
    pub(in crate::ui) fn rect(&self) -> ScreenRect { self.rect }
    pub(in crate::ui) fn highlighter(&self) -> &crate::ui::preview::markdown::highlight::HighlightService {
        &self.highlight
    }
    pub(in crate::ui) fn svg(&self) -> Option<&Quad> { self.svg.as_ref() }
    pub(in crate::ui) fn fatal(&self) -> Option<&MarkdownPreview> { self.fatal.as_ref() }
    pub(in crate::ui) fn scroll(&self) -> u16 { self.scroll }

    /// A freshly shaped buffer replaces the shown one: selection and the math SVG go, and scroll returns to the top.
    pub(in crate::ui) fn install(&mut self, md: MarkdownPreview) {
        self.clear_selection();
        self.md = md;
        self.svg = None;
        self.scroll = 0;
    }

    fn scroll_px(&self) -> f32 { self.scroll as f32 * self.md.line_height().max(1.0) }

    /// The text renderer clips at the last whole visual line, after its top padding.
    fn clip_bottom(&self) -> f32 {
        (self.rect.y + EXTRA_TOP_PAD_PX
            + self.md.whole_line_bottom(self.scroll_px(), self.rect.h - EXTRA_TOP_PAD_PX))
            .min(self.rect.y + self.rect.h)
    }

    /// Pointer coordinates follow the same origin, headroom and scroll as the glyphs. A drag clamps to the
    /// visible text; a press must be inside the flowed rect. The separate media buffers are not hit here.
    pub(in crate::ui) fn hit(&self, (x, y): (f32, f32), strict: bool) -> Option<Cursor> {
        let r = self.rect;
        if r.w <= 0.0 || r.h <= 0.0 || !x.is_finite() || !y.is_finite() { return None; }
        if strict && (x < r.x || x >= r.x + r.w || y < r.y || y >= r.y + r.h) { return None; }
        let x = x.clamp(r.x, r.x + r.w) - r.x;
        let y = y.clamp(r.y, (self.clip_bottom() - 0.01).max(r.y)) - r.y - EXTRA_TOP_PAD_PX + self.scroll_px();
        self.md.buffer.hit(x, y)
    }

    /// A press replaces the old range, including an outside press, which also ends a previous drag.
    pub(in crate::ui) fn press(&mut self, cursor: Option<Cursor>) -> bool {
        let changed = self.selection.is_some() || cursor.is_some();
        self.selection = cursor.map(|c| (c, c));
        self.dragging = cursor.is_some();
        changed
    }

    pub(in crate::ui) fn extend_drag(&mut self, at: (f32, f32)) -> bool {
        if !self.dragging { return false; }
        let Some(end) = self.hit(at, false) else { return false; };
        let Some((start, old_end)) = self.selection else { return false; };
        self.selection = Some((start, end));
        end != old_end
    }

    pub(in crate::ui) fn release(&mut self) { self.dragging = false; }

    pub(in crate::ui) fn clear_selection(&mut self) {
        self.selection = None;
        self.dragging = false;
    }

    fn selection_range(&self) -> Option<(Cursor, Cursor)> {
        let (a, b) = self.selection?;
        // Affinity distinguishes the two sides of a soft wrap, not selected bytes.
        let a_pos = (a.line, a.index);
        let b_pos = (b.line, b.index);
        if a_pos == b_pos { None } else if a_pos < b_pos { Some((a, b)) } else { Some((b, a)) }
    }

    /// Copy the rendered buffer's bytes, joining real buffer lines, never inserting a newline at a soft wrap.
    pub(in crate::ui) fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection_range()?;
        let mut text = String::new();
        for line in start.line..=end.line {
            if line != start.line { text.push('\n'); }
            let source = self.md.buffer.lines.get(line)?.text();
            let from = if line == start.line { start.index } else { 0 };
            let to = if line == end.line { end.index } else { source.len() };
            let selected = source.get(from..to)?;
            if self.md.media_blocks.is_empty() {
                text.push_str(selected);
            } else {
                // Embedded images, math and wide tables have no selectable text in this buffer.
                text.extend(selected.chars().filter(|&c| c != crate::ui::preview::markdown::MATH_PLACEHOLDER));
            }
        }
        (!text.is_empty()).then_some(text)
    }

    /// Pure geometry for the render pass: one tint per selected visual line, clipped to the text's paint bounds.
    pub(in crate::ui) fn selection_rects(&self) -> Vec<ScreenRect> {
        let Some((start, end)) = self.selection_range() else { return Vec::new(); };
        let r = self.rect;
        let origin_y = r.y + EXTRA_TOP_PAD_PX - self.scroll_px();
        let bottom = self.clip_bottom();
        self.md.buffer.layout_runs().filter_map(|run| {
            // Standalone media reservations have no selected glyphs; the wide-table buffer paints separately.
            if !self.md.media_blocks.is_empty() && run.text.trim() == "\u{FFFC}" { return None; }
            let (x, w) = run.highlight(start, end)?;
            let left = (r.x + x).max(r.x);
            let right = (r.x + x + w).min(r.x + r.w);
            let top = (origin_y + run.line_top).max(r.y);
            let bot = (origin_y + run.line_top + run.line_height).min(bottom);
            (right > left && bot > top).then_some(ScreenRect { x: left, y: top, w: right - left, h: bot - top })
        }).collect()
    }

    pub(in crate::ui) fn show_svg(&mut self, quad: Quad) { self.svg = Some(quad); }
    pub(in crate::ui) fn clear_svg(&mut self) { self.svg = None; }

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

    /// Move by `delta` body lines, stopping at the top.
    pub(in crate::ui) fn scroll_by(&mut self, delta: i32) {
        self.scroll = (self.scroll as i32 + delta).max(0) as u16;
    }
    pub(in crate::ui) fn scroll_up(&mut self, rows: u16) { self.scroll = self.scroll.saturating_sub(rows); }
    pub(in crate::ui) fn scroll_down(&mut self, rows: u16) { self.scroll = self.scroll.saturating_add(rows); }
    pub(in crate::ui) fn scroll_to_top(&mut self) { self.scroll = 0; }
    /// The far end; the next layout clamps it to (total - visible).
    pub(in crate::ui) fn scroll_to_end(&mut self) { self.scroll = u16::MAX; }
    pub(in crate::ui) fn clamp_scroll(&mut self, max_scroll: u16) { self.scroll = self.scroll.min(max_scroll); }

    /// A `preview.get` was sent: its reply anchors to `line`, if any.
    pub(in crate::ui) fn arm_anchor(&mut self, line: Option<u32>) { self.anchor_line = line; }
    pub(in crate::ui) fn clear_anchor(&mut self) { self.anchor_line = None; }

    /// The cursor moved within the shown file: on a real change of target line, a code preview scrolls to the new
    /// definition. Returns whether it scrolled.
    pub(in crate::ui) fn reanchor(&mut self, line: Option<u32>, is_code: bool) -> bool {
        if line == self.anchored_to {
            return false;
        }
        let mut scrolled = false;
        if let Some(l) = line {
            if l > 0 && is_code {
                self.scroll = self.md.anchor_scroll_for_def_line(l as usize);
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
                self.scroll = self.md.anchor_scroll_for_def_line(def_line as usize);
                self.anchored_to = Some(def_line);
            }
            _ => self.anchored_to = None,
        }
    }
}

#[cfg(test)]
#[path = "shaped_tests.rs"]
mod tests;
