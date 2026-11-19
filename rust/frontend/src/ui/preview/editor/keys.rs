//! The editor's keys, entering edit mode from the preview, and re-shaping the edit buffer.

use crate::ui::*;
use std::ops::ControlFlow::{self, Break, Continue};
use crate::ui::input::keypress::KeyPress;
use super::state::EditorKey;

pub(in crate::ui) fn editor_key(state: &mut State, key: KeyPress<'_>) -> ControlFlow<()> {
    // Edit mode hijacks all keys — typing into the
    // editable annotation body takes precedence
    // over scroll keys. Ctrl+S saves; Esc discards
    // (commit 3 adds the dirty-confirm modal).
    // 1/3-pane step here too so the editor's
    // cursor doesn't blow past visible context
    // on PgUp/PgDn.
    let page_rows = (state.pane_rects.preview.height as usize / 3).max(1);
    let Some(outcome) = state.editor.key(key, page_rows) else {
        return Continue(());
    };
    match outcome {
        EditorKey::DiscardAnswered | EditorKey::ConfirmDiscard => {}
        EditorKey::ReloadFile(node_id) => {
            let ws = state.selected.workspace_id().clone();
            if let Err(e) = state.send(OutgoingReq::FileRead {
                node_id,
                workspace_id: ws,
            }) {
                tracing::warn!(error = %e,
                    "drop file.read for stale reload");
            }
            state.rebuild_edit_preview();
        }
        EditorKey::ReloadConcept(target) => {
            let ws = state.selected.workspace_id().clone();
            let generation = state.next_concept_gen();
            if let Err(e) = state.send(OutgoingReq::ConceptRead {
                target,
                workspace_id: ws,
                generation,
            }) {
                tracing::warn!(error = %e,
                    "drop concept.read for stale reload");
            }
            state.rebuild_edit_preview();
        }
        EditorKey::Closed => {
            // Re-fetch the underlying preview so
            // it reflects content saved during
            // this edit session — the cached
            // preview is the pre-edit render.
            // Clearing the fired-guard lets
            // maybe_fire_preview re-issue
            // preview.get for the still-selected
            // node.
            state.preview_requests.forget_fired();
            state.maybe_fire_preview();
        }
        EditorKey::SaveFile { node_id, content, expected_version } => {
            let ws = state.selected.workspace_id().clone();
            if let Err(e) = state.send(OutgoingReq::FileWrite {
                node_id,
                content,
                expected_version,
                workspace_id: ws,
            }) {
                tracing::warn!(error = %e,
                    "drop file.write — channel closed");
            }
            state.rebuild_edit_preview();
        }
        EditorKey::SaveConcept { target, content, expected_ast_hash } => {
            let ws = state.selected.workspace_id().clone();
            if let Err(e) = state.send(OutgoingReq::ConceptWrite {
                target,
                content,
                expected_ast_hash,
                workspace_id: ws,
            }) {
                tracing::warn!(error = %e,
                    "drop concept.write — channel closed");
            }
            state.rebuild_edit_preview();
        }
        // Cursor moves rebuild too: the buffer's selection tint and
        // footer follow the edit, and the next vertical step reads the
        // rows this shaping gives.
        EditorKey::StaleAnswered => state.rebuild_edit_preview(),
        // The cursor moved or the text changed: the view follows it.
        EditorKey::Edited => {
            state.rebuild_edit_preview();
            state.follow_edit_cursor();
        }
    }
    state.window.request_redraw();
    Break(())
}

pub(in crate::ui) fn enter_editor(state: &mut State) {
    // Concept-annotation edit takes priority when one
    // is loaded for the cursored node (content is
    // already in the concept slot).
    let mut entered = false;
    if let (Some(target), Some(info)) =
        (state.preview_requests.concept_target().map(str::to_owned), state.concept.info())
    {
        if info.target() == target && info.exists() {
            state.editor.enter_concept(target, info.synced_against().map(str::to_owned), info.content());
            state.rebuild_edit_preview();
            entered = true;
        }
    }
    // Otherwise, if the preview is showing a general
    // file, edit the file itself: fetch its raw text
    // via file.read and enter edit mode when the reply
    // lands (see the FileRead handler). The editor's armed
    // file matches the reply to this request.
    if !entered && !state.editor.is_editing() {
        if let Some(node_id) = state.shown_target().map(str::to_owned) {
            if node_id.starts_with("files:") {
                let ws = state.selected.workspace_id().clone();
                if let Err(e) = state.send(OutgoingReq::FileRead {
                    node_id: node_id.clone(),
                    workspace_id: ws,
                }) {
                    tracing::warn!(error = %e, "drop file.read for edit-enter");
                } else {
                    state.editor.arm_file(node_id);
                }
            }
        }
    }
}

impl State {
    /// Scroll the edit view just far enough to show the cursor's row (the pane's height as the scroll clamp reads it),
    /// at once: an eased follow leaves the row of a held key below the pane for most of its frames.
    pub(in crate::ui) fn follow_edit_cursor(&mut self) {
        let (now, length) = (std::time::Instant::now(), std::time::Duration::ZERO);
        let visible = self.concept.rect().h - crate::ui::render::text::EXTRA_TOP_PAD_PX;
        self.editor.follow_cursor(&mut self.shaped, visible, now, length);
    }

    /// The cursor's bar over the edit view, px on screen, in the pane `pane` scrolled `scroll_px`, clipped as the
    /// text is (`rows::bar`); none where nothing of it shows or the cursor has no place in the layout.
    pub(in crate::ui) fn edit_cursor_rect(&self, pane: ScreenRect, scroll_px: f32) -> Option<ScreenRect> {
        let clip_bottom = self.editor.shaped()?.clip_bottom(pane, scroll_px);
        super::rows::bar(self.editor.cursor_place()?, pane, scroll_px, clip_bottom)
    }

    /// Re-shape the edit buffer when the pane's width or the text scale is no longer the one it was shaped for (a
    /// window resize, a drawer opening, a zoom): the frame calls it after the pane is laid out and before the text is
    /// drawn, so the screen and the next key's rows are of the pane's own width.
    pub(in crate::ui) fn reshape_edit_to_pane(&mut self) {
        let width = self.concept.rect().w;
        let scale = self.canvas.scale() * self.canvas.text_scale_mult();
        if !self.editor.shaped_for(width, scale) {
            self.rebuild_edit_preview();
        }
    }

    /// Re-shape the edit preview from the active edit buffer. Called
    /// whenever a key mutates the buffer; cheap enough for a
    /// few-KB annotation. Without an edit it clears the shaped
    /// buffer so the next render falls back to the read-only path.
    pub(in crate::ui) fn rebuild_edit_preview(&mut self) {
        if self.editor.is_editing() { self.shaped.clear_selection(); }
        let width = self.concept.rect().w.max(1.0);
        let scale = self.canvas.scale() * self.canvas.text_scale_mult();
        let canvas = &mut self.canvas;
        self.editor.reshape(&self.bindings, |spans| {
            canvas.shape(|fs| MarkdownPreview::new_plain_spans(fs, spans, width, scale))
        });
    }
}

#[cfg(test)]
#[path = "keys_native_fixture_tests.rs"]
mod keys_native_fixture_tests;
