//! Mouse selection over flowed text: a range of buffer line/byte positions, the drag that extends it, the copied bytes
//! and the clipped tint. The owner knows no pane; a pane lends its buffer and geometry as a `FlowedText` per call.

use crate::ui::render::quad::ScreenRect;
use cosmic_text::{Buffer, Cursor};

/// What a pane lends for one call: the shaped buffer and where it sits on screen. `origin_y` is the screen y of the
/// buffer's first line (pane top, padding and scroll already applied); `clip_bottom` is where the renderer stops
/// painting. `reserved` is a placeholder character that stands for non-text media: it is never copied, and a buffer
/// line holding only it is never tinted (a visual line of a wrapped buffer line that holds other text is).
pub(in crate::ui) struct FlowedText<'a> {
    pub buffer: &'a Buffer,
    pub rect: ScreenRect,
    pub origin_y: f32,
    pub clip_bottom: f32,
    pub reserved: Option<char>,
}

/// Buffer positions survive scrolling and resizing; the pane retires them when it replaces the buffer.
#[derive(Default)]
pub(in crate::ui) struct FlowedSelection {
    range: Option<(Cursor, Cursor)>,
    dragging: bool,
}

impl FlowedSelection {
    /// Pointer coordinates follow the glyphs' origin and scroll. A drag clamps to the visible text; a press must be
    /// inside the rect.
    pub(in crate::ui) fn hit(text: &FlowedText, (x, y): (f32, f32), strict: bool) -> Option<Cursor> {
        let r = text.rect;
        if r.w <= 0.0 || r.h <= 0.0 || !x.is_finite() || !y.is_finite() { return None; }
        if strict && (x < r.x || x >= r.x + r.w || y < r.y || y >= r.y + r.h) { return None; }
        let x = x.clamp(r.x, r.x + r.w) - r.x;
        let y = y.clamp(r.y, (text.clip_bottom - 0.01).max(r.y)) - text.origin_y;
        text.buffer.hit(x, y)
    }

    /// A press replaces the old range, including an outside press (`None`), which also ends a previous drag.
    pub(in crate::ui) fn press(&mut self, cursor: Option<Cursor>) -> bool {
        let changed = self.range.is_some() || cursor.is_some();
        self.range = cursor.map(|c| (c, c));
        self.dragging = cursor.is_some();
        changed
    }

    /// Whether a press began a drag that no release has ended; a pane asks before it lends its text.
    pub(in crate::ui) fn dragging(&self) -> bool { self.dragging }

    /// Whether a range is held; a pane asks before it lends its text for the tint.
    pub(in crate::ui) fn is_empty(&self) -> bool { self.range.is_none() }

    /// The drag's end moved to `end` (a `hit` with `strict` false); returns whether it changed.
    pub(in crate::ui) fn extend_drag(&mut self, end: Option<Cursor>) -> bool {
        if !self.dragging { return false; }
        let Some(end) = end else { return false; };
        let Some((start, old_end)) = self.range else { return false; };
        self.range = Some((start, end));
        end != old_end
    }

    pub(in crate::ui) fn release(&mut self) { self.dragging = false; }

    pub(in crate::ui) fn clear(&mut self) {
        self.range = None;
        self.dragging = false;
    }

    fn ordered(&self) -> Option<(Cursor, Cursor)> {
        let (a, b) = self.range?;
        // Affinity distinguishes the two sides of a soft wrap, not selected bytes.
        let a_pos = (a.line, a.index);
        let b_pos = (b.line, b.index);
        if a_pos == b_pos { None } else if a_pos < b_pos { Some((a, b)) } else { Some((b, a)) }
    }

    /// The ordered range as `(line, byte)` pairs, `end` exclusive; `None` when nothing, or an empty range, is held.
    pub(in crate::ui) fn range(&self) -> Option<((usize, usize), (usize, usize))> {
        let (a, b) = self.ordered()?;
        Some(((a.line, a.index), (b.line, b.index)))
    }

    /// The buffer's bytes, joining real buffer lines, never inserting a newline at a soft wrap.
    pub(in crate::ui) fn text(&self, text: &FlowedText) -> Option<String> {
        let (start, end) = self.ordered()?;
        let mut out = String::new();
        for line in start.line..=end.line {
            if line != start.line { out.push('\n'); }
            let source = text.buffer.lines.get(line)?.text();
            let from = if line == start.line { start.index } else { 0 };
            let to = if line == end.line { end.index } else { source.len() };
            let selected = source.get(from..to)?;
            match text.reserved {
                None => out.push_str(selected),
                Some(r) => out.extend(selected.chars().filter(|&c| c != r)),
            }
        }
        (!out.is_empty()).then_some(out)
    }

    /// Pure geometry for the render pass: one tint per selected visual line, clipped to the text's paint bounds.
    pub(in crate::ui) fn rects(&self, text: &FlowedText) -> Vec<ScreenRect> {
        let Some((start, end)) = self.ordered() else { return Vec::new(); };
        let r = text.rect;
        text.buffer.layout_runs().filter_map(|run| {
            if let Some(c) = text.reserved {
                if run.text.trim().chars().eq(std::iter::once(c)) { return None; }
            }
            let (x, w) = run.highlight(start, end)?;
            let left = (r.x + x).max(r.x);
            let right = (r.x + x + w).min(r.x + r.w);
            let top = (text.origin_y + run.line_top).max(r.y);
            let bot = (text.origin_y + run.line_top + run.line_height).min(text.clip_bottom);
            (right > left && bot > top).then_some(ScreenRect { x: left, y: top, w: right - left, h: bot - top })
        }).collect()
    }
}

#[cfg(test)]
#[path = "flowed_selection_tests.rs"]
mod tests;
