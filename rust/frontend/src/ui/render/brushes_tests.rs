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
fn a_colour_past_the_cap_paints_in_the_nearest_colour_held() {
    // Colours (0, i >> 8, i & 255) fill the palette, the last (0, 7, 255); one more, (1, 7, 255), takes its texel.
    let colour = |i: u32| (0u8, (i >> 8) as u8, i as u8);
    let mut rects: Vec<CellQuad> = (0..MAX_BACK_COLORS as u32).map(|i| cell(i as f32, colour(i))).collect();
    rects.push(cell(MAX_BACK_COLORS as f32, (1, 7, 255)));
    let (palette, sampled) = back_palette(&rects);
    assert_eq!(palette.len(), MAX_BACK_COLORS);
    assert_eq!(sampled.len(), MAX_BACK_COLORS + 1);
    assert_eq!(sampled[MAX_BACK_COLORS].1, sampled[MAX_BACK_COLORS - 1].1);
}

#[test]
fn a_surface_that_does_not_encode_srgb_gets_each_colour_back() {
    assert_eq!(palette_bytes(&[(128, 0, 255)], true), vec![128, 0, 255, 255]);
    let stored = palette_bytes(&[(128, 0, 255)], false);
    assert_eq!(stored, vec![188, 0, 255, 255]);
    // What the texture's sRGB decode hands a target that writes it unencoded: the colour's own byte.
    let decode = |s: u8| {
        let x = s as f64 / 255.0;
        let l = if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) };
        (l * 255.0).round() as u8
    };
    assert_eq!(decode(stored[0]), 128);
}
