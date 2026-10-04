//! file.read, file.write, file.delete, dir.create, file.download, file.upload: the requests (send_<op>: write the frame, then record its PendingKind).
//! Their replies (on_<op>: the reply frame becomes an IncomingEvt).

use super::*;

/// Outcome of a `file.write` request, mirroring the backend's three response
/// shapes (success / optimistic-concurrency conflict / error).
#[derive(Debug, Clone)]
pub enum FileWriteResult {
    /// Write committed; `version` is the new content hash to keep editing against.
    #[allow(dead_code)]
    Ok { path: String, version: String },
    /// The on-disk file changed since the matching `FileRead`; carries the
    /// current content+version so the editor can reconcile, never auto-clobber.
    #[allow(dead_code)]
    Conflict {
        current_content: String,
        current_version: String,
    },
    /// Any other backend error — `code` is the protocol code, `message` detail.
    #[allow(dead_code)]
    Error { code: String, message: String },
}

/// Outcome of a `file.delete` request, mirroring the backend's two response
/// shapes (success / error). Directories are refused server-side with
/// `code: "is_directory"`, surfaced here as an `Error`.
#[derive(Debug, Clone)]
pub enum FileDeleteResult {
    /// File trashed; `path` is the absolute path that was removed and
    /// `trash_path` is the in-workspace recovery location when the
    /// `.sot-trash/` fallback was used (`None` for system trash).
    #[allow(dead_code)]
    Ok {
        path: String,
        trashed: bool,
        trash_path: Option<String>,
    },
    /// Any backend error — `code` is the protocol code (`bad_node_id`,
    /// `not_found`, `is_directory`, `file_delete_failed`, …), `message` detail.
    #[allow(dead_code)]
    Error { code: String, message: String },
}

/// Outcome of a `dir.create` request, mirroring the backend's two response
/// shapes (success / error, incl. `code: "already_exists"`).
#[derive(Debug, Clone)]
pub enum DirCreateResult {
    /// Directory created; `path` is the absolute path on disk.
    #[allow(dead_code)]
    Ok { path: String },
    /// Any backend error — `code` is the protocol code (`bad_node_id`,
    /// `already_exists`, `dir_create_failed`, …), `message` detail.
    #[allow(dead_code)]
    Error { code: String, message: String },
}

pub(crate) async fn send_file_read<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    node_id: String,
    workspace_id: Option<String>,
) -> Result<()> {
    tracing::debug!(%node_id, ?workspace_id, id, "→ file.read");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::FILE_READ,
            serde_json::to_value(FileReadReq {
                node_id: node_id.clone(),
                workspace_id,
            })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::FileRead { node_id });
    Ok(())
}

pub(crate) async fn send_file_write<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    node_id: String,
    content: String,
    expected_version: Option<String>,
    workspace_id: Option<String>,
) -> Result<()> {
    tracing::debug!(
        %node_id,
        bytes = content.len(),
        version = ?expected_version,
        ?workspace_id,
        id,
        "→ file.write"
    );
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::FILE_WRITE,
            serde_json::to_value(FileWriteReq {
                node_id: node_id.clone(),
                content,
                expected_version,
                workspace_id,
            })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::FileWrite { node_id });
    Ok(())
}

pub(crate) async fn send_file_delete<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    node_id: String,
    workspace_id: Option<String>,
) -> Result<()> {
    tracing::debug!(
        %node_id,
        ?workspace_id,
        id,
        "→ file.delete"
    );
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::FILE_DELETE,
            serde_json::to_value(FileDeleteReq {
                node_id: node_id.clone(),
                workspace_id,
            })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::FileDelete { node_id });
    Ok(())
}

pub(crate) async fn send_dir_create<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    node_id: String,
    workspace_id: Option<String>,
) -> Result<()> {
    tracing::debug!(
        %node_id,
        ?workspace_id,
        id,
        "→ dir.create"
    );
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::DIR_CREATE,
            serde_json::to_value(DirCreateReq {
                node_id: node_id.clone(),
                workspace_id,
            })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::DirCreate { node_id });
    Ok(())
}

