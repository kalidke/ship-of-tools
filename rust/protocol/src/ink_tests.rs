use super::*;

#[test]
fn one_raster_for_a_drawing_whatever_order_its_segments_are_painted() {
    // the long side is capped at 2000 and the aspect kept; a small base is not scaled up; no side is under 1
    assert_eq!(raster_dims(6400, 3200), (2000, 1000));
    assert_eq!(raster_dims(3000, 6000), (1000, 2000));
    assert_eq!(raster_dims(2000, 1500), (2000, 1500));
    assert_eq!(raster_dims(300, 200), (300, 200));
    assert_eq!(raster_dims(100000, 10), (2000, 1));

    // the radius: 1.5 up to 600 pixels, then 1/400 of the long side
    assert_eq!(ink_radius((600, 300)), 1.5);
    assert_eq!(ink_radius((2000, 1500)), 5.0);

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
