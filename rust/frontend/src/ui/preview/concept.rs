//! The concept-annotation slot beside the preview: the read, the frontmatter split and the drift check.

use crate::ui::*;

/// Annotation read for one node. Cached on `State`; the chrome reads it to
/// decide whether to show "annotation: present" or "(no annotation)".
#[derive(Clone)]
pub(in crate::ui) struct ConceptInfo {
    pub(in crate::ui) target: String,
    pub(in crate::ui) exists: bool,
    pub(in crate::ui) content: String,
    /// `synced_against` AST hash parsed out of the annotation's YAML
    /// frontmatter. `None` when no annotation exists, no frontmatter, or
    /// the field is absent. Compared against `file_ast_hashes[path]` to
    /// drive the drift badge.
    pub(in crate::ui) synced_against: Option<String>,
}

/// An annotation as `(header, body)`: the header, fences included, is `None` when `sot_protocol::annotation`
/// finds no complete one, and the two concatenate to the original. Edit mode keeps the header read-only above
/// the edit area and joins it back on save.
pub(in crate::ui) fn split_frontmatter(s: &str) -> (Option<String>, String) {
    match sot_protocol::annotation::split_frontmatter(s) {
        Some((header, body)) => (Some(header.to_string()), body.to_string()),
        None => (None, s.to_string()),
    }
}

/// Give up on a file's drift check after this many failed `file.parse`
/// attempts (initial fire + 2 retries). Retries are spaced by exponential
/// backoff (2s, 4s) — per review feedback, short enough that a capture
/// window converges, capped so a persistently-failing kernel gets exactly
/// two more chances per session, never a storm.
pub(in crate::ui) const FILE_PARSE_MAX_RETRIES: u32 = 3;

/// Derive a `.concept/`-relative target string for a tree node id. Returns
/// `None` when the node has no natural annotation target (root rows, unknown
/// id prefixes). The backend rejects `..` / absolute paths inside the
/// `target` so we keep this conservative — only the `files:` and `modules:`
/// prefixes today, both producing forward-slash paths.
pub(in crate::ui) fn node_id_to_concept_target(id: &str) -> Option<String> {
    if let Some(path) = id.strip_prefix("files:") {
        if path.is_empty() {
            None
        } else {
            Some(format!("files/{path}"))
        }
    } else if let Some(name) = id.strip_prefix("modules:") {
        if name.is_empty() {
            None
        } else {
            Some(format!("modules/{name}"))
        }
    } else {
        None
    }
}

impl State {
    /// If the selected tree row's annotation target differs from the last
    /// one we asked the backend about, fire a fresh `concept.read`. Called
    /// from `redraw` so cursor moves and event-driven tree updates both
    /// trigger refresh without each caller having to remember.
    pub(in crate::ui) fn maybe_fire_concept_read(&mut self) {
        let Some(row) = self.tree.rows.get(self.tree.selected) else {
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
        if self.concept_target_fired == target {
            // Cursor didn't move; but the cursored row might still need a
            // file.parse fired (first time we see it). Fall through to the
            // file-parse check below without re-firing concept.read.
        } else {
            if let Some(t) = target.as_ref() {
                let generation = self.next_concept_gen();
                if let Err(e) = self.send(crate::net::transport::OutgoingReq::ConceptRead {
                    target: t.clone(),
                    workspace_id: self.active_workspace_id.clone(),
                    generation,
                }) {
                    tracing::warn!(error = %e, target = %t, "drop concept.read request — channel closed");
                    return;
                }
            }
            self.concept_target_fired = target;
            // Clear stale cached result so the chrome doesn't keep showing
            // the previous node's annotation status while the new read is
            // in flight.
            self.concept = None;
            self.preview_concept = None;
        }
        // Drift check: ask the kernel for the file's ast_hash once per
        // distinct path the user has visited. The HashMap grows over the
        // session; phase-2 may add a TTL or eager sweep.
        if let Some(path) = files_path {
            // Retry gate for failed parses: re-arm the one-shot latch only
            // after an exponential backoff (`1u64 << n` with n=1,2 → 2s,
            // 4s), and stop entirely at the attempt cap — at the cap the
            // latch STAYS latched, so a storm is structurally impossible:
            // each re-arm buys exactly one fire (the latch re-inserts on
            // fire; the failure handler re-stamps the timestamp).
            if !self.file_ast_hashes.contains_key(&path) {
                if let Some(&(failed_at, attempts)) = self.file_parse_retry.get(&path) {
                    if attempts < FILE_PARSE_MAX_RETRIES
                        && failed_at.elapsed()
                            >= std::time::Duration::from_secs(1u64 << attempts.min(4))
                    {
                        self.file_parse_fired.remove(&path);
                    }
                }
            }
            if !self.file_ast_hashes.contains_key(&path)
                && self.file_parse_fired.insert(path.clone())
            {
                tracing::debug!(%path, label = %node_label, "→ file.parse for drift check");
                if let Err(e) = self.send(crate::net::transport::OutgoingReq::FileParse {
                    path: path.clone(),
                    workspace_id: self.active_workspace_id.clone(),
                }) {
                    tracing::warn!(error = %e, %path, "drop file.parse request — channel closed");
                    self.file_parse_fired.remove(&path);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A closing fence with no newline after it is the end of the file: the header keeps exactly what was
    /// there, so header plus body is the original and no final newline appears.
    #[test]
    fn concept_frontmatter_closing_fence_at_eof_preserves_bytes() {
        for s in [
            "---\ntarget: x\n---",
            "---\r\ntarget: x\r\n---",
            "---\ntarget: x\n---\nbody",
            "# no header",
        ] {
            let (h, b) = split_frontmatter(s);
            assert_eq!(h.unwrap_or_default() + &b, s);
        }
        assert_eq!(
            split_frontmatter("---\nt: x\n---").0.as_deref(),
            Some("---\nt: x\n---")
        );
        assert_eq!(
            split_frontmatter("---\nt: x\nbody"),
            (None, "---\nt: x\nbody".to_string())
        );
    }
}
