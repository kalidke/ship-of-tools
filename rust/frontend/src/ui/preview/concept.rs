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

/// Pull `synced_against: <value>` out of a markdown file's leading YAML
/// frontmatter. Accepts quoted (`"x"` / `'x'`) and bare values; trims
/// whitespace. Returns `None` when no frontmatter, no closing fence, or
/// the field isn't present. Matches the minimal parser Linux used on the
/// kernel side (`6864c93`) — full YAML is overkill here.
pub(in crate::ui) fn parse_synced_against(s: &str) -> Option<String> {
    let mut lines = s.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            return None;
        }
        let Some(rest) = trimmed.strip_prefix("synced_against:") else {
            continue;
        };
        let v = rest.trim();
        let unquoted = v
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(v);
        let unquoted = unquoted.trim();
        return if unquoted.is_empty() {
            None
        } else {
            Some(unquoted.to_string())
        };
    }
    None
}

/// Strip a leading YAML frontmatter block from an annotation. Matches the
/// jekyll-style `---\n…\n---\n` envelope; returns the original string
/// unchanged when no opening delimiter is on line 1. Tolerant of `\r\n`
/// line endings via `str::lines`.
pub(in crate::ui) fn strip_frontmatter(s: &str) -> String {
    split_frontmatter(s).1
}

