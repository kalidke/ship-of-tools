//! pluto.open, video.open, docs.open, quarto.open: the requests (send_<op>: write the frame, then record its PendingKind).

use super::*;

pub(crate) async fn send_pluto_open<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    path: String,
) -> Result<()> {
    tracing::info!(%path, id, "→ pluto.open");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::PLUTO_OPEN,
            serde_json::to_value(PlutoOpenReq { path })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::PlutoOpen);
    Ok(())
}

pub(crate) async fn send_video_open<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    path: String,
) -> Result<()> {
    tracing::info!(%path, id, "→ video.open");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::VIDEO_OPEN,
            serde_json::to_value(VideoOpenReq { path })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::VideoOpen);
    Ok(())
}

pub(crate) async fn send_docs_open<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    path: String,
) -> Result<()> {
    tracing::info!(%path, id, "→ docs.open");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::DOCS_OPEN,
            serde_json::to_value(DocsOpenReq { path })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::DocsOpen);
    Ok(())
}

pub(crate) async fn send_quarto_open<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    path: String,
    execute: bool,
) -> Result<()> {
    tracing::info!(%path, execute, id, "→ quarto.open");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::QUARTO_OPEN,
            serde_json::to_value(QuartoOpenReq { path, execute })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::QuartoOpen);
    Ok(())
}
