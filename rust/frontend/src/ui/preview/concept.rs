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

/// The annotation target a `files:` node id of a `.concept/` file stands for (`files:.concept/files/src/x.jl.md`
/// is `files/src/x.jl`); `None` for any other node.
pub(in crate::ui) fn annotation_target_of(node_id: &str) -> Option<String> {
    let rel = node_id.strip_prefix("files:.concept/")?.strip_suffix(".md")?;
    (!rel.is_empty()).then(|| rel.to_string())
}

type Hashes = std::collections::HashMap<String, String>;
type Fired = std::collections::HashSet<String>;
type Retries = std::collections::HashMap<String, (std::time::Instant, u32)>;

/// Forget one file's drift check: its hash, its one-shot parse latch and its failure record. The next
/// `maybe_fire_concept_read` with the cursor on it parses it again; no other file is touched.
pub(in crate::ui) fn forget_drift(path: &str, hashes: &mut Hashes, fired: &mut Fired, retry: &mut Retries) {
    hashes.remove(path);
    fired.remove(path);
    retry.remove(path);
}

/// Forget every file's drift check, failure records included. The cache lasts only while this window receives the workspace's changes, so a
/// workspace switch and a reconnect clear it (the daemon sends no `preview.changed` for a row that is not active
/// and replays none).
pub(in crate::ui) fn clear_drift(hashes: &mut Hashes, fired: &mut Fired, retry: &mut Retries) {
    hashes.clear();
    fired.clear();
    retry.clear();
}

/// True when the file's hash is known and differs from the annotation's `synced_against`.
pub(in crate::ui) fn drift_is_stale(hashes: &Hashes, path: &str, synced: &str) -> bool {
    hashes.get(path).is_some_and(|h| h != synced)
}

/// The drift check's fire gate for the cursored `.jl` path: true (and the latch taken) when a `file.parse` must go
/// out now. A failed parse re-arms the latch only after an exponential backoff (`1u64 << n` with n=1,2 → 2s, 4s)
/// and not at all at the attempt cap, where the latch STAYS latched, so a storm is structurally impossible: each
/// re-arm buys exactly one fire (the failure handler re-stamps the timestamp).
pub(in crate::ui) fn take_parse_fire(path: &str, hashes: &Hashes, fired: &mut Fired, retry: &Retries) -> bool {
    if hashes.contains_key(path) {
        return false;
    }
    if let Some(&(failed_at, attempts)) = retry.get(path) {
        if attempts < FILE_PARSE_MAX_RETRIES
            && failed_at.elapsed() >= std::time::Duration::from_secs(1u64 << attempts.min(4))
        {
            fired.remove(path);
        }
    }
    fired.insert(path.to_string())
}

/// Install a `file.parse` reply's hash only while the path is still latched: a forget (the file changed) clears
/// the latch, so a reply read before the edit is dropped. The kernel serves one request at a time, so a newer
/// fire's reply follows it.
pub(in crate::ui) fn install_parsed_hash(
    path: &str,
    hash: String,
    hashes: &mut Hashes,
    fired: &Fired,
    retry: &mut Retries,
) -> bool {
    if !fired.contains(path) {
        return false;
    }
    retry.remove(path);
    hashes.insert(path.to_string(), hash);
    true
}

/// What a `preview.changed` for a `files:` node means to the drift check.
#[derive(Debug, PartialEq)]
pub(in crate::ui) enum ChangeEffect<'a> {
    /// The cursored target's annotation was rewritten: read it again.
    ReadAnnotationAgain,
    /// A file's bytes changed: forget its drift check.
    ForgetFile(&'a str),
    /// Another annotation, or not a file: nothing cached depends on it.
    Nothing,
}

pub(in crate::ui) fn change_effect<'a>(node_id: &'a str, concept_target_fired: Option<&str>) -> ChangeEffect<'a> {
    if let Some(target) = annotation_target_of(node_id) {
        return if concept_target_fired == Some(target.as_str()) {
            ChangeEffect::ReadAnnotationAgain
        } else {
            ChangeEffect::Nothing
        };
    }
    match node_id.strip_prefix("files:") {
        Some(path) => ChangeEffect::ForgetFile(path),
        None => ChangeEffect::Nothing,
    }
}

impl State {
    /// A `preview.changed` for `node_id` (a `files:` id): drop the changed file's cached drift check so the next
    /// view re-parses it, and re-read the cursored annotation when it is the one that changed (a reconcile
    /// rewrote its `synced_against`). Per file; hashing stays lazy, on view. Covered only by the native window
    /// harness: this one call is not reached by a unit test.
    pub(in crate::ui) fn forget_changed_file(&mut self, node_id: &str) {
        match change_effect(node_id, self.concept_target_fired.as_deref()) {
            ChangeEffect::ReadAnnotationAgain => self.concept_target_fired = None,
            ChangeEffect::ForgetFile(path) => forget_drift(
                path,
                &mut self.file_ast_hashes,
                &mut self.file_parse_fired,
                &mut self.file_parse_retry,
            ),
            ChangeEffect::Nothing => {}
        }
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
        // Drift check: ask the kernel for the file's ast_hash once per view of a path whose hash this window does not
        // hold. The cache is lazy (no sweep) and per file: a change to the file, a workspace switch or a reconnect
        // drops it (`forget_changed_file`, `clear_drift`).
        if let Some(path) = files_path {
            if take_parse_fire(&path, &self.file_ast_hashes, &mut self.file_parse_fired, &self.file_parse_retry) {
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
#[path = "concept_tests.rs"]
mod tests;