/// Split a concept-file source into `(header, body)` where `header` is
/// the YAML frontmatter (including both `---` delimiters and the
/// trailing newline) or `None` when there is no frontmatter. The
/// concatenation `header.unwrap_or_default() + body` reproduces a
/// frontmatter-free file exactly and a frontmatter-bearing file
/// modulo a possibly-missing trailing newline after the closing
/// `---` (always preserved here when present in the input).
///
/// Edit mode uses this so the editable buffer is the body only —
/// frontmatter (target, target_kind, synced_against, authored_by,
/// references) renders as a read-only header above the edit area and
/// concatenation on save preserves it byte-perfect.
pub(in crate::ui) fn split_frontmatter(s: &str) -> (Option<String>, String) {
    let mut lines = s.split('\n');
    let Some(first) = lines.next() else {
        return (None, String::new());
    };
    if first.trim() != "---" {
        return (None, s.to_string());
    }
    let mut header_lines = vec![first.to_string()];
    let mut body_lines: Vec<&str> = Vec::new();
    let mut closed = false;
    for line in lines {
        if !closed {
            header_lines.push(line.to_string());
            if line.trim() == "---" {
                closed = true;
            }
        } else {
            body_lines.push(line);
        }
    }
    if !closed {
        return (None, s.to_string());
    }
    // `split('\n')` on a string ending with `\n` produces a trailing
    // empty element; rejoining with `\n` reproduces the original.
    let header = header_lines.join("\n");
    // Add the newline that separates header from body (it was the
    // `\n` after the closing `---` in the source).
    let header = header + "\n";
    let body = body_lines.join("\n");
    (Some(header), body)
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
                if let Err(e) = self.send(crate::transport::OutgoingReq::ConceptRead {
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
                if let Err(e) = self.send(crate::transport::OutgoingReq::FileParse {
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

    #[test]
    fn split_frontmatter_returns_header_and_body() {
        let s = "---\ntarget: x\nsynced_against: abc\n---\n# Body\n\nText.\n";
        let (h, b) = split_frontmatter(s);
        assert_eq!(
            h.as_deref(),
            Some("---\ntarget: x\nsynced_against: abc\n---\n")
        );
        assert_eq!(b, "# Body\n\nText.\n");
        // Round-trip: header + body == original.
        assert_eq!(h.unwrap() + &b, s);
    }

    #[test]
    fn split_frontmatter_no_header_returns_none() {
        let s = "# Just markdown\n\nNo frontmatter.\n";
        let (h, b) = split_frontmatter(s);
        assert_eq!(h, None);
        assert_eq!(b, s);
    }

    #[test]
    fn split_frontmatter_unterminated_treated_as_no_header() {
        let s = "---\ntarget: x\n# never closed\n";
        let (h, b) = split_frontmatter(s);
        assert_eq!(h, None);
        assert_eq!(b, s);
    }

    #[test]
    fn split_frontmatter_empty_body_after_header() {
        let s = "---\ntarget: x\n---\n";
        let (h, b) = split_frontmatter(s);
        assert_eq!(h.as_deref(), Some("---\ntarget: x\n---\n"));
        assert_eq!(b, "");
        assert_eq!(h.unwrap() + &b, s);
    }

    #[test]
    fn strip_frontmatter_removes_yaml_block() {
        // Trailing newline is preserved now — the new `split_frontmatter`
        // back-end uses `s.split('\n')` so the round-trip (header +
        // body) reproduces the source byte-perfect, which the edit
        // flow needs to keep `concept.write` payloads stable.
        let s = "---\ntarget: foo\nsynced_against: hash\n---\n# Body\n\nText.\n";
        assert_eq!(strip_frontmatter(s), "# Body\n\nText.\n");
    }

    #[test]
    fn strip_frontmatter_passthrough_when_no_block() {
        let s = "# Title\n\nNo frontmatter here.";
        assert_eq!(strip_frontmatter(s), s);
    }

    #[test]
    fn strip_frontmatter_passthrough_when_unterminated() {
        let s = "---\ntarget: foo\n# But no closing fence\n\nBody.";
        assert_eq!(strip_frontmatter(s), s);
    }

    #[test]
    fn strip_frontmatter_handles_empty_body() {
        let s = "---\ntarget: foo\n---\n";
        // Lines after the closing fence: empty trailing line. join("\n") = "".
        assert_eq!(strip_frontmatter(s), "");
    }

    #[test]
    fn parse_synced_against_bare_value() {
        let s = "---\ntarget: foo\nsynced_against: abc123\n---\n# Body\n";
        assert_eq!(parse_synced_against(s).as_deref(), Some("abc123"));
    }

    #[test]
    fn parse_synced_against_double_quoted() {
        let s = "---\nsynced_against: \"abc123\"\n---\n";
        assert_eq!(parse_synced_against(s).as_deref(), Some("abc123"));
    }

    #[test]
    fn parse_synced_against_single_quoted() {
        let s = "---\nsynced_against: 'abc123'\n---\n";
        assert_eq!(parse_synced_against(s).as_deref(), Some("abc123"));
    }

    #[test]
    fn parse_synced_against_missing_field() {
        let s = "---\ntarget: foo\nauthored_by: x\n---\n";
        assert_eq!(parse_synced_against(s), None);
    }

    #[test]
    fn parse_synced_against_no_frontmatter() {
        let s = "# Just markdown, no frontmatter\n";
        assert_eq!(parse_synced_against(s), None);
    }

    #[test]
    fn parse_synced_against_empty_value() {
        let s = "---\nsynced_against:\n---\n";
        assert_eq!(parse_synced_against(s), None);
    }

    #[test]
    fn parse_synced_against_field_in_body_ignored() {
        // The closing `---` ends scanning before we see this line.
        let s = "---\ntarget: foo\n---\nsynced_against: not-real\n";
        assert_eq!(parse_synced_against(s), None);
    }

    #[test]
    fn node_id_to_target_files_and_modules() {
        assert_eq!(
            node_id_to_concept_target("files:rust/foo.rs").as_deref(),
            Some("files/rust/foo.rs")
        );
        assert_eq!(
            node_id_to_concept_target("modules:Foo").as_deref(),
            Some("modules/Foo")
        );
        assert_eq!(node_id_to_concept_target("files:"), None);
        assert_eq!(node_id_to_concept_target("modules:"), None);
        assert_eq!(node_id_to_concept_target("unknown:Foo"), None);
    }
}
