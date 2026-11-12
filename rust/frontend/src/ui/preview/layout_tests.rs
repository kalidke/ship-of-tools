use super::*;
use crate::ui::preview::image::ink::{source_dims, InkLayer};
use crate::ui::preview::source::InstalledPreviewSource;
use sot_protocol::ink::CANVAS_DIMS;
use sot_protocol::ops::InkBase;

/// `(png, edit, md, canvas)` of what the pane decides: the canvas comes from the one decider, `shown_base`.
fn pane(ink: &InkLayer, raster_node: Option<&str>, edit: bool, refused: bool) -> (bool, bool, bool, bool) {
    let canvas = ink.shown_base(raster_node, edit, refused) == Some(InkBase::Canvas);
    let (_, png, edit, md, canvas) = shows(refused, raster_node.is_some(), edit, canvas);
    (png, edit, md, canvas)
}

#[test]
fn the_canvas_takes_the_pane_and_leaves_the_preview_untouched() {
    let (mut source, mut ink) = (InstalledPreviewSource::default(), InkLayer::default());
    source.install("image/png".into(), vec![1, 2], Some("files:a.png".into()), None, None);
    ink.set_canvas(true);
    let node = Some("files:a.png");
    // it wins over the image and the text, so no text is prepared for it
    assert_eq!(pane(&ink, node, false, false), (false, false, false, true));
    assert_eq!(pane(&ink, None, false, false), (false, false, false, true));
    // it loses to the refused-hello overlay and to edit mode
    assert_eq!(pane(&ink, None, false, true), (false, false, false, false));
    assert_eq!(pane(&ink, None, true, false), (false, true, false, false));
    // hidden, the pane shows what it showed
    ink.set_canvas(false);
    assert_eq!(pane(&ink, node, false, false), (true, false, false, false));
    assert_eq!(pane(&ink, None, false, false), (false, false, true, false));
    // an install through the owner's call: another node's hides the canvas, the same node's does not, and the bytes
    // installed are the reply's either way
    ink.set_canvas(true);
    for (node, bytes, shown) in [("files:a.png", 3u8, true), ("files:b.png", 4, false)] {
        ink.install_preview(&mut source, "image/png".into(), vec![bytes], Some(node.into()), None, None);
        assert_eq!(pane(&ink, Some(node), false, false).3, shown);
        assert_eq!((source.node_id(), source.bytes()), (Some(node), Some(&[bytes][..])));
    }
}

#[test]
fn the_canvas_has_its_own_source_size() {
    assert_eq!(source_dims(Some(&InkBase::Canvas), None), Some(CANVAS_DIMS));
    assert_eq!(source_dims(Some(&InkBase::Canvas), Some((5, 6))), Some(CANVAS_DIMS));
    assert_eq!(source_dims(Some(&InkBase::Figure { node_id: "f".into() }), Some((5, 6))), Some((5, 6)));
    assert_eq!(source_dims(None, None), None);
}
