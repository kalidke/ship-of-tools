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
    host: HostKey,
    /// Basename sent to the backend (it sanitizes + de-dups).
    name: String,
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
    /// refresh after the queue drains and the file's upload is already
    /// `None`, so THIS is where the refresh's routing must be pinned.
    host: HostKey,
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
    done_files: usize,
}

/// The Files tree's in-flight file transactions: the Ctrl+N create and Ctrl+D delete awaiting their replies, and
/// the upload batch with its one file in flight. Each reply settles only the transaction it names.
#[derive(Default)]
pub(in crate::ui) struct FileOps {
    /// A create's node id awaiting its `file.write` or `dir.create` reply.
    created: Option<String>,
    /// A delete's node id awaiting its `file.delete` reply.
    deleted: Option<String>,
    upload: Option<UploadState>,
    batch: Option<UploadBatch>,
}

/// What the batch does next: start its next file, or finish (refresh the destination once, report the total).
pub(in crate::ui) enum NextUpload {
    Start { local: std::path::PathBuf, host: HostKey, workspace_id: Option<String>, dir: String, dir_node_id: String },
    Finished { host: HostKey, workspace_id: Option<String>, dir: String, dir_node_id: String, done: usize, total: usize },
}

/// One chunk read from the file in flight, as its `file.upload` request needs it.
pub(in crate::ui) struct UploadChunk {
    host: HostKey,
    dir: String,
    name: String,
    total: u64,
    sent: u64,
    offset: u64,
    eof: bool,
    bytes: Vec<u8>,
}

impl FileOps {
    pub(in crate::ui) fn mark_created(&mut self, node_id: String) { self.created = Some(node_id); }

    /// A create reply for `node_id`: true (and the create settled) only when it is the pending one.
    pub(in crate::ui) fn settle_created(&mut self, node_id: &str) -> bool {
        if self.created.as_deref() != Some(node_id) {
            return false;
        }
        self.created = None;
        true
    }

    pub(in crate::ui) fn mark_deleted(&mut self, node_id: String) { self.deleted = Some(node_id); }

    /// A delete reply for `node_id`: true (and the delete settled) only when it is the pending one.
    pub(in crate::ui) fn settle_deleted(&mut self, node_id: &str) -> bool {
        if self.deleted.as_deref() != Some(node_id) {
            return false;
        }
        self.deleted = None;
        true
    }

    /// An upload or its batch is running: `u` refuses a second one.
    pub(in crate::ui) fn uploading(&self) -> bool { self.upload.is_some() || self.batch.is_some() }

    fn begin_batch(
        &mut self, host: HostKey, workspace_id: Option<String>, dir: String, dir_node_id: String,
        queue: std::collections::VecDeque<std::path::PathBuf>,
    ) {
        let total_files = queue.len();
        self.batch = Some(UploadBatch { host, workspace_id, dir, dir_node_id, queue, total_files, done_files: 0 });
    }

    /// The batch's next step, the batch ending when its queue is drained; `None` with no batch.
    fn next_upload(&mut self) -> Option<NextUpload> {
        let batch = self.batch.as_mut()?;
        match batch.queue.pop_front() {
            Some(local) => Some(NextUpload::Start {
                local, host: batch.host.clone(), workspace_id: batch.workspace_id.clone(),
                dir: batch.dir.clone(), dir_node_id: batch.dir_node_id.clone(),
            }),
            None => self.batch.take().map(|b| NextUpload::Finished {
                host: b.host, workspace_id: b.workspace_id, dir: b.dir, dir_node_id: b.dir_node_id,
                done: b.done_files, total: b.total_files,
            }),
        }
    }

    /// A local file that will not open ends the batch; files already uploaded stay.
    fn drop_batch(&mut self) { self.batch = None; }

    fn start_file(&mut self, file: std::fs::File, dir: String, host: HostKey, name: String, total: u64) {
        self.upload = Some(UploadState { file, dir, host, name, total, sent: 0 });
    }

    /// The next chunk of the file in flight; `None` with none in flight, the file's name with the read error.
    fn read_chunk(&mut self) -> Option<Result<UploadChunk, (String, String)>> {
        use std::io::Read;
        let up = self.upload.as_mut()?;
        let mut buf = vec![0u8; UPLOAD_CHUNK];
        Some(match up.file.read(&mut buf) {
            Ok(n) => {
                buf.truncate(n);
                let offset = up.sent;
                up.sent += n as u64;
                Ok(UploadChunk {
                    host: up.host.clone(), dir: up.dir.clone(), name: up.name.clone(), total: up.total,
                    sent: up.sent, offset, eof: up.sent >= up.total, bytes: buf,
                })
            }
            Err(e) => Err((up.name.clone(), format!("{e}"))),
        })
    }

    /// `file i/N · ` while a batch of several runs; empty for a lone file.
    fn progress_prefix(&self) -> String {
        match self.batch.as_ref() {
            Some(b) if b.total_files > 1 => format!("file {}/{} · ", b.done_files + 1, b.total_files),
            _ => String::new(),
        }
    }

    /// The upload and its batch end (a failed send or read, a backend failure).
    pub(in crate::ui) fn abort_upload(&mut self) {
        self.upload = None;
        self.batch = None;
    }

    /// A reconnect strands the chunk/ack loop: both end; true when anything was running.
    pub(in crate::ui) fn interrupt(&mut self) -> bool {
        let had_batch = self.batch.take().is_some();
        self.upload.take().is_some() || had_batch
    }

    /// Whether an ack from `host` drives the file in flight (its pinned host).
    pub(in crate::ui) fn file_owned_by(&self, host: &HostKey) -> bool {
        self.upload.as_ref().map(|u| &u.host) == Some(host)
    }

    /// The host a failure must come from: the file's, or the batch's between files.
    pub(in crate::ui) fn owner(&self) -> Option<HostKey> {
        self.upload.as_ref().map(|u| u.host.clone()).or_else(|| self.batch.as_ref().map(|b| b.host.clone()))
    }

    /// The file's final ack: it counts against the batch.
    pub(in crate::ui) fn finish_file(&mut self) {
        self.upload = None;
        if let Some(b) = self.batch.as_mut() {
            b.done_files += 1;
        }
    }

    /// The first chunk's ack names the backend's resolved file name; later chunks target it.
    pub(in crate::ui) fn adopt_name(&mut self, final_name: Option<String>) {
        if let (Some(fname), Some(up)) = (final_name, self.upload.as_mut()) {
            up.name = fname;
        }
    }
}

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
        if self.file_ops.uploading() {
            self.status = "upload · already in progress".to_string();
            self.window.request_redraw();
            return;
        }
        let (is_dir, node_id) = match self.nav.tree().rows().get(self.nav.tree().selected()) {
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
                    self.status = format!("upload · {prefix}{name} {sent}/{total}");
                }
            }
            Err((name, e)) => {
                tracing::warn!(error = %e, "upload: local read failed");
                self.status = format!("upload failed · read {name}: {e}");
                self.file_ops.abort_upload();
            }
        }
        self.window.request_redraw();
    }
}

#[cfg(test)]
#[path = "transfer_tests.rs"]
mod tests;
