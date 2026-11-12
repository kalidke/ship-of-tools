//! The installed preview source: the bytes the pane shows, the node their reply answered, their physical scale and,
//! for markdown, the document's origin. One value, so a row's snapshot carries all of it together.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use super::image::overlay::PhysicalScale;

/// The last generation handed out, process-wide: a restored or emptied source must never reuse the number of one a
/// drawing job still carries.
static LAST_GENERATION: AtomicU64 = AtomicU64::new(0);

fn fresh_generation() -> u64 {
    LAST_GENERATION.fetch_add(1, Relaxed) + 1
}

/// What the last admitted preview reply installed. Its parts travel together: a font rescale or a row's restore
/// repaints from these bytes without a round trip, so the node (what `o`/`W`/`O` act on, distinct from the fired
/// target while a newer request is in flight), the scale (ADR 0034: another row's calibration must never label these
/// pixels) and the markdown origin (relative figures resolve against this document's folder and workspace) must be
/// the bytes' own.
#[derive(Clone)]
pub(in crate::ui) struct InstalledPreviewSource {
    /// Which source this is, for the drawing worker (`svg_draws`): raised by an install and by `renewed`, kept by
    /// anything that draws the same source again (a reflow, a text-scale change). Never zero.
    generation: u64,
    /// `(mime, bytes)`; `None` until a reply installs, and after a first visit clears.
    source: Option<(String, Vec<u8>)>,
    node_id: Option<String>,
    scale: Option<PhysicalScale>,
    /// The last markdown document installed with a node id, and its workspace; a later non-markdown reply leaves
    /// them, as figure fetches for the shown markdown still need them.
    md_node_id: Option<String>,
    md_workspace_id: Option<String>,
}

impl Default for InstalledPreviewSource {
    fn default() -> Self {
        Self { generation: fresh_generation(), source: None, node_id: None, scale: None, md_node_id: None, md_workspace_id: None }
    }
}

impl InstalledPreviewSource {
    /// A snapshot's source coming back: the same bytes, a new generation, so a job the departed row left is stale.
    pub(in crate::ui) fn renewed(mut self) -> Self {
        self.generation = fresh_generation();
        self
    }

    #[cfg_attr(not(test), expect(dead_code, reason = "the drawing worker, the next landing, reads it"))]
    pub(in crate::ui) fn generation(&self) -> u64 { self.generation }

    /// An admitted reply: its bytes, node and scale replace the installed ones; a markdown reply with a node also
    /// becomes the document origin.
    pub(in crate::ui) fn install(
        &mut self, mime: String, bytes: Vec<u8>, node_id: Option<String>, scale: Option<PhysicalScale>,
        workspace_id: Option<String>,
    ) {
        if matches!(mime.as_str(), "text/markdown" | "text/x-markdown") {
            if let Some(id) = node_id.as_ref() {
                self.md_node_id = Some(id.clone());
                self.md_workspace_id = workspace_id;
            }
        }
        self.generation = fresh_generation();
        self.source = Some((mime, bytes));
        self.node_id = node_id;
        self.scale = scale;
    }

    pub(in crate::ui) fn mime(&self) -> Option<&str> { self.source.as_ref().map(|(m, _)| m.as_str()) }
    pub(in crate::ui) fn bytes(&self) -> Option<&[u8]> { self.source.as_ref().map(|(_, b)| b.as_slice()) }

    /// An owned copy for a repaint, which needs the window mutably while it shapes.
    pub(in crate::ui) fn source_copy(&self) -> Option<(String, Vec<u8>)> { self.source.clone() }

    pub(in crate::ui) fn node_id(&self) -> Option<&str> { self.node_id.as_deref() }
    pub(in crate::ui) fn scale(&self) -> Option<&PhysicalScale> { self.scale.as_ref() }
    pub(in crate::ui) fn md_node_id(&self) -> Option<&str> { self.md_node_id.as_deref() }
    pub(in crate::ui) fn md_workspace_id(&self) -> Option<&str> { self.md_workspace_id.as_deref() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed(source: &mut InstalledPreviewSource) {
        source.install("text/plain".into(), b"x".to_vec(), None, None, None);
    }

    #[test]
    fn a_new_source_is_a_new_generation_and_the_same_source_is_not() {
        let mut source = InstalledPreviewSource::default();
        let empty = source.generation();
        installed(&mut source);
        let installed_once = source.generation();
        assert_ne!(installed_once, empty);
        // Repainting reads the source and never installs it.
        let _ = source.source_copy();
        assert_eq!(source.generation(), installed_once);
        installed(&mut source);
        assert_ne!(source.generation(), installed_once);
    }

    #[test]
    fn a_restored_or_emptied_source_never_reuses_a_generation() {
        let mut source = InstalledPreviewSource::default();
        installed(&mut source);
        let snapshot = source.clone();
        assert_eq!(snapshot.generation(), source.generation(), "a clone is the same source");
        let restored = snapshot.renewed();
        assert_ne!(restored.generation(), source.generation());
        assert_ne!(InstalledPreviewSource::default().generation(), restored.generation());
    }
}
