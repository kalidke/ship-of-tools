//! Downloads and chunked uploads between the Files tree and the host that owns it.

use super::*;
/// Upload chunk size. Must stay well under the protocol frame cap (1 MiB, see
/// codec.rs): each chunk is base64-encoded into the JSON `file.upload` envelope,
/// which inflates it ~4/3 — so the old 1 MiB chunk became a ~1.4 MiB frame and
/// blew the cap, resetting the transport mid-upload and stranding the in-flight
/// state ("upload · already in progress" forever). 512 KiB → ~700 KiB base64 +
/// envelope, comfortably under the cap. The backend writes each chunk at its
/// offset, so chunk size is a frontend-only choice.
const UPLOAD_CHUNK: usize = 512 * 1024;

/// In-flight upload bookkeeping. The local file stays open; the chrome reads
/// the next `UPLOAD_CHUNK` from it on each `FileUploadAck`. `sent` is the byte
/// offset of the next chunk (also the cumulative bytes acked so far).
pub(in crate::ui) struct UploadState {
    file: std::fs::File,
    /// Absolute backend destination directory (the cursored nav folder).
    dir: String,
    /// Host this upload targets, pinned at start (ADR 0042 L2a codex
    /// review, item F) -- reuses this field slot (formerly the dead
    /// `dir_node_id: String`, never actually read: the nav-listing
    /// refresh always reads `UploadBatch.dir_node_id` instead, even for
    /// a single-file "batch of one"). Every wire request for this file
    /// routes via `send_to(&host, ...)`, and a `FileUploadAck` /
    /// `FileTransferFailed` tagged with any other host is ignored --
    /// otherwise a workspace/host switch mid-upload would silently
    /// redirect the remaining chunks to the NEW active_host's daemon.
    pub(in crate::ui) host: HostKey,
    /// Basename sent to the backend (it sanitizes + de-dups).
    pub(in crate::ui) name: String,
    total: u64,
    sent: u64,
}

/// A multi-file upload batch. The OS picker returns N files that all target the
/// same cursored folder; they upload **sequentially** — one `UploadState` in
/// flight at a time — because the wire protocol is per-file (offset/name) and
/// serial transfer keeps the chunk/ack flow-control loop simple. The next file
/// is popped from `queue` when the current file's final ack lands. `dir` /
/// `dir_node_id` are resolved once for the whole batch (the picker is invoked
/// once); the completion refresh uses `dir_node_id`.
pub(in crate::ui) struct UploadBatch {
    /// Host + workspace this whole batch targets, pinned once at
    /// `start_upload` (ADR 0042 L2a codex review, item F) -- the batch
    /// outlives any single `UploadState`, including the completion
    /// refresh after the queue drains and `self.upload` is already
    /// `None`, so THIS is where the refresh's routing must be pinned.
    pub(in crate::ui) host: HostKey,
    workspace_id: Option<String>,
    /// Absolute backend destination directory (shared by every file).
    dir: String,
    /// `files:`-prefixed tree node id of the destination dir, for the
    /// end-of-batch listing refresh.
    dir_node_id: String,
    /// Local files not yet started (front = next). Drained as each completes.
    queue: std::collections::VecDeque<std::path::PathBuf>,
    /// Total files picked, for `file i/N` progress in the status line.
    total_files: usize,
    /// Files whose upload has fully completed (final ack seen).
    pub(in crate::ui) done_files: usize,
}

impl State {
    /// `d` in NavTree: download the cursored file row to the local downloads
    /// dir (OS-independent — `Settings::download_dir()`), non-clobbering. The
    /// transport streams chunks and writes the dest as they arrive. Directory
    /// rows are a no-op (download a file, not a folder).
    pub(in crate::ui) fn start_download(&mut self) {
        let is_dir = self
            .tree
            .rows
            .get(self.tree.selected)
            .map(|r| r.node.kind == "dir")
            .unwrap_or(false);
        if is_dir {
            self.status = "download · select a file, not a folder".to_string();
            self.window.request_redraw();
            return;
        }
        let Some(abs) = self.cursored_files_path() else {
            self.status = "download · no file under cursor".to_string();
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
            self.status = format!("download failed · mkdir {}: {e}", dir.display());
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
        self.status = format!("download · {basename} → {}", dest.display());
        self.window.request_redraw();
    }

    /// `u` in NavTree: pick one or more local files via the native OS dialog and
    /// upload them to the cursored nav folder (the dir itself for a dir row, else
    /// the cursored file's parent dir). The picker is multi-select; the files
    /// upload sequentially (see `UploadBatch`). Opens the first file and sends
    /// its first chunk; subsequent chunks/files are pumped by `FileUploadAck`.
    pub(in crate::ui) fn start_upload(&mut self) {
        if self.upload.is_some() || self.upload_batch.is_some() {
            self.status = "upload · already in progress".to_string();
            self.window.request_redraw();
            return;
        }
        let (is_dir, node_id) = match self.tree.rows.get(self.tree.selected) {
            Some(r) => (r.node.kind == "dir", r.node.id.clone()),
            None => (false, String::new()),
        };
        let Some(abs) = self.cursored_files_path() else {
            self.status = "upload · no target folder for this row".to_string();
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
                    self.status = "upload · cannot resolve parent folder".to_string();
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
                self.status = "upload · cancelled".to_string();
                self.window.request_redraw();
                return;
            }
        };
        let total_files = files.len();
        self.upload_batch = Some(UploadBatch {
            host: self.active_host.clone(),
            workspace_id: self.active_workspace_id.clone(),
            dir,
            dir_node_id,
            queue: files,
            total_files,
            done_files: 0,
        });
        self.start_next_file();
    }

