//! The concept slot's coordinator: a file's change forgets what it invalidated, and the cursor's follow reads the
//! annotation and arms the drift parse.

use crate::ui::*;
use super::concept::{node_id_to_concept_target, ChangeEffect};

impl State {
    /// A `preview.changed` for `node_id` (a `files:` id): drop the changed file's cached drift check so the next
    /// view re-parses it, and re-read the cursored annotation when it is the one that changed (a reconcile
    /// rewrote its `synced_against`). Per file; hashing stays lazy, on view.
    pub(in crate::ui) fn forget_changed_file(&mut self, node_id: &str) {
        match self.preview_requests.invalidate_changed(node_id) {
            ChangeEffect::ReadAnnotationAgain => {},
            ChangeEffect::ForgetFile(path) => self.drift.forget(path),
            ChangeEffect::Nothing => {}
        }
    }
}

impl State {
    /// If the selected tree row's annotation target differs from the last
    /// one we asked the backend about, fire a fresh `concept.read`. Called
    /// from `redraw` so cursor moves and event-driven tree updates both
    /// trigger refresh without each caller having to remember.
    pub(in crate::ui) fn maybe_fire_concept_read(&mut self) {
        let Some(row) = self.nav.tree().rows().get(self.nav.tree().selected()) else {
            return;
        };
        let target = node_id_to_concept_target(&row.node.id);
        // Only fire file.parse for real Julia source. Directories get an
        // io_error from the kernel; binary files (.h5, .png, .arrow, …)
        // make the JuliaSyntax parser walk huge byte streams looking for
        // valid syntax and block the kernel queue for every subsequent
        // request — observed as a full-app freeze when the cursor lands
        // on a multi-MB HDF5 file. Restrict to `.jl` so the drift check
        // only runs where it can possibly succeed.
        let files_path = if row.node.kind == "dir" {
            None
        } else {
            row.node.id.strip_prefix("files:").and_then(|p| {
                if p.is_empty() || !p.ends_with(".jl") {
                    None
                } else {
                    Some(p.to_string())
                }
            })
        };
        let node_label = row.node.label.clone();
        let workspace_id = self.selected.workspace_id().clone();
        let follow = self.preview_requests.prepare_concept_follow(target, workspace_id.clone());
        super::requests::continue_concept_follow(self, follow, files_path, node_label, workspace_id);
    }
}

impl super::requests::ConceptRequestContext for State {
    fn send_request(&mut self, request: crate::net::transport::OutgoingReq) -> Result<(), String> {
        self.send(request).map_err(|e| e.to_string())
    }

    fn commit_follow(&mut self, commit: super::requests::ConceptCommit) {
        let super::requests::ClearConceptCaches = self.preview_requests.commit_concept_follow(commit);
        self.concept.clear();
    }

    fn take_drift_parse(&mut self, path: &str) -> bool {
        self.drift.take_parse_fire(path)
    }

    fn forget_drift_fire(&mut self, path: &str) {
        self.drift.forget_fire(path);
    }
}
