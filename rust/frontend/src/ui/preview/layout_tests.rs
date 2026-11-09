use super::*;
use crate::ui::preview::image::ink::InkLayer;
use crate::ui::preview::source::InstalledPreviewSource;

/// `(png, edit, md, canvas)` of `shows`.
fn pane(refused: bool, raster: bool, edit: bool, canvas: bool) -> (bool, bool, bool, bool) {
    let (_, png, _, edit, md, canvas) = shows(refused, raster, edit, canvas);
    (png, edit, md, canvas)
}

#[test]
fn the_canvas_takes_the_pane_and_leaves_the_preview_untouched() {
    // it wins over the image and the text, so no text is prepared for it
    assert_eq!(pane(false, true, false, true), (false, false, false, true));
    assert_eq!(pane(false, false, false, true), (false, false, false, true));
    // it loses to the refused-hello overlay and to edit mode
    assert_eq!(pane(true, false, false, true), (false, false, false, false));
    assert_eq!(pane(false, false, true, true), (false, true, false, false));
    // hidden, the pane shows what it showed
    assert_eq!(pane(false, true, false, false), (true, false, false, false));
    assert_eq!(pane(false, false, false, false), (false, false, true, false));
    // the installed preview is untouched; another node's install hides the canvas, the same node's does not
    let (mut source, mut ink) = (InstalledPreviewSource::default(), InkLayer::default());
    source.install("image/png".into(), vec![1, 2], Some("files:a.png".into()), None, None);
    ink.set_canvas(true);
    for (node, shown) in [("files:a.png", true), ("files:b.png", false)] {
        ink.installed(source.node_id(), Some(node));
        source.install("image/png".into(), vec![3], Some(node.into()), None, None);
        assert_eq!((ink.canvas(), source.node_id()), (shown, Some(node)));
    }
    assert_eq!(source.bytes(), Some(&[3u8][..]));
}
