//! The shaped preview: the flowed text buffer the pane draws, its rect, scroll and definition-line anchor, the math
//! SVG beside it, the refused-hello overlay, and the highlighter every markdown shape uses.

use crate::ui::render::quad::Quad;
use crate::ui::*;

/// What the preview pane's flowed text is: one buffer, shaped for the rect the last layout pass gave it. A new
/// installation drops the math SVG and resets the scroll to the top; the anchor scrolls a code preview to the
/// definition a modules row names, once per request.
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
    /// The refused-hello overlay shaped for the active host (ADR 0030 §2, ADR 0042 L2a), rebuilt on set, on resize
    /// and on an active-host switch.
    fatal: Option<MarkdownPreview>,
}

impl ShapedPreview {
    pub(in crate::ui) fn new(
        md: MarkdownPreview, highlight: crate::ui::preview::markdown::highlight::HighlightService, rect: ScreenRect,
    ) -> Self {
        Self { md, svg: None, rect, highlight, anchor_line: None, anchored_to: None, scroll: 0, fatal: None }
    }

    pub(in crate::ui) fn md(&self) -> &MarkdownPreview { &self.md }
    pub(in crate::ui) fn rect(&self) -> ScreenRect { self.rect }
    pub(in crate::ui) fn highlighter(&self) -> &crate::ui::preview::markdown::highlight::HighlightService {
        &self.highlight
    }
    pub(in crate::ui) fn svg(&self) -> Option<&Quad> { self.svg.as_ref() }
    pub(in crate::ui) fn fatal(&self) -> Option<&MarkdownPreview> { self.fatal.as_ref() }
    pub(in crate::ui) fn scroll(&self) -> u16 { self.scroll }

    /// A freshly shaped buffer replaces the shown one: the math SVG goes and the scroll returns to the top.
    pub(in crate::ui) fn install(&mut self, md: MarkdownPreview) {
        self.md = md;
        self.svg = None;
        self.scroll = 0;
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
