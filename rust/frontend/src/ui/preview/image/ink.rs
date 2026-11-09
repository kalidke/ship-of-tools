//! The row's drawing in the window: its base and committed strokes, the live stroke, drawing mode and the `revision`
//! every committed change raises. It maps a screen point through the canvas and image rectangles each call hands it, so
//! zoom, pan and a resize need no state here. It never takes `State`. The mask and the GPU overlay are D4b's.
// D4c is the first reader.
#![cfg_attr(not(test), allow(dead_code))]

use crate::ui::render::quad::ScreenRect;
use sot_protocol::ink::to_raster;
use sot_protocol::ops::{InkBase, MAX_INK_POINTS};

type Stroke = Vec<[u16; 2]>;

/// A step undo takes back: a committed stroke, or the strokes one erase drag removed (with the index each had).
enum Edit {
    Added,
    Erased(Vec<(usize, Stroke)>),
}

#[derive(Default)]
pub(in crate::ui) struct InkLayer {
    base: Option<InkBase>,
    strokes: Vec<Stroke>,
    cleared: Vec<Stroke>,
    history: Vec<Edit>,
    live: Option<Stroke>,
    erasing: Option<Vec<(usize, Stroke)>>,
    on: bool,
    revision: u64,
}

fn inside(r: &ScreenRect, p: (f32, f32)) -> bool {
    p.0 >= r.x && p.0 <= r.x + r.w && p.1 >= r.y && p.1 <= r.y + r.h
}

