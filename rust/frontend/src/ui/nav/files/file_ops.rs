//! The Files tree's in-flight file transactions: the pending create and delete, and the upload batch with its one
//! file in flight.

use super::*;
use super::transfer::UPLOAD_CHUNK;

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
    pub(in crate::ui) host: HostKey,
    pub(in crate::ui) dir: String,
    pub(in crate::ui) name: String,
    pub(in crate::ui) total: u64,
    pub(in crate::ui) sent: u64,
    pub(in crate::ui) offset: u64,
    pub(in crate::ui) eof: bool,
    pub(in crate::ui) bytes: Vec<u8>,
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

    pub(in crate::ui) fn begin_batch(
        &mut self, host: HostKey, workspace_id: Option<String>, dir: String, dir_node_id: String,
        queue: std::collections::VecDeque<std::path::PathBuf>,
    ) {
        let total_files = queue.len();
        self.batch = Some(UploadBatch { host, workspace_id, dir, dir_node_id, queue, total_files, done_files: 0 });
    }

    /// The batch's next step, the batch ending when its queue is drained; `None` with no batch.
    pub(in crate::ui) fn next_upload(&mut self) -> Option<NextUpload> {
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
    pub(in crate::ui) fn drop_batch(&mut self) { self.batch = None; }

    pub(in crate::ui) fn start_file(&mut self, file: std::fs::File, dir: String, host: HostKey, name: String, total: u64) {
        self.upload = Some(UploadState { file, dir, host, name, total, sent: 0 });
    }

    /// The next chunk of the file in flight; `None` with none in flight, the file's name with the read error.
    pub(in crate::ui) fn read_chunk(&mut self) -> Option<Result<UploadChunk, (String, String)>> {
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
    pub(in crate::ui) fn progress_prefix(&self) -> String {
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
