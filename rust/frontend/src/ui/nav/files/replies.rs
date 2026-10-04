//! Replies to Files-mode file operations: file.delete, dir.create, upload acks and failures,
//! download progress.

use crate::ui::*;

impl State {
    pub(crate) fn on_file_delete_done(
        &mut self,
        node_id: String,
        result: crate::net::transport::FileDeleteResult,
    ) {
        // Ctrl+D delete round-trip: did this reply close out the
        // file we just asked the backend to trash? Late replies for
        // a stale request are ignored.
        let matches_delete =
            self.pending_deleted_node_id.as_deref() == Some(node_id.as_str());
        match result {
            crate::net::transport::FileDeleteResult::Ok {
                path,
                trashed,
                trash_path,
            } => {
                tracing::info!(%node_id, %path, trashed, ?trash_path, "file.delete ok");
                if matches_delete {
                    self.pending_deleted_node_id = None;
                    // Re-list the parent dir so the deleted row
                    // vanishes without a manual re-expand — same
                    // tree.children refresh the create path uses.
                    // TreeView reconciliation re-clamps the cursor.
                    let parent = parent_files_node_id(&node_id);
                    if let Err(e) =
                        self.send(crate::net::transport::OutgoingReq::TreeChildren {
                            parent_id: parent,
                            workspace_id: self.active_workspace_id.clone(),
                        })
                    {
                        tracing::warn!(error = %e,
                            "drop post-delete tree.children refresh");
                    }
                    let name = node_id
                        .rsplit(['/', ':'])
                        .next()
                        .unwrap_or(node_id.as_str());
                    self.status = match trash_path {
                        Some(tp) => format!("deleted · {name} → {tp}"),
                        None => format!("deleted · {name}"),
                    };
                    self.window.request_redraw();
                }
            }
            crate::net::transport::FileDeleteResult::Error { code, message } => {
                tracing::error!(%node_id, %code, %message, "file.delete failed");
                if matches_delete {
                    self.pending_deleted_node_id = None;
                    self.status = format!("delete failed · {code}: {message}");
                    self.window.request_redraw();
                }
            }
        }
    }

    pub(crate) fn on_dir_create_done(
        &mut self,
        node_id: String,
        result: crate::net::transport::DirCreateResult,
    ) {
        // Ctrl+N new-dir round-trip: does nothing unless
        // `node_id` is the pending create (a late reply for an
        // abandoned/superseded request is ignored).
        match result {
            crate::net::transport::DirCreateResult::Ok { path } => {
                tracing::info!(%node_id, %path, "dir.create ok");
                self.finish_pending_create(&node_id, CreateOutcome::Ok);
            }
            crate::net::transport::DirCreateResult::Error { code, message } => {
                tracing::error!(%node_id, %code, %message, "dir.create failed");
                let outcome = if code == "already_exists" {
                    CreateOutcome::AlreadyExists
                } else {
                    CreateOutcome::Error(&message)
                };
                self.finish_pending_create(&node_id, outcome);
            }
        }
    }

    pub(crate) fn on_file_download_progress(
        &mut self,
        dest: std::path::PathBuf,
        written: u64,
        total: u64,
        eof: bool,
    ) {
        let name = dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dest.display().to_string());
        if eof {
            self.status = format!("downloaded · {name} ({written} bytes)");
        } else {
            self.status = format!("download · {name} {written}/{total}");
        }
        self.window.request_redraw();
    }

    pub(crate) fn on_file_upload_ack(
        &mut self,
        event_host: HostKey,
        done: bool,
        final_name: Option<String>,
    ) {
        // ADR 0042 L2a codex review, item F: an ack must come
        // from the upload's OWN pinned host — a switch away
        // mid-upload leaves the transfer running in the
        // background, and its acks must keep driving THIS
        // upload rather than being ignored (event_host, not
        // active_host) or, worse, misapplied to whatever a
        // differently-hosted upload happens to be active now.
        if self.upload.as_ref().map(|u| &u.host) != Some(&event_host) {
            tracing::debug!(%event_host, "file.upload ack from a non-owning host — dropped");
            return;
        }
        if done {
            // Current file finished. Count it against the batch and
            // advance: `start_next_file` starts the next file, or —
            // when the queue is drained — finalizes with one listing
            // refresh + the aggregate status.
            self.upload = None;
            if let Some(b) = self.upload_batch.as_mut() {
                b.done_files += 1;
            }
            self.start_next_file();
            self.window.request_redraw();
        } else {
            // The chunk-0 ack returns the backend's resolved name
            // (sanitized + de-duped, e.g. `report (1).csv`). Adopt it
            // so chunks 1..N target that same file.
            if let (Some(fname), Some(up)) = (final_name, self.upload.as_mut()) {
                up.name = fname;
            }
            // Flow control: ack of chunk N → send chunk N+1.
            self.send_next_upload_chunk();
        }
    }

    pub(crate) fn on_file_transfer_failed(
        &mut self,
        event_host: HostKey,
        op: &'static str,
        message: String,
    ) {
        if op == "upload" {
            // ADR 0042 L2a codex review, item F: a failure
            // from a non-owning host must not abort THIS
            // upload's batch — e.g. a stray failure on a host
            // this FE has since switched away from. Falls
            // back to the batch's own pinned host in the
            // narrow between-files window where `self.upload`
            // is momentarily `None` but the batch is still
            // live (so a legitimate same-host failure isn't
            // dropped just because no file is mid-flight).
            let owner = self
                .upload
                .as_ref()
                .map(|u| u.host.clone())
                .or_else(|| self.upload_batch.as_ref().map(|b| b.host.clone()));
            if owner.as_ref() != Some(&event_host) {
                tracing::debug!(%event_host, ?owner,
                    "file.upload failure from a non-owning host — dropped");
                return;
            }
            // A backend upload failure aborts the whole batch — the
            // remaining files are dropped rather than uploaded into
            // an ambiguous state.
            self.upload = None;
            self.upload_batch = None;
        }
        self.status = format!("{op} failed · {message}");
        self.window.request_redraw();
    }
}
