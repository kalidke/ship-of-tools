//! Downloads and chunked uploads between the Files tree and the host that owns it.

use super::*;
/// Upload chunk size. Must stay well under the protocol frame cap (1 MiB, see
/// codec.rs): each chunk is base64-encoded into the JSON `file.upload` envelope,
/// which inflates it ~4/3 — so the old 1 MiB chunk became a ~1.4 MiB frame and
/// blew the cap, resetting the transport mid-upload and stranding the in-flight
/// state ("upload · already in progress" forever). 512 KiB → ~700 KiB base64 +
/// envelope, comfortably under the cap. The backend writes each chunk at its
/// offset, so chunk size is a frontend-only choice.
pub(super) const UPLOAD_CHUNK: usize = 512 * 1024;

impl State {
    /// `d` in NavTree: download the cursored file row to the local downloads
    /// dir (OS-independent — `Settings::download_dir()`), non-clobbering. The
    /// transport streams chunks and writes the dest as they arrive. Directory
    /// rows are a no-op (download a file, not a folder).
    pub(in crate::ui) fn start_download(&mut self) {
        let is_dir = self
            .nav.tree()
            .rows()
            .get(self.nav.tree().selected())
            .map(|r| r.node.kind == "dir")
            .unwrap_or(false);
        if is_dir {
            self.status.set("download · select a file, not a folder".to_string());
            self.window.request_redraw();
            return;
        }
        let Some(abs) = self.cursored_files_path() else {
            self.status.set("download · no file under cursor".to_string());
            self.window.request_redraw();
            return;
        };
        let basename = abs
            .rsplit(['/', '\\'])
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("download.bin")
            .to_string();
        let dir = self.settings.download_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!(error = %e, dir = %dir.display(), "download: cannot create downloads dir");
            self.status.set(format!("download failed · mkdir {}: {e}", dir.display()));
            self.window.request_redraw();
            return;
        }
        let dest = crate::ui::nav::files::download::non_clobbering_path(&dir, &basename);
        if let Err(e) = self.send(crate::net::transport::OutgoingReq::FileDownload {
            path: abs,
            dest: dest.clone(),
        }) {
            tracing::warn!(error = %e, "drop file.download — channel closed");
            return;
        }
        self.status.set(format!("download · {basename} → {}", dest.display()));
        self.window.request_redraw();
    }

    /// `u` in NavTree: pick one or more local files via the native OS dialog and
    /// upload them to the cursored nav folder (the dir itself for a dir row, else
    /// the cursored file's parent dir). The picker is multi-select; the files
    /// upload sequentially (see `UploadBatch`). Opens the first file and sends
    /// its first chunk; subsequent chunks/files are pumped by `FileUploadAck`.
    pub(in crate::ui) fn start_upload(&mut self) {
        if self.file_ops.uploading() {
            self.status.set("upload · already in progress".to_string());
            self.window.request_redraw();
            return;
        }
        let (is_dir, node_id) = match self.nav.tree().rows().get(self.nav.tree().selected()) {
            Some(r) => (r.node.kind == "dir", r.node.id.clone()),
            None => (false, String::new()),
        };
        let Some(abs) = self.cursored_files_path() else {
            self.status.set("upload · no target folder for this row".to_string());
            self.window.request_redraw();
            return;
        };
        // Destination dir + its tree node id: a dir row is the target itself,
        // a file row targets its parent dir. `dir_node_id` lets us refresh the
        // nav listing when the upload completes so the new files appear.
        let (dir, dir_node_id) = if is_dir {
            (abs, node_id)
        } else {
            let dir = match abs.rsplit_once(['/', '\\']) {
                Some((parent, _)) if !parent.is_empty() => parent.to_string(),
                _ => {
                    self.status.set("upload · cannot resolve parent folder".to_string());
                    self.window.request_redraw();
                    return;
                }
            };
            (dir, parent_files_node_id(&node_id))
        };
        // Native OS picker (rfd), multi-select: Win common dialog / macOS
        // NSOpenPanel / Linux GTK-or-XDG-portal. Blocking — the app waits on
        // the modal dialog. `pick_files` returns every selected path.
        let picked = rfd::FileDialog::new()
            .set_title("Upload file(s) to the cursored folder")
            .pick_files();
        let files: std::collections::VecDeque<std::path::PathBuf> = match picked {
            Some(v) if !v.is_empty() => v.into_iter().collect(),
            _ => {
                self.status.set("upload · cancelled".to_string());
                self.window.request_redraw();
                return;
            }
        };
        self.file_ops.begin_batch(
            self.selected.host().clone(), self.selected.workspace_id().clone(), dir, dir_node_id, files,
        );
        self.start_next_file();
    }

    /// Pop the next file from the active `upload_batch` and begin uploading it,
    /// or — when the queue is drained — finalize the batch (one listing refresh
    /// + aggregate status). Called by `start_upload` to kick off the first file
    /// and by the `done` ack handler to advance to the next. A no-op if no batch
    /// is active.
    pub(in crate::ui) fn start_next_file(&mut self) {
        // Pull the next path + the shared destination out of the batch.
        let (local, host, workspace_id, dir, dir_node_id) = match self.file_ops.next_upload() {
            None => return,
            Some(NextUpload::Start { local, host, workspace_id, dir, dir_node_id }) => {
                (local, host, workspace_id, dir, dir_node_id)
            }
            Some(NextUpload::Finished { host, workspace_id, dir, dir_node_id, done, total }) => {
                // Queue drained — the whole batch is complete. Refresh the
                // destination listing once so the new files appear without a
                // manual re-expand, then report the aggregate.
                self.refresh_upload_dir(&host, workspace_id, &dir_node_id);
                self.status.set(if total == 1 {
                    format!("uploaded · {done} file → {dir}")
                } else {
                    format!("uploaded · {done}/{total} files → {dir}")
                });
                self.window.request_redraw();
                return;
            }
        };
        let name = local
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "upload.bin".to_string());
        let opened = std::fs::File::open(&local).and_then(|f| {
            let total = f.metadata()?.len();
            Ok((f, total))
        });
        let (file, total) = match opened {
            Ok(ft) => ft,
            Err(e) => {
                tracing::warn!(error = %e, local = %local.display(), "upload: open local file failed");
                self.status.set(format!("upload failed · open {}: {e}", local.display()));
                // A local-open failure aborts the rest of the batch: already
                // uploaded files stay (refresh so they show), the remainder is
                // dropped. Predictable partial state over silent skipping.
                self.file_ops.drop_batch();
                self.refresh_upload_dir(&host, workspace_id, &dir_node_id);
                self.window.request_redraw();
                return;
            }
        };
        self.file_ops.start_file(file, dir, host, name, total);
        self.send_next_upload_chunk();
    }

    /// Refresh a destination directory's nav listing after upload activity so
    /// newly written files appear without a manual re-expand. No-op for an
    /// empty node id (unresolved parent). Routes via the PINNED `host`/
    /// `workspace_id` the upload started with (ADR 0042 L2a codex review,
    /// item F) — not `active_host`/`active_workspace_id`, which may have
    /// moved on by the time the batch completes.
    fn refresh_upload_dir(&self, host: &HostKey, workspace_id: Option<String>, dir_node_id: &str) {
        if dir_node_id.is_empty() {
            return;
        }
        if let Err(e) = self.send_to(
            host,
            crate::net::transport::OutgoingReq::TreeChildren {
                parent_id: dir_node_id.to_string(),
                workspace_id,
            },
        ) {
            tracing::warn!(error = %e, "drop post-upload tree.children refresh");
        }
    }

    /// Read the next `UPLOAD_CHUNK` from the in-flight upload's local file and
    /// send it as a `file.upload` chunk. Called once to kick off the upload and
    /// again on each non-`done` ack. A read error or closed channel aborts.
    pub(in crate::ui) fn send_next_upload_chunk(&mut self) {
        let Some(read) = self.file_ops.read_chunk() else {
            return;
        };
        match read {
            Ok(UploadChunk { host, dir, name, total, sent, offset, eof, bytes }) => {
                // ADR 0042 L2a codex review, item F: routed to the
                // PINNED owner, not active_host — a workspace/host
                // switch mid-upload must not redirect the remaining
                // chunks to a different daemon.
                if let Err(e) = self.send_to(
                    &host,
                    crate::net::transport::OutgoingReq::FileUpload {
                        dir,
                        name: name.clone(),
                        offset,
                        total,
                        eof,
                        bytes,
                    },
                ) {
                    tracing::warn!(error = %e, "drop file.upload chunk — channel closed");
                    self.file_ops.abort_upload();
                } else {
                    // Multi-file batches prefix `file i/N ·`; a lone file omits it.
                    let prefix = self.file_ops.progress_prefix();
                    self.status.set(format!("upload · {prefix}{name} {sent}/{total}"));
                }
            }
            Err((name, e)) => {
                tracing::warn!(error = %e, "upload: local read failed");
                self.status.set(format!("upload failed · read {name}: {e}"));
                self.file_ops.abort_upload();
            }
        }
        self.window.request_redraw();
    }
}

#[cfg(test)]
#[path = "transfer_tests.rs"]
mod tests;
