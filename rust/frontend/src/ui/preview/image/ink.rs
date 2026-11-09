//! The row's drawing in the window: its base and committed strokes, the live stroke, drawing mode and the `revision`
//! every committed change raises. It maps a screen point through the canvas and image rectangles each call hands it, so
//! zoom, pan and a resize need no state here. It keeps the drawing's raster (the wire's own functions paint it) and the
//! overlay texture made from it. It never takes `State`.
use crate::ui::render::quad::{Quad, QuadPipeline, ScreenRect};
use sot_protocol::ink::{ink_radius, mask, paint_segment, raster_dims, segments, to_raster, INK_RGB};
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
    /// An erase drag: whether its step is in `history` yet (it is at the first hit), and its last point inside the image
    /// (none after an ignored point, so the path never joins two points across a stretch it was not tested on).
    erasing: Option<(bool, Option<[u16; 2]>)>,
    on: bool,
    revision: u64,
    /// The raster's size (`raster_dims` of the base's source size, taken at a press), its coverage bytes, and the
    /// rectangle `[x0, y0, x1, y1)` changed since the last upload.
    dims: (u32, u32),
    mask: Vec<u8>,
    dirty: Option<[u32; 4]>,
    overlay: Option<Quad>,
    /// The last upload failed and was logged: a repeat is not logged again.
    upload_failed: bool,
    /// The last frame's canvas and image rectangles, which the pointer maps through.
    frame: Option<(ScreenRect, ScreenRect)>,
    /// The right button is down: every motion of the drag goes to the eraser.
    right_down: bool,
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

