//! preview.get, preview.set_scale, image.crop, math.render: the requests (send_<op>: write the frame, then record its PendingKind).

use super::*;

pub(crate) async fn send_math_render<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    latex: String,
    display: bool,
) -> Result<()> {
    let is_display = display;
    tracing::debug!(
        latex_len = latex.len(),
        is_display,
        id,
        "→ math.render"
    );
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::MATH_RENDER,
            serde_json::to_value(MathRenderReq {
                latex: latex.clone(),
                display,
            })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::MathRender { latex, display });
    Ok(())
}

pub(crate) async fn send_image_crop<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    node_id: String,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    workspace_id: Option<String>,
) -> Result<()> {
    tracing::debug!(%node_id, x, y, w, h, ?workspace_id, id, "→ image.crop");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::IMAGE_CROP,
            serde_json::to_value(ImageCropReq {
                node_id: node_id.clone(),
                x,
                y,
                w,
                h,
                workspace_id,
            })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::ImageCrop { node_id });
    Ok(())
}

pub(crate) async fn send_preview_get<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    node_id: String,
    workspace_id: Option<String>,
    page: Option<u32>,
    fit_w: Option<u32>,
    fit_h: Option<u32>,
    generation: u64,
) -> Result<()> {
    tracing::debug!(%node_id, ?workspace_id, ?page, generation, id, "→ preview.get");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::PREVIEW_GET,
            serde_json::to_value(PreviewGetReq {
                node_id: node_id.clone(),
                workspace_id: workspace_id.clone(),
                page,
                fit_w,
                fit_h,
            })?,
        ),
        None,
    )
    .await?;
    pending.insert(
        id,
        PendingKind::PreviewGet {
            node_id,
            workspace_id,
            generation,
        },
    );
    Ok(())
}

pub(crate) async fn send_preview_set_scale<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    node_id: String,
    nm_per_px: f64,
    workspace_id: Option<String>,
    generation: u64,
) -> Result<()> {
    tracing::debug!(%node_id, nm_per_px, ?workspace_id, generation, id,
        "→ preview.set_scale");
    // Isotropic from a single typed value. `nm_per_px` is the
    // RAW/original pixel size the user entered, sent verbatim:
    // the backend writes it to the sidecar as-is and returns
    // the served-rescaled value for rendering, so a round-trip
    // can never compound the downsample ratio (ADR 0034 §5).
    let payload = serde_json::json!({
        "node_id": node_id,
        "workspace_id": workspace_id,
        "physical_scale": {
            "axes": [
                { "name": "x", "nm_per_px": nm_per_px },
                { "name": "y", "nm_per_px": nm_per_px },
            ],
            "unit": "nm",
        },
    });
    codec::write_frame(
        &mut tx,
        &Frame::req(id, op::PREVIEW_SET_SCALE, payload),
        None,
    )
    .await?;
    pending.insert(
        id,
        PendingKind::SetScale {
            node_id,
            workspace_id,
            generation,
        },
    );
    Ok(())
}
