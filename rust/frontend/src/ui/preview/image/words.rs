//! A PDF page's words in the window: wire's `PageWords` of one file's page, and what a drag and its marks ask of them,
//! each a pure function of its inputs: the word at a point, the run of words between a drag's two ends, its text, and the
//! bands that mark it. A point is a fraction of the page as drawn (`x / width`, `y / height`, of the canvas rectangle), so
//! a zoom or a pan changes nothing here. A word is named by its index over all lines in order. It never takes `State`.
use crate::ui::render::quad::ScreenRect;
use sot_protocol::ops::PageWords;

/// One page's words, and the file and page they belong to.
#[allow(dead_code, reason = "its caller, the press and drag on a PDF page, lands in the next landing of the plan")]
pub(in crate::ui) struct PageText {
    node_id: String,
    words: PageWords,
    /// The index of each line's first word.
    starts: Vec<usize>,
}

/// A box side, in fractions of the page.
fn frac(v: u16) -> f32 {
    f32::from(v) / 65535.0
}

#[allow(dead_code, reason = "its caller, the press and drag on a PDF page, lands in the next landing of the plan")]
impl PageText {
    pub(in crate::ui) fn new(node_id: String, words: PageWords) -> Self {
        let starts = words.lines.iter().scan(0, |at, line| Some(std::mem::replace(at, *at + line.len()))).collect();
        PageText { node_id, words, starts }
    }

    /// Whether these are the words of `page` of `node_id`.
    pub(in crate::ui) fn belongs_to(&self, node_id: &str, page: u32) -> bool {
        self.node_id == node_id && self.words.page == page
    }

    /// The word at `(x, y)`, fractions of the page (a point past the page's edge is allowed): the first word whose box
    /// holds it, else the first nearest to it, by the distance to the box in units of the page's height (`aspect` is the
    /// page's width over its height). None when the page has no words.
    pub(in crate::ui) fn word_at(&self, (x, y): (f32, f32), aspect: f32) -> Option<usize> {
        let aspect = if aspect.is_finite() && aspect > 0.0 { aspect } else { 1.0 };
        let apart = |near: f32, far: f32, at: f32| (near - at).max(at - far).max(0.0);
        let mut best: Option<(usize, f32)> = None;
        for (i, w) in self.words.lines.iter().flatten().enumerate() {
            let (dx, dy) = (apart(frac(w.left), frac(w.right), x) * aspect, apart(frac(w.top), frac(w.bottom), y));
            let d = dx * dx + dy * dy;
            if best.is_none_or(|(_, b)| d < b) {
                best = Some((i, d));
            }
        }
        best.map(|(i, _)| i)
    }

    /// The words a drag from `anchor` to `pointer` selects: every word from the one at the first point to the one at the
    /// second, in the document's order and whichever way the drag runs, as `(first, last)`, inclusive.
    pub(in crate::ui) fn select(&self, anchor: (f32, f32), pointer: (f32, f32), aspect: f32) -> Option<(usize, usize)> {
        let (a, b) = (self.word_at(anchor, aspect)?, self.word_at(pointer, aspect)?);
        Some((a.min(b), a.max(b)))
    }

    /// The text of a selection (`PageWords::text`, wire's one definition), none for a range outside the page.
    pub(in crate::ui) fn text(&self, (first, last): (usize, usize)) -> Option<String> {
        self.words.text(first, last)
    }

    /// The bands that mark a selection, one for each line it touches: from its first selected word's left to its last's
    /// right, as high as the tallest of them, on `canvas` (the rectangle the whole page is drawn in) and clipped to `clip`
    /// (the image rectangle), so a band the clip leaves nothing of is left out.
    pub(in crate::ui) fn bands(&self, (first, last): (usize, usize), canvas: ScreenRect, clip: ScreenRect) -> Vec<ScreenRect> {
        let mut out = Vec::new();
        for (line, &at) in self.words.lines.iter().zip(&self.starts) {
            let (lo, hi) = (first.max(at), last.min((at + line.len()).saturating_sub(1)));
            if line.is_empty() || lo > hi {
                continue;
            }
            let chosen = &line[lo - at..=hi - at];
            let top = chosen.iter().map(|w| w.top).min().unwrap_or(0);
            let bottom = chosen.iter().map(|w| w.bottom).max().unwrap_or(0);
            let (left, right) = (frac(chosen[0].left), frac(chosen[chosen.len() - 1].right));
            let band = ScreenRect {
                x: canvas.x + left * canvas.w,
                y: canvas.y + frac(top) * canvas.h,
                w: (right - left) * canvas.w,
                h: (frac(bottom) - frac(top)) * canvas.h,
            };
            let (x0, y0) = (band.x.max(clip.x), band.y.max(clip.y));
            let (x1, y1) = ((band.x + band.w).min(clip.x + clip.w), (band.y + band.h).min(clip.y + clip.h));
            if x1 > x0 && y1 > y0 {
                out.push(ScreenRect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 });
            }
        }
        out
    }
}

#[cfg(test)]
#[path = "words_tests.rs"]
mod tests;
