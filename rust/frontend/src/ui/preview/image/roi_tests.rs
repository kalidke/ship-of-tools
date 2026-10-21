use super::*;

#[test]
fn is_image_node_id_matches_rasters_not_pdf() {
    assert!(State::is_image_node_id("files:plots/a.png"));
    assert!(State::is_image_node_id("files:IMG.JPEG"));
    assert!(!State::is_image_node_id("files:doc.pdf"));
    assert!(!State::is_image_node_id("files:src/lib.jl"));
}
