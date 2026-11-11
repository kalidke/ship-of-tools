use super::*;

#[test]
fn is_image_node_id_matches_rasters_not_pdf() {
    assert!(State::is_image_node_id("files:plots/a.png"));
    assert!(State::is_image_node_id("files:IMG.JPEG"));
    assert!(!State::is_image_node_id("files:doc.pdf"));
    assert!(!State::is_image_node_id("files:src/lib.jl"));
}

#[test]
fn an_svg_is_a_figure_node_but_not_an_image_node() {
    // A figure takes a drawing; an image node is what `image.crop` can decode, a scale and a caption describe, and the
    // view carry keys on, and the daemon cannot crop an SVG.
    assert!(State::is_figure_node_id("files:fig/a.SVG"));
    assert!(State::is_figure_node_id("files:plots/a.png"));
    assert!(!State::is_image_node_id("files:fig/a.svg"));
    assert!(!State::is_figure_node_id("files:fig/a.svgz"));
    assert!(!State::is_figure_node_id("files:doc.pdf"));
}

#[test]
fn a_capture_of_a_shown_svg_says_it_cannot_be_cropped() {
    assert_eq!(no_roi_line(Some("files:fig/a.SVG")), "capture: an SVG cannot be cropped (the daemon crops raster files)");
    assert_eq!(no_roi_line(Some("files:a.png")), "capture: no image ROI in preview (zoom an image first)");
    assert_eq!(no_roi_line(None), "capture: no image ROI in preview (zoom an image first)");
}