pub(crate) async fn send_file_download<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    path: String,
    dest: PathBuf,
) -> Result<()> {
    tracing::info!(%path, dest = %dest.display(), id, "→ file.download");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::FILE_DOWNLOAD,
            serde_json::to_value(FileDownloadReq { path })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::FileDownload { dest, file: None });
    Ok(())
}

pub(crate) async fn send_file_upload<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    dir: String,
    name: String,
    offset: u64,
    total: u64,
    eof: bool,
    bytes: Vec<u8>,
) -> Result<()> {
    tracing::debug!(%dir, %name, offset, total, eof, len = bytes.len(), id, "→ file.upload");
    // Upload chunk bytes ride as base64 in the JSON
    // (`data_b64`), not a trailing blob — keeps the backend's
    // incoming-frame path simple (same as pty.write).
    let data_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::FILE_UPLOAD,
            serde_json::to_value(FileUploadReq {
                dir,
                name,
                offset,
                total,
                eof,
                data_b64,
            })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::FileUpload);
    Ok(())
}

pub(crate) fn on_file_read(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
    node_id: String,
) {
    match serde_json::from_value::<FileReadRes>(frame.payload) {
        Ok(res) => {
            emit(IncomingEvt::FileRead {
                node_id: res.node_id,
                exists: res.exists,
                content: res.content,
                version: res.version,
            });
        }
        Err(e) => {
            tracing::warn!(error = %e, %node_id, "file.read res parse failed");
        }
    }
}

pub(crate) fn on_file_write(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
    node_id: String,
) {
    // Mirror the backend's three shapes: happy-path FileWriteRes, a
    // `{code: "conflict", current_content, current_version}`
    // envelope, or any other `{error, code}` failure.
    let result = if let Some(code) = frame.payload.get("code").and_then(|v| v.as_str())
    {
        if code == "conflict" {
            let current_content = frame
                .payload
                .get("current_content")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let current_version = frame
                .payload
                .get("current_version")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            FileWriteResult::Conflict {
                current_content,
                current_version,
            }
        } else {
            let message = frame
                .payload
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("(no message)")
                .to_string();
            FileWriteResult::Error {
                code: code.to_string(),
                message,
            }
        }
    } else {
        match serde_json::from_value::<FileWriteRes>(frame.payload) {
            Ok(res) => FileWriteResult::Ok {
                path: res.path,
                version: res.version,
            },
            Err(e) => {
                tracing::warn!(error = %e, %node_id, "file.write res parse failed");
                FileWriteResult::Error {
                    code: "parse_failed".to_string(),
                    message: e.to_string(),
                }
            }
        }
    };
    emit(IncomingEvt::FileWriteDone { node_id, result });
}

pub(crate) fn on_file_delete(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
    node_id: String,
) {
    // Mirror the backend's two shapes: happy-path FileDeleteRes or
    // any `{error, code}` failure (`is_directory`, `not_found`, …).
    let result = if let Some(code) = frame.payload.get("code").and_then(|v| v.as_str())
    {
        let message = frame
            .payload
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("(no message)")
            .to_string();
        FileDeleteResult::Error {
            code: code.to_string(),
            message,
        }
    } else {
        match serde_json::from_value::<FileDeleteRes>(frame.payload) {
            Ok(res) => FileDeleteResult::Ok {
                path: res.path,
                trashed: res.trashed,
                trash_path: res.trash_path,
            },
            Err(e) => {
                tracing::warn!(error = %e, %node_id, "file.delete res parse failed");
                FileDeleteResult::Error {
                    code: "parse_failed".to_string(),
                    message: e.to_string(),
                }
            }
        }
    };
    emit(IncomingEvt::FileDeleteDone { node_id, result });
}

pub(crate) fn on_dir_create(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
    node_id: String,
) {
    // Mirror the backend's two shapes: happy-path DirCreateRes or
    // any `{error, code}` failure (`already_exists`, `bad_node_id`, …).
    let result = if let Some(code) = frame.payload.get("code").and_then(|v| v.as_str())
    {
        let message = frame
            .payload
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("(no message)")
            .to_string();
        DirCreateResult::Error {
            code: code.to_string(),
            message,
        }
    } else {
        match serde_json::from_value::<DirCreateRes>(frame.payload) {
            Ok(res) => DirCreateResult::Ok { path: res.path },
            Err(e) => {
                tracing::warn!(error = %e, %node_id, "dir.create res parse failed");
                DirCreateResult::Error {
                    code: "parse_failed".to_string(),
                    message: e.to_string(),
                }
            }
        }
    };
    emit(IncomingEvt::DirCreateDone { node_id, result });
}