/// The wire point of screen position `p` on the canvas rectangle, its fractions clamped to the base.
fn map(p: (f32, f32), canvas: &ScreenRect) -> Option<[u16; 2]> {
    if canvas.w <= 0.0 || canvas.h <= 0.0 {
        return None;
    }
    let f = |v: f32| (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
    Some([f((p.0 - canvas.x) / canvas.w), f((p.1 - canvas.y) / canvas.h)])
}

fn seg_dist(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    ((p.0 - a.0 - t * dx).powi(2) + (p.1 - a.1 - t * dy).powi(2)).sqrt()
}

fn name(base: &InkBase) -> &str {
    match base {
        InkBase::Canvas => "the canvas",
        InkBase::Figure { node_id } => node_id.rsplit(['/', ':']).next().unwrap_or(node_id),
    }
}

impl InkLayer {
    pub(in crate::ui) fn base(&self) -> Option<&InkBase> {
        self.base.as_ref()
    }
    pub(in crate::ui) fn strokes(&self) -> &[Stroke] {
        &self.strokes
    }
    pub(in crate::ui) fn live(&self) -> Option<&[[u16; 2]]> {
        self.live.as_deref()
    }
    pub(in crate::ui) fn revision(&self) -> u64 {
        self.revision
    }
    pub(in crate::ui) fn is_on(&self) -> bool {
        self.on
    }
    pub(in crate::ui) fn set_on(&mut self, on: bool) {
        self.on = on;
    }

    fn points(&self) -> usize {
        self.strokes.iter().chain(self.live.iter()).map(Vec::len).sum()
    }

    /// Whether a press at `p` reaches `shown`, the base on show (`None`: none): drawing is on, or `shown` is the canvas,
    /// and `p` is inside the image rectangle and the canvas rectangle.
    fn reaches(&self, shown: Option<&InkBase>, p: (f32, f32), canvas: &ScreenRect, image: &ScreenRect) -> bool {
        shown.is_some_and(|b| (self.on || *b == InkBase::Canvas) && inside(image, p) && inside(canvas, p))
    }

    /// A left press: ends a stroke whose release was lost, then begins one. Another base replaces the row's drawing, and
    /// the text for the status line comes back.
    pub(in crate::ui) fn press(
        &mut self, shown: Option<&InkBase>, p: (f32, f32), canvas: &ScreenRect, image: &ScreenRect,
    ) -> Option<String> {
        if !self.reaches(shown, p, canvas, image) {
            return None;
        }
        let shown = shown?;
        self.release();
        self.erase_end();
        let mut note = None;
        if self.base.as_ref() != Some(shown) {
            if !self.strokes.is_empty() {
                note = self.base.as_ref().map(|b| format!("the drawing on {} was replaced", name(b)));
                self.revision += 1;
            }
            self.strokes.clear();
            self.history.clear();
            self.base = Some(shown.clone());
        }
        self.cleared.clear();
        self.live = map(p, canvas).map(|q| vec![q]);
        note
    }

    /// Motion with a left stroke live: a point is kept one raster pixel from the last, and dropped past the bound (the
    /// status text comes back).
    pub(in crate::ui) fn drag(&mut self, p: (f32, f32), canvas: &ScreenRect, dims: (u32, u32)) -> Option<&'static str> {
        let (Some(q), Some(last)) = (map(p, canvas), self.live.as_ref().and_then(|s| s.last().copied())) else {
            return None;
        };
        let (a, b) = (to_raster(last, dims), to_raster(q, dims));
        if (a.0 - b.0).hypot(a.1 - b.1) < 1.0 {
            return None;
        }
        if self.points() >= MAX_INK_POINTS {
            return Some("the drawing is full: undo or clear");
        }
        self.live.as_mut()?.push(q);
        None
    }

    /// A release or a lost focus commits the live stroke.
    pub(in crate::ui) fn release(&mut self) {
        if let Some(s) = self.live.take() {
            self.strokes.push(s);
            self.history.push(Edit::Added);
            self.revision += 1;
        }
    }

    /// The base stops being shown: a live stroke is dropped, not committed.
    pub(in crate::ui) fn base_hidden(&mut self) {
        self.live = None;
        self.erase_end();
    }

    /// Undo drops a live stroke and nothing else; otherwise it takes back the last step, or brings back what a clear moved.
    pub(in crate::ui) fn undo(&mut self) {
        if self.live.take().is_some() {
            return;
        }
        match self.history.pop() {
            Some(Edit::Added) => drop(self.strokes.pop()),
            Some(Edit::Erased(gone)) => gone.into_iter().rev().for_each(|(i, s)| self.strokes.insert(i, s)),
            None if self.strokes.is_empty() && !self.cleared.is_empty() => {
                self.strokes = std::mem::take(&mut self.cleared);
                self.history = self.strokes.iter().map(|_| Edit::Added).collect();
            }
            None => return,
        }
        self.revision += 1;
    }

    /// Clear drops a live stroke and nothing else; otherwise the strokes move to the `cleared` copy.
    pub(in crate::ui) fn clear(&mut self) {
        if self.live.take().is_some() || self.strokes.is_empty() {
            return;
        }
        self.cleared = std::mem::take(&mut self.strokes);
        self.history.clear();
        self.revision += 1;
    }

    /// A right press (and each right motion): removes each committed stroke within one raster pixel of `p`. Ignored while
    /// a left stroke is live. The strokes one drag removes are one undo step, taken at `erase_end`.
    pub(in crate::ui) fn erase(
        &mut self, shown: Option<&InkBase>, p: (f32, f32), canvas: &ScreenRect, image: &ScreenRect, dims: (u32, u32),
    ) {
        if self.live.is_some() || shown != self.base.as_ref() {
            return;
        }
        if self.erasing.is_none() && !self.reaches(shown, p, canvas, image) {
            return;
        }
        let Some(q) = map(p, canvas) else { return };
        let (at, gone) = (to_raster(q, dims), self.erasing.get_or_insert_with(Vec::new));
        let hit = |s: &Stroke| match s.len() {
            1 => seg_dist(at, to_raster(s[0], dims), to_raster(s[0], dims)) <= 1.0,
            _ => s.windows(2).any(|w| seg_dist(at, to_raster(w[0], dims), to_raster(w[1], dims)) <= 1.0),
        };
        let mut i = 0;
        while i < self.strokes.len() {
            if hit(&self.strokes[i]) {
                gone.push((i, self.strokes.remove(i)));
                self.revision += 1;
            } else {
                i += 1;
            }
        }
    }

    pub(in crate::ui) fn erase_end(&mut self) {
        if let Some(gone) = self.erasing.take().filter(|g| !g.is_empty()) {
            self.history.push(Edit::Erased(gone));
        }
    }
}

#[cfg(test)]
#[path = "ink_tests.rs"]
mod ink_tests;
