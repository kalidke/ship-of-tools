//! pluto.open, video.open, docs.open, page.reply, quarto.open: the requests (send_<op>: write the frame, then record its PendingKind).
//! Their replies (on_<op>: the reply frame becomes an IncomingEvt).

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
    direct: sot_protocol::ReplyScope,
) -> Result<()> {
    tracing::info!(%path, id, "→ docs.open");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::DOCS_OPEN,
            serde_json::to_value(DocsOpenReq { path, direct })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::DocsOpen);
    Ok(())
}

pub(crate) async fn send_page_reply<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    workspace_id: String,
    handle: String,
    line: String,
) -> Result<()> {
    tracing::info!(%workspace_id, %handle, id, "→ page.reply");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::PAGE_REPLY,
            serde_json::to_value(PageReplyReq { workspace_id, handle: handle.clone(), line, hold: false })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::PageReply { handle });
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

pub(crate) fn on_pluto_open(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
) {
    let payload = frame.payload;
    if payload.get("code").and_then(|v| v.as_str()) == Some("pluto_starting") {
        let message = payload.get("error").and_then(|v| v.as_str()).unwrap_or("Pluto is still starting").to_string();
        emit(IncomingEvt::PlutoStarting { message });
        return;
    }
    let result = if payload.get("url").is_some() {
        match serde_json::from_value::<PlutoOpenRes>(payload) {
            Ok(r) => Ok(r.url),
            Err(e) => Err(format!("pluto.open res parse: {e}")),
        }
    } else {
        let msg = payload
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error")
            .to_string();
        Err(msg)
    };
    emit(IncomingEvt::PlutoOpened { result });
}

pub(crate) fn on_video_open(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
) {
    let payload = frame.payload;
    let result = if payload.get("url").is_some() {
        match serde_json::from_value::<VideoOpenRes>(payload) {
            Ok(r) => Ok(r.url),
            Err(e) => Err(format!("video.open res parse: {e}")),
        }
    } else {
        let msg = payload
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error")
            .to_string();
        Err(msg)
    };
    emit(IncomingEvt::VideoOpened { result });
}

pub(crate) fn on_docs_open(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
) {
    let payload = frame.payload;
    let result = if payload.get("url").is_some() {
        match serde_json::from_value::<DocsOpenRes>(payload) {
            Ok(r) => Ok(r.url),
            Err(e) => Err(format!("docs.open res parse: {e}")),
        }
    } else {
        let msg = payload
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error")
            .to_string();
        Err(msg)
    };
    emit(IncomingEvt::DocsOpened { result });
}

pub(crate) fn on_page_reply(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
    handle: String,
) {
    let payload = frame.payload;
    let (outcome, detail) = match error_text(&payload) {
        Some(message) => ("error".to_string(), message),
        None => match serde_json::from_value::<PageReplyRes>(payload) {
            Ok(r) => (r.outcome, r.detail),
            Err(e) => ("error".to_string(), format!("page.reply res parse: {e}")),
        },
    };
    emit(IncomingEvt::PageReplied { handle, outcome, detail });
}

pub(crate) fn on_quarto_open(
    frame: Frame,
    blob: Option<Vec<u8>>,
    emit: &impl Fn(IncomingEvt),
) {
    let payload = frame.payload;
    // The rendered HTML rides the framing blob, like math.render's
    // SVG: a `--embed-resources` Quarto doc routinely exceeds the
    // codec's 1 MiB *envelope* cap (a 1.2 MB HTML base64s to
    // 1.61 MiB), and an oversize envelope killed the whole
    // connection — the FE saw eof and rebuilt the nav tree.
    //
    // The legacy `html_base64` arm stays because the FE and the
    // daemon are separate binaries on separate hosts and roll out
    // independently; accepting both shapes makes the deploy order
    // irrelevant instead of leaving a window where `o` is broken.
    let result = if let Some(bytes) = blob {
        Ok(bytes)
    } else if let Some(b64) = payload.get("html_base64").and_then(|v| v.as_str()) {
        // Read the field off the JSON rather than through
        // `QuartoOpenRes` on purpose: the struct is the daemon's to
        // reshape for the blob move, and this arm must keep
        // compiling either way.
        base64::engine::general_purpose::STANDARD
            .decode(b64.as_bytes())
            .map_err(|e| format!("quarto.open base64 decode: {e}"))
    } else {
        let msg = payload
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error")
            .to_string();
        Err(msg)
    };
    emit(IncomingEvt::QuartoOpened { result });
}

#[cfg(test)]
#[path = "pages_tests.rs"]
mod tests;