pub(crate) fn on_file_download(
    frame: Frame,
    mut blob: Option<Vec<u8>>,
    pending: &mut HashMap<u64, PendingKind>,
    emit: &impl Fn(IncomingEvt),
    dest: PathBuf,
    mut file: Option<std::fs::File>,
) {
    let frame_id = frame.id;
    let payload = frame.payload;
    // A download error replies with `{error, code}` and no chunk.
    if let Some(err) = payload.get("error").and_then(|v| v.as_str()) {
        tracing::warn!(error = %err, dest = %dest.display(), "file.download failed");
        if file.is_some() {
            let _ = std::fs::remove_file(&dest);
        }
        emit(IncomingEvt::FileTransferFailed {
            op: "download",
            message: err.to_string(),
        });
    } else {
        match serde_json::from_value::<FileChunk>(payload) {
            Ok(chunk) => {
                let bytes = blob.take().unwrap_or_default();
                // Create (truncating) the dest on the first chunk so a
                // pre-chunk error never leaves an empty file behind.
                if file.is_none() {
                    match std::fs::File::create(&dest) {
                        Ok(f) => file = Some(f),
                        Err(e) => {
                            tracing::warn!(error = %e, dest = %dest.display(),
                                "file.download: cannot create dest");
                            emit(IncomingEvt::FileTransferFailed {
                                op: "download",
                                message: format!("create {}: {e}", dest.display()),
                            });
                            return;
                        }
                    }
                }
                let mut write_err: Option<String> = None;
                if let Some(f) = file.as_mut() {
                    use std::io::{Seek, SeekFrom, Write};
                    if let Err(e) = f
                        .seek(SeekFrom::Start(chunk.offset))
                        .and_then(|_| f.write_all(&bytes))
                    {
                        write_err = Some(format!("write {}: {e}", dest.display()));
                    }
                }
                if let Some(msg) = write_err {
                    tracing::warn!(dest = %dest.display(), %msg, "file.download write failed");
                    let _ = std::fs::remove_file(&dest);
                    emit(IncomingEvt::FileTransferFailed {
                        op: "download",
                        message: msg,
                    });
                } else {
                    let written = chunk.offset + bytes.len() as u64;
                    emit(IncomingEvt::FileDownloadProgress {
                        dest: dest.clone(),
                        written,
                        total: chunk.total,
                        eof: chunk.eof,
                    });
                    if !chunk.eof {
                        // One request id, many chunks: keep the
                        // transfer (with its open file handle) alive
                        // for the next streamed frame.
                        pending
                            .insert(frame_id, PendingKind::FileDownload { dest, file });
                    }
                    // On eof: pending stays removed; `file` drops here,
                    // flushing + closing the completed download.
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "file.download chunk parse failed");
                let _ = std::fs::remove_file(&dest);
                emit(IncomingEvt::FileTransferFailed {
                    op: "download",
                    message: format!("bad chunk: {e}"),
                });
            }
        }
    }
}

pub(crate) fn on_file_upload(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
) {
    let payload = frame.payload;
    if let Some(err) = payload.get("error").and_then(|v| v.as_str()) {
        tracing::warn!(error = %err, "file.upload failed");
        emit(IncomingEvt::FileTransferFailed {
            op: "upload",
            message: err.to_string(),
        });
    } else {
        match serde_json::from_value::<FileUploadAck>(payload) {
            Ok(ack) => {
                emit(IncomingEvt::FileUploadAck {
                    offset: ack.offset,
                    done: ack.done,
                    final_name: ack.final_name,
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, "file.upload ack parse failed");
                emit(IncomingEvt::FileTransferFailed {
                    op: "upload",
                    message: format!("bad ack: {e}"),
                });
            }
        }
    }
}
