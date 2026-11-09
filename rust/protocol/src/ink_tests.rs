use super::*;

#[test]
fn one_raster_for_a_drawing_whatever_order_its_segments_are_painted() {
    // the long side is capped at 2000 and the aspect kept; a small base is not scaled up; no side is under 1
    assert_eq!(raster_dims(6400, 3200), (2000, 1000));
    assert_eq!(raster_dims(3000, 6000), (1000, 2000));
    assert_eq!(raster_dims(2000, 1500), (2000, 1500));
    assert_eq!(raster_dims(300, 200), (300, 200));
    assert_eq!(raster_dims(100000, 10), (2000, 1));

    // the radius: 1.5 up to 900 pixels, then half of 1/300 of the long side
    assert_eq!(ink_radius((600, 300)), 1.5);
    assert_eq!(ink_radius((900, 300)), 1.5);
    assert_eq!(ink_radius((1200, 800)), 2.0);
    assert_eq!(ink_radius((2000, 1500)), 2000.0 / 300.0 / 2.0);

    // a wire point maps x to the width and y to the height, 65535 to the far edge
    assert_eq!(to_raster([65535, 0], (100, 50)), (100.0, 0.0));
    assert_eq!(to_raster([0, 65535], (100, 50)), (0.0, 50.0));
    assert_eq!(to_raster([0, 0], (100, 50)), (0.0, 0.0));

    // one segment: 255 on it, 0 past r + 0.5, partial between
    let dims = (100, 50);
    let r = 2.0;
    let (a, b) = ((20.5, 25.5), (60.5, 25.5));
    let mut m = vec![0u8; 5000];
    paint_segment(&mut m, dims, a, b, r);
    let at = |m: &[u8], x: usize, y: usize| m[y * 100 + x];
    assert_eq!(at(&m, 40, 25), 255);
    assert_eq!(at(&m, 20, 25), 255);
    assert_eq!(at(&m, 40, 26), 255);
    assert_eq!(at(&m, 40, 27), 128);
    assert_eq!(at(&m, 40, 28), 0);
    assert_eq!(at(&m, 40, 22), 0);
    assert_eq!(at(&m, 19, 25), 255);
    assert_eq!(at(&m, 18, 25), 128);
    assert_eq!(at(&m, 17, 25), 0);
    assert_eq!(at(&m, 65, 25), 0);

    // a one-point stroke is a dot, centered on its point
    let dot = mask(&[vec![[32768, 32768]]], (101, 101));
    assert_eq!(dot[50 * 101 + 50], 255);
    assert_eq!(dot[50 * 101 + 70], 0);
    assert!(dot.iter().filter(|&&v| v > 0).count() > 4);

    // painting the segments one at a time, in any order, gives `mask`
    let strokes: Vec<Vec<[u16; 2]>> = vec![
        vec![[1000, 2000], [60000, 5000], [30000, 62000], [100, 65535]],
        vec![[20000, 20000]],
        vec![[65535, 0], [0, 65535]],
    ];
    let dims = (160, 100);
    let want = mask(&strokes, dims);
    assert!(want.iter().any(|&v| v == 255) && want.iter().any(|&v| v == 0));
    let r = ink_radius(dims);
    let mut segs = Vec::new();
    for s in &strokes {
        for i in 0..s.len() {
            let a = to_raster(s[i], dims);
            if i + 1 < s.len() {
                segs.push((a, to_raster(s[i + 1], dims)));
            } else if s.len() == 1 {
                segs.push((a, a));
            }
        }
    }
    for order in 0..3 {
        let mut idx: Vec<usize> = (0..segs.len()).collect();
        match order {
            0 => {}
            1 => idx.reverse(),
            _ => idx.sort_by_key(|i| (i * 7) % segs.len()),
        }
        let mut m = vec![0u8; 160 * 100];
        for i in idx {
            paint_segment(&mut m, dims, segs[i].0, segs[i].1, r);
        }
        assert_eq!(m, want, "order {order}");
    }
    // a point at the far edge stays inside the buffer
    assert_eq!(mask(&[vec![[65535, 65535]]], (10, 10)).len(), 100);
}

struct Rng(u64);
impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

/// Every pixel against every segment: the distance rule with no box and no row spans.
fn reference(strokes: &[Vec<[u16; 2]>], (rw, rh): (u32, u32)) -> Vec<u8> {
    let r = ink_radius((rw, rh));
    let segs: Vec<_> = segments(strokes, (rw, rh)).collect();
    let mut m = vec![0u8; (rw * rh) as usize];
    for y in 0..rh {
        for x in 0..rw {
            let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
            for &(a, b) in &segs {
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let len2 = dx * dx + dy * dy;
                let t = if len2 > 0.0 { (((cx - a.0) * dx + (cy - a.1) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
                let (ex, ey) = (cx - a.0 - t * dx, cy - a.1 - t * dy);
                let v = (255.0 * (r + 0.5 - (ex * ex + ey * ey).sqrt()).clamp(0.0, 1.0)).round() as u8;
                m[(y * rw + x) as usize] = m[(y * rw + x) as usize].max(v);
            }
        }
    }
    m
}

#[test]
fn the_mask_is_the_distance_rule_on_every_pixel() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for case in 0..300 {
        let dims = (1 + rng.below(60) as u32, 1 + rng.below(60) as u32);
        let pick = |rng: &mut Rng| match rng.below(5) {
            0 => 0u16,
            1 => 65535,
            _ => rng.below(65536) as u16,
        };
        let strokes: Vec<Vec<[u16; 2]>> =
            (0..1 + rng.below(3)).map(|_| (0..1 + rng.below(4)).map(|_| [pick(&mut rng), pick(&mut rng)]).collect()).collect();
        assert_eq!(mask(&strokes, dims), reference(&strokes, dims), "case {case} {dims:?} {strokes:?}");
    }
    // the window's order: a dot at the press, then the segment to the next point, is the two-point stroke
    let dims = (80, 40);
    let r = ink_radius(dims);
    let (a, b) = (to_raster([5000, 9000], dims), to_raster([50000, 40000], dims));
    let mut m = vec![0u8; 3200];
    paint_segment(&mut m, dims, a, a, r);
    paint_segment(&mut m, dims, a, b, r);
    assert_eq!(m, mask(&[vec![[5000, 9000], [50000, 40000]]], dims));
}

#[test]
fn a_segment_costs_its_own_area_not_its_bounding_box() {
    let dims = (2000, 2000);
    let r = ink_radius(dims);
    let mut m = vec![0u8; 2000 * 2000];
    // the longest diagonal, and a shallow and a steep one: pixels visited stay within 3 times the area 2r+1 wide
    for (a, b) in [((0.0f32, 0.0f32), (2000.0f32, 2000.0f32)), ((0.0, 0.0), (2000.0, 100.0)), ((10.0, 0.0), (110.0, 2000.0))] {
        let len = ((b.0 - a.0) * (b.0 - a.0) + (b.1 - a.1) * (b.1 - a.1)).sqrt();
        let area = len * (2.0 * r + 1.0);
        let visited = paint_segment(&mut m, dims, a, b, r);
        assert!((visited as f32) < 3.0 * area, "{a:?} {b:?}: {visited} pixels, area {area}");
    }
}
