//! file.read, file.write, file.delete, dir.create, file.download, file.upload: the requests (send_<op>: write the frame, then record its PendingKind).

use super::*;

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
