//! The installed preview source: the bytes the pane shows, the node their reply answered, their physical scale and,
//! for markdown, the document's origin. One value, so a row's snapshot carries all of it together.

use super::image::overlay::PhysicalScale;

/// What the last admitted preview reply installed. Its parts travel together: a font rescale or a row's restore
/// repaints from these bytes without a round trip, so the node (what `o`/`W`/`O` act on, distinct from the fired
/// target while a newer request is in flight), the scale (ADR 0034: another row's calibration must never label these
/// pixels) and the markdown origin (relative figures resolve against this document's folder and workspace) must be
/// the bytes' own.
#[derive(Clone, Default)]
pub(in crate::ui) struct InstalledPreviewSource {
    /// `(mime, bytes)`; `None` until a reply installs, and after a first visit clears.
    source: Option<(String, Vec<u8>)>,
    node_id: Option<String>,
    scale: Option<PhysicalScale>,
    /// The last markdown document installed with a node id, and its workspace; a later non-markdown reply leaves
    /// them, as figure fetches for the shown markdown still need them.
    md_node_id: Option<String>,
    md_workspace_id: Option<String>,
}

impl InstalledPreviewSource {
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
