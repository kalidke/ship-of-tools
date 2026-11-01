use super::*;

fn cell(x: f32, color: (u8, u8, u8)) -> CellQuad {
    CellQuad { x, y: 0.0, w: 1.0, h: 1.0, color }
}

#[test]
fn each_colour_takes_one_texel_and_its_rects_sample_the_centre() {
    let red = (205, 49, 49);
    let blue = (36, 114, 200);
    let (palette, sampled) = back_palette(&[cell(0.0, red), cell(1.0, blue), cell(2.0, red)]);
    assert_eq!(palette, vec![red, blue]);
    let points: Vec<[f32; 2]> = sampled.iter().map(|(_, uv)| *uv).collect();
    assert_eq!(points, vec![[0.25, 0.5], [0.75, 0.5], [0.25, 0.5]]);
    assert_eq!(sampled.iter().map(|(r, _)| r.x).collect::<Vec<_>>(), vec![0.0, 1.0, 2.0]);
}

#[test]
fn a_colour_past_the_cap_paints_no_background() {
    let rects: Vec<CellQuad> =
        (0..=MAX_BACK_COLORS as u32).map(|i| cell(i as f32, ((i >> 16) as u8, (i >> 8) as u8, i as u8))).collect();
    let (palette, sampled) = back_palette(&rects);
    assert_eq!(palette.len(), MAX_BACK_COLORS);
    assert_eq!(sampled.len(), MAX_BACK_COLORS);
    assert!(sampled.iter().all(|(r, _)| r.x < MAX_BACK_COLORS as f32));
}
