//! The in-pane editor's state (`EditState`) and the rebuild of its preview buffer.

use crate::ui::*;

/// Active concept-annotation edit. `None` when the preview pane is in
/// read-only view mode (the default); `Some` when the user pressed `e`
/// on a cursored annotation. Carries enough context to fire a
/// well-formed `concept.write` and to discriminate a stale-write
/// refusal from a fresh-edit refusal.
#[derive(Clone)]
pub(in crate::ui) struct EditState {
    /// Concept target being edited (`files/path/to.rs`,
    /// `modules/Foo`, ...). Matched against `ConceptWriteDone.target`
    /// when the write reply lands.
    pub(in crate::ui) target: String,
    /// `synced_against` AST hash captured at edit-enter time. Sent as
    /// `expected_ast_hash` on every save so the backend gates the
    /// write with optimistic concurrency (Linux's `4ebca35`).
    pub(in crate::ui) expected_ast_hash: Option<String>,
    /// YAML frontmatter (`---\n...\n---\n`) captured at edit-enter,
    /// or `None` when the source had none. Rendered above the
    /// editable region as a read-only header; concatenated with
    /// `buf.body()` on save so the on-disk file's frontmatter
    /// survives round-trip byte-perfect.
    pub(in crate::ui) header: Option<String>,
    /// Body captured when entering edit mode — used to detect whether
    /// the buffer is dirty (the discard-confirm modal cares; save
    /// snaps this to the current body so post-save edits start
    /// clean again).
    pub(in crate::ui) original: String,
    /// Live editable text + cursor. Holds *body only* — frontmatter
    /// is in `header`.
    pub(in crate::ui) buf: EditBuffer,
    /// True while the discard-confirm modal is up (Esc pressed on a
    /// dirty buffer). `y` confirms discard, `n` / Esc / any other key
    /// dismisses and returns to editing.
    pub(in crate::ui) confirm_discard: bool,
    /// True while the stale-write banner is up (a `concept.write`
    /// returned `stale_write` because the on-disk `synced_against`
    /// no longer matches our `expected_ast_hash`). `r` re-reads the
    /// file and replaces the buffer with the on-disk content
    /// (discarding edits); `k` dismisses the banner and lets the
    /// user keep editing (the next save will fail again until they
    /// reload or the file changes back). Never auto-clobber.
    pub(in crate::ui) stale_banner: bool,
    /// `Some(files:<relpath>)` when editing a general source file (vs a
    /// `.concept/` annotation, where this is `None`). Selects the save path:
    /// `Some` → `file.write` keyed on this node id; `None` → `concept.write`
    /// keyed on `target`.
    pub(in crate::ui) file_node_id: Option<String>,
    /// Content version from the file's `file.read`, sent back as
    /// `file.write`'s `expected_version` for optimistic concurrency. `None`
    /// for concept edits (they gate on `expected_ast_hash`).
    pub(in crate::ui) file_version: Option<String>,
}

impl EditState {
    pub(in crate::ui) fn is_dirty(&self) -> bool {
        self.buf.body() != self.original
    }

    /// Reassemble the full on-disk content from `header` + the live
    /// buffer body — sent as the payload of every `concept.write`.
    pub(in crate::ui) fn full_content(&self) -> String {
        match &self.header {
            Some(h) => h.clone() + self.buf.body(),
            None => self.buf.body().to_string(),
        }
    }
}

impl State {
    /// Rebuild `preview_edit` from the active edit buffer. Layout:
    ///   - read-only frontmatter header (if any), prefixed line-by-line
    ///     with `│ ` so the user reads it as a sidebar
    ///   - blank line separator
    ///   - editable body with `█` injected at the cursor byte
    ///   - blank line + status footer ("modified" if dirty; modal
    ///     prompt when `confirm_discard` is up)
    /// Called whenever a key mutates the buffer; cheap enough for a
    /// few-KB annotation. When `edit_state` is `None`, clears the
    /// preview so the next render falls back to the read-only path.
    pub(in crate::ui) fn rebuild_edit_preview(&mut self) {
        let Some(edit) = self.edit_state.as_ref() else {
            self.preview_edit = None;
            return;
        };
        // Compose styled spans: header, body (cursor block + optional
        // selection tint), footer. Concatenated text is what renders; the
        // `bool` flags the selected run so `new_plain_spans` tints it.
        let mut spans: Vec<(String, bool)> = Vec::new();
        if let Some(header) = &edit.header {
            let mut h = String::new();
            for line in header.lines() {
                h.push_str("│ ");
                h.push_str(line);
                h.push('\n');
            }
            h.push('\n');
            spans.push((h, false));
        }
        let body = edit.buf.body();
        let cur = edit.buf.cursor();
        const CURSOR: &str = "\u{2588}";
        let has_sel = edit.buf.selection_range().is_some();
        match edit.buf.selection_range() {
            // Cursor sits at one end of the range (a = start, b = end). Place
            // the cursor block on its side; tint the selected run amber.
            Some((a, b)) if cur <= a => {
                spans.push((body[..a].to_string(), false));
                spans.push((CURSOR.to_string(), false));
                spans.push((body[a..b].to_string(), true));
                spans.push((body[b..].to_string(), false));
            }
            Some((a, b)) => {
                spans.push((body[..a].to_string(), false));
                spans.push((body[a..b].to_string(), true));
                spans.push((CURSOR.to_string(), false));
                spans.push((body[b..].to_string(), false));
            }
            None => {
                spans.push((body[..cur].to_string(), false));
                spans.push((CURSOR.to_string(), false));
                spans.push((body[cur..].to_string(), false));
            }
        }
        // Footer: blank line + status. Modal overrides everything — when the
        // user is confirming, that's the only thing they should read.
        // Priority: stale > discard-confirm > selection > modified > clean.
        let mut footer = String::from("\n\n");
        if edit.stale_banner {
            footer.push_str(&format!(
                "── STALE: file changed on disk. {} reload (discard edits) · {} keep editing ──",
                self.bindings.labels(Action::StaleReload), self.bindings.labels(Action::StaleKeep)));

        } else if edit.confirm_discard {
            footer.push_str(&format!(
                "── DISCARD UNSAVED EDITS? {} discard · another key keeps editing ──",
                self.bindings.labels(Action::DiscardConfirm)));

        } else if has_sel {
            footer.push_str(&format!("── edit mode · selection · {} copy · {} cut · {} save ──",
                self.bindings.first_label(Action::EditCopy), self.bindings.first_label(Action::EditCut), self.bindings.first_label(Action::EditSave)));
        } else if edit.is_dirty() {
            footer.push_str(&format!("── edit mode · modified · {} save · {} discard ──",
                self.bindings.first_label(Action::EditSave), self.bindings.first_label(Action::Cancel)));
        } else {
            footer.push_str(&format!("── edit mode · clean · {} save · {} exit ──",
                self.bindings.first_label(Action::EditSave), self.bindings.first_label(Action::Cancel)));
        }
        spans.push((footer, false));
        let width = self.concept_rect_px.w.max(1.0);
        let scale = self.scale * self.text_scale_mult;
        self.preview_edit = Some(MarkdownPreview::new_plain_spans(
            self.text.font_system_mut(),
            &spans,
            width,
            scale,
        ));
    }
}
