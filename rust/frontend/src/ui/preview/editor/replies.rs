//! Editor replies: file.read opens the editor on a file; file.write reports the save, the conflict
//! or the failure.

use crate::ui::*;

impl State {
    pub(crate) fn on_file_read(
        &mut self,
        node_id: String,
        exists: bool,
        content: String,
        version: String,
    ) {
        // Edit-enter (or stale-reload) for a general file: when this
        // reply matches the pending request and the file exists, open
        // the editor on it (replacing any prior edit_state — that's
        // how `r` reload-discards). Non-pending replies are ignored.
        if self.pending_file_edit.as_deref() == Some(node_id.as_str()) {
            self.pending_file_edit = None;
            if exists {
                self.edit_state = Some(EditState {
                    target: node_id.clone(),
                    expected_ast_hash: None,
                    header: None,
                    original: content.clone(),
                    buf: EditBuffer::new(content),
                    confirm_discard: false,
                    stale_banner: false,
                    file_node_id: Some(node_id.clone()),
                    file_version: Some(version),
                });
                self.rebuild_edit_preview();
                tracing::info!(%node_id, "entered file edit mode");
            } else {
                tracing::warn!(%node_id, "file.read: not found — not entering edit");
            }
        } else {
            tracing::debug!(%node_id, exists, "file.read reply (no pending edit)");
        }
    }

    pub(crate) fn on_file_write_done(
        &mut self,
        node_id: String,
        result: crate::transport::FileWriteResult,
    ) {
        // Reconcile only when the reply targets the active file edit
        // (late replies for an abandoned edit are ignored).
        let matches_active = self
            .edit_state
            .as_ref()
            .and_then(|e| e.file_node_id.as_deref())
            == Some(node_id.as_str());
        match result {
            crate::transport::FileWriteResult::Ok { path, version } => {
                tracing::info!(%node_id, %path, %version, "file.write ok");
                if matches_active {
                    if let Some(edit) = self.edit_state.as_mut() {
                        // Snap the dirty baseline + adopt the new
                        // version so further edits start clean and
                        // the next save's conflict check is current.
                        edit.original = edit.buf.body().to_string();
                        edit.file_version = Some(version);
                    }
                    // Surface the save (peer report 2026-08-19):
                    // this arm used to set no status and request
                    // no redraw, so an active-edit save was
                    // SILENT — and the snapped dirty baseline
                    // didn't repaint until some other event came
                    // along. Same toast shape as the create path.
                    let name = node_id
                        .rsplit(['/', ':'])
                        .next()
                        .unwrap_or(node_id.as_str());
                    self.status = format!("saved · {name}");
                    self.window.request_redraw();
                }
                // Ctrl+N new-file round-trip: does nothing unless
                // `node_id` is the pending create.
                self.finish_pending_create(&node_id, CreateOutcome::Ok);
            }
            crate::transport::FileWriteResult::Conflict {
                current_version, ..
            } => {
                tracing::warn!(%node_id, %current_version, "file.write refused: conflict");
                if matches_active {
                    if let Some(edit) = self.edit_state.as_mut() {
                        edit.stale_banner = true;
                    }
                    self.rebuild_edit_preview();
                    // The banner lives in the rebuilt preview, but
                    // nothing here scheduled a paint for it — pair
                    // it with a status line and an explicit redraw
                    // so the refusal is visible immediately.
                    self.status =
                        "save refused · file changed on disk (reload to update)"
                            .to_string();
                    self.window.request_redraw();
                }
                // A conflicting write means the name collided on
                // disk (a file the tree didn't list yet) when
                // this was a Ctrl+N create.
                self.finish_pending_create(&node_id, CreateOutcome::AlreadyExists);
            }
            crate::transport::FileWriteResult::Error { code, message } => {
                tracing::error!(%node_id, %code, %message, "file.write failed");
                if matches_active {
                    // A FAILED save of the user's live edit was
                    // completely silent outside the log — the
                    // most dangerous of the three outcomes (the
                    // user walks away believing it saved). The
                    // edit buffer stays as-is so nothing is lost;
                    // say so.
                    self.status =
                        format!("SAVE FAILED · {message} (edit kept in buffer)");
                    self.window.request_redraw();
                }
                self.finish_pending_create(&node_id, CreateOutcome::Error(&message));
            }
        }
    }
}
