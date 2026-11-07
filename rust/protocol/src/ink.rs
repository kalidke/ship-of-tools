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

/// The coverage byte of a pixel whose center is `d` from the segment: `r + 0.5` minus `d`, clamped to 0..1, as 0..255.
fn coverage(r: f32, d: f32) -> u8 {
    (255.0 * (r + 0.5 - d).clamp(0.0, 1.0)).round() as u8
}

/// Paint the segment `a` to `b` (raster positions) of radius `r` into `mask`, `rw * rh` coverage bytes: a pixel keeps
/// the larger of its coverage and the segment's. Round caps and joins; `a == b` is a dot. Taking the maximum makes
/// painting order-free. Each row visits only the pixels within reach of the segment, so the cost follows the segment's
/// own area, not its bounding box. Returns the pixels visited.
pub fn paint_segment(mask: &mut [u8], (rw, rh): (u32, u32), a: (f32, f32), b: (f32, f32), r: f32) -> usize {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let reach = r + 0.5;
    let y0 = (a.1.min(b.1) - r - 1.0).floor().max(0.0) as u32;
    let y1 = ((a.1.max(b.1) + r + 1.0).ceil().max(0.0) as u32).min(rh.saturating_sub(1));
    let mut visited = 0;
    for y in y0..=y1 {
        let cy = y as f32 + 0.5;
        // the part of the segment within `reach` of this row's centers, as parameters 0..1
        let (t0, t1) = if dy != 0.0 {
            let (u, v) = ((cy - reach - a.1) / dy, (cy + reach - a.1) / dy);
            (u.min(v).max(0.0), u.max(v).min(1.0))
        } else if (a.1 - cy).abs() <= reach {
            (0.0, 1.0)
        } else {
            continue;
        };
        if t0 > t1 {
            continue;
        }
        let (xa, xb) = (a.0 + t0 * dx, a.0 + t1 * dx);
        let x0 = (xa.min(xb) - reach - 1.0).floor().max(0.0) as u32;
        let x1 = ((xa.max(xb) + reach + 1.0).ceil().max(0.0) as u32).min(rw.saturating_sub(1));
        for x in x0..=x1 {
            let (px, py) = (x as f32 + 0.5 - a.0, cy - a.1);
            let t = if len2 > 0.0 { ((px * dx + py * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
            let (ex, ey) = (px - t * dx, py - t * dy);
            let m = &mut mask[(y * rw + x) as usize];
            *m = (*m).max(coverage(r, (ex * ex + ey * ey).sqrt()));
            visited += 1;
        }
    }
    visited
}

/// The segments of a drawing in raster positions: every consecutive pair of points of every stroke, and a one-point
/// stroke as one segment from its point to itself (a dot). The window's press is such a dot.
pub fn segments(strokes: &[Vec<[u16; 2]>], dims: (u32, u32)) -> impl Iterator<Item = ((f32, f32), (f32, f32))> + '_ {
    strokes.iter().flat_map(move |s| {
        let dot = (s.len() == 1).then(|| (to_raster(s[0], dims), to_raster(s[0], dims)));
        dot.into_iter().chain(s.windows(2).map(move |w| (to_raster(w[0], dims), to_raster(w[1], dims))))
    })
}

/// The mask of a drawing's strokes on a raster `dims`.
pub fn mask(strokes: &[Vec<[u16; 2]>], dims: (u32, u32)) -> Vec<u8> {
    let mut m = vec![0u8; dims.0 as usize * dims.1 as usize];
    let r = ink_radius(dims);
    for (a, b) in segments(strokes, dims) {
        paint_segment(&mut m, dims, a, b, r);
    }
    m
}

#[cfg(test)]
#[path = "ink_tests.rs"]
mod ink_tests;
