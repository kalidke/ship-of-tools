// the one raster of a drawing's ink: the window paints it as the person draws, the daemon over the base it composes,
// and both get the same mask

/// The longest side of a drawing's raster, in pixels.
pub const INK_RASTER_MAX: u32 = 2000;
/// The raster of a blank canvas (16:10).
pub const CANVAS_DIMS: (u32, u32) = (1600, 1000);
/// The one ink color.
pub const INK_RGB: [u8; 3] = [230, 32, 32];

/// The raster of a base `w` by `h`: the same aspect, the long side at most `INK_RASTER_MAX`, no side under one pixel.
pub fn raster_dims(w: u32, h: u32) -> (u32, u32) {
    let s = (INK_RASTER_MAX as f32 / w.max(h).max(1) as f32).min(1.0);
    (((w as f32 * s).round() as u32).max(1), ((h as f32 * s).round() as u32).max(1))
}

/// The ink's radius in raster pixels: a line 3 pixels wide, or 1/200 of the long side if more.
pub fn ink_radius((rw, rh): (u32, u32)) -> f32 {
    (3.0f32).max(rw.max(rh) as f32 / 200.0) / 2.0
}

/// A wire point as a position in the raster, in pixels.
pub fn to_raster(p: [u16; 2], (rw, rh): (u32, u32)) -> (f32, f32) {
    (p[0] as f32 / 65535.0 * rw as f32, p[1] as f32 / 65535.0 * rh as f32)
}

/// Paint the segment `a` to `b` (raster positions) of radius `r` into `mask`, `rw * rh` coverage bytes: a pixel keeps
/// the larger of its coverage and the segment's, `r + 0.5` minus the distance from its center to the segment, clamped
/// to 0..1. Round caps and joins; `a == b` is a dot. Taking the maximum makes painting order-free.
pub fn paint_segment(mask: &mut [u8], (rw, rh): (u32, u32), a: (f32, f32), b: (f32, f32), r: f32) {
    let x0 = (a.0.min(b.0) - r - 1.0).floor().max(0.0) as u32;
    let y0 = (a.1.min(b.1) - r - 1.0).floor().max(0.0) as u32;
    let x1 = ((a.0.max(b.0) + r + 1.0).ceil().max(0.0) as u32).min(rw.saturating_sub(1));
    let y1 = ((a.1.max(b.1) + r + 1.0).ceil().max(0.0) as u32).min(rh.saturating_sub(1));
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let (px, py) = (x as f32 + 0.5 - a.0, y as f32 + 0.5 - a.1);
            let t = if len2 > 0.0 { ((px * dx + py * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
            let d = ((px - t * dx).powi(2) + (py - t * dy).powi(2)).sqrt();
            let v = (255.0 * (r + 0.5 - d).clamp(0.0, 1.0)).round() as u8;
            let m = &mut mask[(y * rw + x) as usize];
            *m = (*m).max(v);
        }
    }
}

/// The mask of a drawing's strokes on a raster `dims`: every consecutive pair of points of every stroke, a one-point
/// stroke as a dot.
pub fn mask(strokes: &[Vec<[u16; 2]>], dims: (u32, u32)) -> Vec<u8> {
    let mut m = vec![0u8; dims.0 as usize * dims.1 as usize];
    let r = ink_radius(dims);
    for s in strokes {
        for (i, p) in s.iter().enumerate() {
            let a = to_raster(*p, dims);
            let b = s.get(i + 1).map_or(a, |q| to_raster(*q, dims));
            if i + 1 < s.len() || s.len() == 1 {
                paint_segment(&mut m, dims, a, b, r);
            }
        }
    }
    m
}

#[cfg(test)]
#[path = "ink_tests.rs"]
mod ink_tests;
