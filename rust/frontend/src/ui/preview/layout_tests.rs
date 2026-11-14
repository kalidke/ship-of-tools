use super::*;
use crate::ui::preview::image::ink::{source_dims, InkLayer};
use crate::ui::preview::source::InstalledPreviewSource;
use sot_protocol::ink::CANVAS_DIMS;
use sot_protocol::ops::InkBase;

/// `(png, edit, md, canvas)` of what the pane decides: the canvas comes from the one decider, `shown_base`.
fn pane(ink: &InkLayer, raster_node: Option<&str>, edit: bool, refused: bool) -> (bool, bool, bool, bool) {
    let canvas = ink.shown_base(raster_node, edit, refused) == Some(InkBase::Canvas);
    let s = shows(refused, raster_node.is_some(), edit, canvas);
    (s.png, s.edit, s.md, s.canvas)
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

impl State {
    /// A wheel tick over a long preview moves it over frames, the pacer asks for each, the hit test follows the drawn
    /// offset, and it comes to rest exactly three lines down with no frame left owed. The frame times print
    /// (`smooth-witness`), asserting none.
    #[allow(dead_code, reason = "runs only inside a native State, from the premise overlays; no libtest body can construct one")]
    pub(in crate::ui) fn a_preview_scroll_moves_over_frames_and_comes_to_rest(&mut self) -> anyhow::Result<()> {
        self.redraw()?;
        let source: String = (0..400).map(|n| format!("line {n}\n\n")).collect();
        self.preview_source.install("text/markdown".into(), source.clone().into_bytes(), Some("files:long.md".into()), None, None);
        self.render_preview_source("text/markdown", source.as_bytes());
        self.set_focus(PaneFocus::Preview);
        self.redraw()?;
        anyhow::ensure!(self.shaped.scroll() == 0.0, "a new file did not start at the top: {}", self.shaped.scroll());
        // One tick down: three rows (the wheel adapter's step), owed, not applied.
        self.pointer.clear_residue();
        crate::ui::input::mouse::mouse_wheel(self, winit::keyboard::ModifiersState::empty(), winit::event::MouseScrollDelta::LineDelta(0.0, -1.0));
        anyhow::ensure!(self.shaped.scroll() == 0.0, "the wheel moved the preview at the event, not over frames");
        let (mut frames, mut between) = (Vec::new(), false);
        let begun = std::time::Instant::now();
        while self.shaped.scroll() != 3.0 {
            anyhow::ensure!(begun.elapsed() < std::time::Duration::from_secs(5), "the scroll never came to rest: {}", self.shaped.scroll());
            let at = std::time::Instant::now();
            self.redraw()?;
            frames.push(at.elapsed().as_secs_f64() * 1000.0);
            let lines = self.shaped.scroll();
            if lines > 0.0 && lines < 3.0 {
                between = true;
                anyhow::ensure!(self.pacer.is_dirty(), "a frame mid-motion did not ask for the next");
                let px = self.shaped.scroll_px();
                anyhow::ensure!(px == px.round(), "the drawn offset {px} is not a whole pixel");
            }
            std::thread::sleep(std::time::Duration::from_millis(4));
        }
        anyhow::ensure!(between, "no frame showed the preview between the lines");
        self.pacer.clear_dirty();
        self.redraw()?;
        anyhow::ensure!(!self.pacer.is_dirty(), "a preview at rest kept asking for frames");
        anyhow::ensure!(self.shaped.scroll_px() == 3.0 * self.shaped.md().line_height().max(1.0), "the rest is not three whole lines");
        frames.sort_by(f64::total_cmp);
        let p95 = frames[(frames.len() * 95 / 100).min(frames.len() - 1)];
        println!("smooth-witness frames={} max_ms={:.1} p95_ms={p95:.1}", frames.len(), frames[frames.len() - 1]);
        Ok(())
    }
}