/// The distance between segments `a`-`b` and `c`-`d`: 0 when they cross.
fn segs_dist(a: (f32, f32), b: (f32, f32), c: (f32, f32), d: (f32, f32)) -> f32 {
    let side = |p: (f32, f32), q: (f32, f32), r: (f32, f32)| (q.0 - p.0) * (r.1 - p.1) - (q.1 - p.1) * (r.0 - p.0);
    if side(a, b, c) * side(a, b, d) < 0.0 && side(c, d, a) * side(c, d, b) < 0.0 {
        return 0.0;
    }
    seg_dist(a, c, d).min(seg_dist(b, c, d)).min(seg_dist(c, a, b)).min(seg_dist(d, a, b))
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
    /// Read by the share (D6) and the per-row parking (D4c2); the tests read it now.
    #[cfg_attr(not(test), allow(dead_code))]
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
    pub(in crate::ui) fn frame(&self) -> Option<(ScreenRect, ScreenRect)> {
        self.frame
    }
    pub(in crate::ui) fn set_frame(&mut self, canvas: ScreenRect, image: ScreenRect) {
        self.frame = Some((canvas, image));
    }
    pub(in crate::ui) fn right_down(&self) -> bool {
        self.right_down
    }
    pub(in crate::ui) fn set_right_down(&mut self, down: bool) {
        self.right_down = down;
    }

    /// Run before each frame and pointer event: a base no longer shown drops a live stroke, and a shown base whose source
    /// size changed (the same node installed again) rebuilds the raster at once.
    pub(in crate::ui) fn sync(&mut self, shown: Option<&InkBase>, src: Option<(u32, u32)>) {
        if shown != self.base.as_ref() {
            self.base_hidden();
        } else if let (Some(_), Some((w, h))) = (shown, src) {
            if self.dims != raster_dims(w, h) {
                self.live = None;
                self.dims = raster_dims(w, h);
                self.repaint();
            }
        }
    }

    /// The raster is painted again from the committed strokes, and all of it is dirty.
    fn repaint(&mut self) {
        self.mask = mask(&self.strokes, self.dims);
        self.dirty = Some([0, 0, self.dims.0, self.dims.1]);
    }

    /// One segment (raster positions) is painted over the mask, and its box joins the dirty rectangle.
    fn paint(&mut self, a: (f32, f32), b: (f32, f32)) {
        let r = ink_radius(self.dims);
        paint_segment(&mut self.mask, self.dims, a, b, r);
        let lo = |v: f32, max: u32| ((v - r - 2.0).floor().max(0.0) as u32).min(max);
        let hi = |v: f32, max: u32| ((v + r + 2.0).ceil().max(0.0) as u32).min(max);
        let (x0, y0) = (lo(a.0.min(b.0), self.dims.0), lo(a.1.min(b.1), self.dims.1));
        let (x1, y1) = (hi(a.0.max(b.0), self.dims.0), hi(a.1.max(b.1), self.dims.1));
        self.dirty = Some(self.dirty.map_or([x0, y0, x1, y1], |d| [d[0].min(x0), d[1].min(y0), d[2].max(x1), d[3].max(y1)]));
    }

    /// The changed rectangle `(x, y, w, h)` since the last upload.
    fn dirty(&self) -> Option<(u32, u32, u32, u32)> {
        self.dirty.map(|[x0, y0, x1, y1]| (x0, y0, x1 - x0, y1 - y0))
    }

    /// The mask's `w` by `h` rectangle at `(x, y)` as straight-alpha RGBA in the ink color.
    fn rgba(&self, (x, y, w, h): (u32, u32, u32, u32)) -> Vec<u8> {
        let mut out = vec![0; (w * h * 4) as usize];
        for (row, dst) in (y..y + h).zip(out.chunks_exact_mut(w as usize * 4)) {
            let start = (row * self.dims.0 + x) as usize;
            for (px, &a) in dst.chunks_exact_mut(4).zip(&self.mask[start..start + w as usize]) {
                px.copy_from_slice(&[INK_RGB[0], INK_RGB[1], INK_RGB[2], a]);
            }
        }
        out
    }

    /// The base on show: the figure the shown raster came from (`raster_node`), none when it is not an image node (a PDF
    /// page or a plugin preview is no base), edit mode is on or the refused-hello overlay is on. The canvas base is D5's.
    pub(in crate::ui) fn shown_base(raster_node: Option<&str>, editing: bool, refused: bool) -> Option<InkBase> {
        let node_id = raster_node.filter(|n| !editing && !refused && crate::ui::State::is_image_node_id(n))?;
        Some(InkBase::Figure { node_id: node_id.to_string() })
    }

    pub(in crate::ui) fn overlay(&self) -> Option<&Quad> {
        self.overlay.as_ref()
    }

    /// The dirty rectangle (all of it for a new raster size) goes to the overlay texture. A failure keeps the rectangle,
    /// so it repeats every frame: it is logged once until an upload succeeds.
    pub(in crate::ui) fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, pipeline: &QuadPipeline) -> anyhow::Result<()> {
        let sent = self.send(device, queue, pipeline);
        if let Err(e) = &sent {
            if !self.upload_failed {
                tracing::warn!(error = %e, "the drawing's overlay was not uploaded; skipped until it is");
            }
        }
        self.upload_failed = sent.is_err();
        sent
    }

    fn send(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, pipeline: &QuadPipeline) -> anyhow::Result<()> {
        let Some((x, y, w, h)) = self.dirty() else { return Ok(()) };
        match self.overlay.as_ref().filter(|q| q.size_px == self.dims) {
            Some(q) => q.write_region(queue, &self.rgba((x, y, w, h)), x, y, w, h)?,
            None => {
                let rgba = self.rgba((0, 0, self.dims.0, self.dims.1));
                self.overlay = Some(Quad::from_rgba8(device, queue, pipeline, &rgba, self.dims.0, self.dims.1)?);
            }
        }
        self.dirty = None;
        Ok(())
    }

    fn points(&self) -> usize {
        self.strokes.iter().chain(self.live.iter()).map(Vec::len).sum()
    }

    /// Whether a press at `p` reaches `shown`, the base on show (`None`: none): drawing is on, or `shown` is the canvas,
    /// and `p` is inside the image rectangle and the canvas rectangle.
    pub(in crate::ui) fn reaches(&self, shown: Option<&InkBase>, p: (f32, f32), canvas: &ScreenRect, image: &ScreenRect) -> bool {
        shown.is_some_and(|b| (self.on || *b == InkBase::Canvas) && inside(image, p) && inside(canvas, p))
    }

    /// A left press: ends a stroke whose release was lost, then begins one. Another base replaces the row's drawing, and
    /// the text for the status line comes back. `src` is the base's source size: the raster takes its dims from it, and
    /// is painted again when the base or those dims change.
    pub(in crate::ui) fn press(
        &mut self, shown: Option<&InkBase>, p: (f32, f32), canvas: &ScreenRect, image: &ScreenRect, src: (u32, u32),
    ) -> Option<String> {
        if !self.reaches(shown, p, canvas, image) {
            return None;
        }
        let (shown, q) = (shown?, map(p, canvas)?);
        self.release();
        let mut note = None;
        if self.base.as_ref() != Some(shown) {
            if !self.strokes.is_empty() {
                note = self.base.as_ref().map(|b| format!("the drawing on {} was replaced", name(b)));
                self.revision += 1;
            }
            self.strokes.clear();
            self.history.clear();
            self.cleared.clear();
            self.base = Some(shown.clone());
            self.dims = (0, 0);
        }
        if self.dims != raster_dims(src.0, src.1) {
            self.dims = raster_dims(src.0, src.1);
            self.repaint();
        }
        if self.points() >= MAX_INK_POINTS {
            return Some("the drawing is full: undo or clear".to_string());
        }
        self.live = Some(vec![q]);
        let a = to_raster(q, self.dims);
        self.paint(a, a);
        note
    }

    /// Motion with a left stroke live: a point is kept one raster pixel from the last, and dropped past the bound (the
    /// status text comes back).
    pub(in crate::ui) fn drag(&mut self, p: (f32, f32), canvas: &ScreenRect) -> Option<&'static str> {
        let dims = self.dims;
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
        self.paint(a, b);
        None
    }

    /// A release or a lost focus ends an erase step and commits the live stroke (which drops the `cleared` copy).
    pub(in crate::ui) fn release(&mut self) {
        self.erasing = None;
        if let Some(s) = self.live.take() {
            self.strokes.push(s);
            self.history.push(Edit::Added);
            self.cleared.clear();
            self.revision += 1;
        }
    }

    /// The base stops being shown: a live stroke is dropped, not committed.
    pub(in crate::ui) fn base_hidden(&mut self) {
        if self.live.take().is_some() {
            self.repaint();
        }
        self.erasing = None;
    }

    /// Undo acts only while `shown` is the drawing's base. It drops a live stroke and nothing else; otherwise it takes back
    /// the last step, or brings back what a clear moved.
    pub(in crate::ui) fn undo(&mut self, shown: Option<&InkBase>) {
        if shown != self.base.as_ref() {
            return;
        }
        self.erasing = None;
        if self.live.take().is_some() {
            self.repaint();
            return;
        }
        match self.history.pop() {
            Some(Edit::Added) => drop(self.strokes.pop()),
            Some(Edit::Erased(gone)) => gone.into_iter().rev().for_each(|(i, s)| self.strokes.insert(i, s)),
            None if !self.cleared.is_empty() => {
                self.strokes = std::mem::take(&mut self.cleared);
                self.history = self.strokes.iter().map(|_| Edit::Added).collect();
            }
            None => return,
        }
        self.repaint();
        self.revision += 1;
    }

    /// Clear acts only while `shown` is the drawing's base. It drops a live stroke and nothing else; otherwise the
    /// strokes move to the `cleared` copy.
    pub(in crate::ui) fn clear(&mut self, shown: Option<&InkBase>) {
        if shown != self.base.as_ref() {
            return;
        }
        if self.live.take().is_some() {
            self.repaint();
            return;
        }
        if self.strokes.is_empty() {
            return;
        }
        self.cleared = std::mem::take(&mut self.strokes);
        self.history.clear();
        self.repaint();
        self.revision += 1;
    }

    /// A right press (and each right motion): removes each committed stroke the path from the drag's last point to `p`
    /// comes within the painted ink's reach of (`ink_radius` and one raster pixel). Ignored while a left stroke is live,
    /// and for a point outside the image. The strokes one drag removes are one undo step, recorded at its first hit.
    pub(in crate::ui) fn erase(
        &mut self, shown: Option<&InkBase>, p: (f32, f32), canvas: &ScreenRect, image: &ScreenRect,
    ) {
        let dims = self.dims;
        let ok = self.live.is_none() && shown == self.base.as_ref() && self.reaches(shown, p, canvas, image);
        let Some(q) = map(p, canvas).filter(|_| ok) else {
            self.erasing = self.erasing.map(|(recorded, _)| (recorded, None));
            return;
        };
        let (recorded, last) = self.erasing.unwrap_or((false, None));
        let (a, b, reach) = (to_raster(last.unwrap_or(q), dims), to_raster(q, dims), ink_radius(dims) + 1.0);
        let near = |s: &Stroke| segments(std::slice::from_ref(s), dims).any(|(c, d)| segs_dist(a, b, c, d) <= reach);
        let mut gone = Vec::new();
        let mut i = 0;
        while i < self.strokes.len() {
            if near(&self.strokes[i]) {
                gone.push((i, self.strokes.remove(i)));
            } else {
                i += 1;
            }
        }
        let hit = !gone.is_empty();
        if hit {
            match self.history.last_mut() {
                Some(Edit::Erased(g)) if recorded => g.extend(gone),
                _ => self.history.push(Edit::Erased(gone)),
            }
            self.repaint();
            self.revision += 1;
        }
        self.erasing = Some((recorded || hit, Some(q)));
    }

    /// The right button is up: the next erase starts a new step.
    pub(in crate::ui) fn erase_end(&mut self) {
        self.erasing = None;
    }
}

#[cfg(test)]
#[path = "ink_tests.rs"]
mod ink_tests;
