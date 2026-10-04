//! preview.get, preview.set_scale, image.crop, math.render: the requests (send_<op>: write the frame, then record its PendingKind).

use super::*;

/// Send one `figure.get` request. Extracted from the `OutgoingReq::FigureGet`
/// arm of `run_protocol`'s dispatch loop so the ordering that fixes a
/// round-2 review finding is a) shared, not duplicated, and b) directly
/// testable with a writer that fails, independent of the full connection/
/// handshake machinery `run_protocol` otherwise requires.
///
/// `pending` is updated BEFORE the write below, not after: `write_frame`
/// awaits a fallible `write_all`/`flush`, and on that error this function's
/// `?` propagates out of `run_protocol` entirely — the exact case
/// `PendingGuard`'s `Drop` exists to catch, but only for entries the map
/// already contains. Inserting first is safe: a reply cannot arrive before
/// the request even reaches the backend, and `id` is a freshly allocated,
/// never-reused key (`take_id`), so there's no live entry this could
/// collide with.
pub(crate) async fn send_figure_get<W: AsyncWrite + Unpin>(
    tx: &mut W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    url: String,
    node_id: String,
    workspace_id: Option<String>,
) -> Result<()> {
    pending.insert(id, PendingKind::FigureGet { url });
    codec::write_frame(
        tx,
        &Frame::req(
            id,
            op::PREVIEW_GET,
            serde_json::to_value(PreviewGetReq {
                node_id,
                workspace_id,
                page: None,
                fit_w: None,
                fit_h: None,
            })?,
        ),
        None,
    )
    .await?;
    Ok(())
}

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal `AsyncWrite` that fails every write — simulates the
    /// transport socket breaking mid-request without a real socket pair.
    struct FailingWriter;

    impl AsyncWrite for FailingWriter {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "simulated write failure",
            )))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// Round-2 review finding: `pending.insert` used to run AFTER the
    /// fallible write, so a write failure stranded the request — the
    /// guard's Drop found nothing to flush because the entry was never
    /// added. This drives the REAL send path (`send_figure_get`, the same
    /// function `run_protocol` calls) against a writer that always fails,
    /// then drops the guard exactly as `run_protocol` does on that `?`
    /// exit, and asserts the request still reaches `FigureGetFailed` —
    /// proving the insert-before-write ordering, not a reimplementation
    /// of it.
    #[tokio::test]
    async fn write_failure_flushes_via_guard_because_insert_precedes_the_write() {
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let host = "test-host".to_string();
        let mut writer = FailingWriter;
        {
            let mut guard = PendingGuard {
                map: HashMap::new(),
                evt_tx: &evt_tx,
                host: host.clone(),
            };
            let result = send_figure_get(
                &mut writer,
                &mut guard,
                42,
                "figures/never-sent.png".to_string(),
                "files:a/b.md".to_string(),
                None,
            )
            .await;
            assert!(result.is_err(), "the simulated write must fail");
            assert!(
                guard.contains_key(&42),
                "the entry must already be in the map when the write fails"
            );
            // `guard` drops here — the same `?` exit `run_protocol` takes.
        }
        let events: Vec<(HostKey, IncomingEvt)> = evt_rx.try_iter().collect();
        assert_eq!(
            events.len(),
            1,
            "the stranded request must flush exactly once, got {events:?}"
        );
        assert!(matches!(
            &events[0],
            (h, IncomingEvt::FigureGetFailed { url }) if h == &host && url == "figures/never-sent.png"
        ));
    }
}