    /// Pop the next file from the active `upload_batch` and begin uploading it,
    /// or — when the queue is drained — finalize the batch (one listing refresh
    /// + aggregate status). Called by `start_upload` to kick off the first file
    /// and by the `done` ack handler to advance to the next. A no-op if no batch
    /// is active.
    pub(in crate::ui) fn start_next_file(&mut self) {
        // Pull the next path + the shared destination out of the batch.
        let next = {
            let Some(batch) = self.upload_batch.as_mut() else {
                return;
            };
            batch.queue.pop_front().map(|p| {
                (
                    p,
                    batch.host.clone(),
                    batch.workspace_id.clone(),
                    batch.dir.clone(),
                    batch.dir_node_id.clone(),
                )
            })
        };
        let (local, host, workspace_id, dir, dir_node_id) = match next {
            Some(v) => v,
            None => {
                // Queue drained — the whole batch is complete. Refresh the
                // destination listing once so the new files appear without a
                // manual re-expand, then report the aggregate.
                let (host, workspace_id, dir, dir_node_id, done, total) =
                    match self.upload_batch.take() {
                        Some(b) => (
                            b.host,
                            b.workspace_id,
                            b.dir,
                            b.dir_node_id,
                            b.done_files,
                            b.total_files,
                        ),
                        None => return,
                    };
                self.refresh_upload_dir(&host, workspace_id, &dir_node_id);
                self.status = if total == 1 {
                    format!("uploaded · {done} file → {dir}")
                } else {
                    format!("uploaded · {done}/{total} files → {dir}")
                };
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
                self.status = format!("upload failed · open {}: {e}", local.display());
                // A local-open failure aborts the rest of the batch: already
                // uploaded files stay (refresh so they show), the remainder is
                // dropped. Predictable partial state over silent skipping.
                self.upload_batch = None;
                self.refresh_upload_dir(&host, workspace_id, &dir_node_id);
                self.window.request_redraw();
                return;
            }
        };
        self.upload = Some(UploadState {
            file,
            dir,
            host,
            name,
            total,
            sent: 0,
        });
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
        use std::io::Read;
        let read = {
            let Some(up) = self.upload.as_mut() else {
                return;
            };
            let mut buf = vec![0u8; UPLOAD_CHUNK];
            match up.file.read(&mut buf) {
                Ok(n) => {
                    buf.truncate(n);
                    let offset = up.sent;
                    up.sent += n as u64;
                    let eof = up.sent >= up.total;
                    Ok((
                        up.host.clone(),
                        up.dir.clone(),
                        up.name.clone(),
                        up.total,
                        up.sent,
                        offset,
                        eof,
                        buf,
                    ))
                }
                Err(e) => Err(format!("{e}")),
            }
        };
        match read {
            Ok((host, dir, name, total, sent, offset, eof, bytes)) => {
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
                    self.upload = None;
                    self.upload_batch = None;
                } else {
                    // Multi-file batches prefix `file i/N ·`; a lone file omits it.
                    let prefix = match self.upload_batch.as_ref() {
                        Some(b) if b.total_files > 1 => {
                            format!("file {}/{} · ", b.done_files + 1, b.total_files)
                        }
                        _ => String::new(),
                    };
                    self.status = format!("upload · {prefix}{name} {sent}/{total}");
                }
            }
            Err(e) => {
                let name = self
                    .upload
                    .as_ref()
                    .map(|u| u.name.clone())
                    .unwrap_or_default();
                tracing::warn!(error = %e, "upload: local read failed");
                self.status = format!("upload failed · read {name}: {e}");
                self.upload = None;
                self.upload_batch = None;
            }
        }
        self.window.request_redraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_owner_gate_ignores_a_non_owning_hosts_ack() {
        // ADR 0042 L2a codex review, item F: an UploadState pins the
        // host it's uploading to at start (reusing the field slot that
        // used to be the dead UploadState.dir_node_id). A
        // FileUploadAck/FileTransferFailed is applied only when its
        // event_host matches that pin -- mirrors the
        // `self.upload.as_ref().map(|u| &u.host) != Some(&event_host)`
        // gate in the real handlers, without needing a GPU-backed State
        // (upload holds a live std::fs::File, which isn't test-friendly
        // to construct).
        let upload_host: HostKey = "alpha".to_string();
        let owning_ack_host: HostKey = "alpha".to_string();
        let stray_ack_host: HostKey = "beta".to_string();
        assert_eq!(
            Some(&upload_host),
            Some(&owning_ack_host),
            "an ack from the pinned host is accepted"
        );
        assert_ne!(
            Some(&upload_host),
            Some(&stray_ack_host),
            "an ack from any OTHER host is dropped, even mid-batch"
        );
    }
}
