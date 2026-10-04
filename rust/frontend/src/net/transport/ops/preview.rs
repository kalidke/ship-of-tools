//! preview.get, preview.set_scale, image.crop, math.render: the requests (send_<op>: write the frame, then record its PendingKind).
//! Their replies (on_<op>: the reply frame becomes an IncomingEvt).

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

pub(crate) fn on_math_render(
    frame: Frame,
    blob: Option<Vec<u8>>,
    emit: &impl Fn(IncomingEvt),
    latex: String,
    display: bool,
) {
    let is_display = display;
    match serde_json::from_value::<MathRenderRes>(frame.payload) {
        Ok(res) => {
            // SVG bytes ride as the framing blob.
            // `MathRenderRes::blob` carries the descriptor
            // (len/type) for documentation; the actual bytes
            // are the `blob` argument from `read_frame`. Skip
            // when the blob is missing — backend bug or a
            // weird transport edge — log and move on.
            let _ = res.blob;
            match blob {
                Some(svg_bytes) => {
                    emit(IncomingEvt::MathRendered {
                        latex,
                        svg_bytes,
                        ex: res.ex,
                        display: res.display,
                    });
                }
                None => {
                    tracing::warn!(
                        latex_len = latex.len(),
                        is_display,
                        "math.render reply missing blob bytes"
                    );
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, latex_len = latex.len(), is_display,
                "math.render res parse failed");
        }
    }
}

pub(crate) fn on_image_crop(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
    node_id: String,
) {
    if let Some(err) = frame.payload.get("error").and_then(|v| v.as_str()) {
        emit(IncomingEvt::ImageCropFailed {
            node_id,
            message: err.to_string(),
        });
    } else {
        match serde_json::from_value::<ImageCropRes>(frame.payload) {
            Ok(res) => {
                emit(IncomingEvt::ImageCropped {
                    node_id,
                    path: res.path,
                    x: res.x,
                    y: res.y,
                    w: res.w,
                    h: res.h,
                    src_w: res.src_w,
                    src_h: res.src_h,
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, "image.crop res parse failed");
            }
        }
    }
}

pub(crate) fn on_preview_get(
    frame: Frame,
    blob: Option<Vec<u8>>,
    emit: &impl Fn(IncomingEvt),
    node_id: String,
    workspace_id: Option<String>,
    generation: u64,
) {
    // An error envelope (`{"error", "code"}` — e.g.
    // `code: "kernel_unavailable"`) fails `PreviewGetRes`
    // deserialization (both its fields are required), so check
    // for it FIRST — same convention `PendingKind::SetScale`
    // below already uses for the same wire op. Surfaced on the
    // status line via `PreviewGetFailed`, carrying the same
    // ownership pair `IncomingEvt::Preview` echoes below so a
    // stale failure can be dropped the same way a stale success
    // is.
    if let Some(err) = frame.payload.get("error").and_then(|v| v.as_str()) {
        let code = frame
            .payload
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or("error");
        tracing::warn!(%node_id, %code, %err, "preview.get failed");
        emit(IncomingEvt::PreviewGetFailed {
            node_id: Some(node_id),
            workspace_id,
            generation,
            message: format!("{code}: {err}"),
        });
        return;
    }
    // Same shape as the connect-time preview.get: a typed
    // PreviewGetRes envelope plus a length-prefixed blob the
    // codec already pulled out. Emit the existing
    // `IncomingEvt::Preview` so the chrome handler reuses
    // the same routing it does at startup, but include the
    // node id + workspace so figure-url resolution can
    // anchor against the right markdown directory + go to
    // the right workspace.
    match serde_json::from_value::<PreviewGetRes>(frame.payload) {
        Ok(res) => {
            emit(IncomingEvt::Preview {
                node_id: Some(node_id),
                workspace_id,
                mime: res.mime,
                bytes: blob.unwrap_or_default(),
                extras: res.extras,
                generation,
            });
        }
        Err(e) => {
            // A reply that is neither an `{error, code}`
            // envelope (handled above) nor a valid
            // `PreviewGetRes` — the shape a MISSING FILE
            // produces, whose only trace used to be this warn
            // plus `reveal: target still absent`. That silence
            // actively misled: it was once read as a rendering
            // regression when the file simply did not exist.
            // `PendingKind::FigureGet` below already routes
            // both causes down one terminal path; the pane the
            // person is actually looking at gets the same.
            tracing::warn!(error = %e, "preview.get res parse failed");
            emit(IncomingEvt::PreviewGetFailed {
                node_id: Some(node_id),
                workspace_id,
                generation,
                message: format!("preview unavailable: {e}"),
            });
        }
    }
    return;
}

pub(crate) fn on_set_scale(
    frame: Frame,
    blob: Option<Vec<u8>>,
    emit: &impl Fn(IncomingEvt),
    node_id: String,
    workspace_id: Option<String>,
    generation: u64,
) {
    // ADR 0034 §5: the backend persisted the sidecar and returned
    // the RE-RENDERED preview in the same PreviewGetRes envelope,
    // with `extras.physical_scale` already rescaled for the served
    // image. Decode with the existing type and emit the existing
    // `IncomingEvt::Preview` so the chrome installs it through the
    // ONE preview path it already has — no second install path to
    // drift out of sync (the failure mode behind F2/R4-R6).
    //
    // This also has to be a REPLY, not an unsolicited push: replies
    // are correlated by frame id via `pending.remove`, so a pushed
    // preview frame would find no entry and be dropped silently.
    //
    // Rejections come back as an `error` payload under the same
    // frame id; surface them so the prompt's "saving…" resolves
    // instead of hanging.
    if let Some(err) = frame.payload.get("error").and_then(|v| v.as_str()) {
        let code = frame
            .payload
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or("error");
        tracing::warn!(%node_id, %code, %err, "preview.set_scale rejected");
        emit(IncomingEvt::ScaleSetFailed {
            node_id,
            message: format!("{code}: {err}"),
        });
        return;
    }
    match serde_json::from_value::<PreviewGetRes>(frame.payload) {
        Ok(res) => {
            emit(IncomingEvt::Preview {
                node_id: Some(node_id),
                workspace_id,
                mime: res.mime,
                bytes: blob.unwrap_or_default(),
                extras: res.extras,
                generation,
            });
        }
        Err(e) => {
            // Same rule as the plain `preview.get` arm above: a
            // malformed reply is a FAILED set_scale, not a
            // no-op. Silence here leaves the prompt's "saving…"
            // resolved-looking while nothing was served.
            tracing::warn!(error = %e, "preview.set_scale res parse failed");
            emit(IncomingEvt::ScaleSetFailed {
                node_id,
                message: format!("scale set but preview unavailable: {e}"),
            });
        }
    }
    return;
}

pub(crate) fn on_figure_get(
    frame: Frame,
    blob: Option<Vec<u8>>,
    emit: &impl Fn(IncomingEvt),
    url: String,
) {
    // Same wire shape as PreviewGet, routed to the chrome's
    // figure cache via a different IncomingEvt so the
    // active markdown buffer isn't replaced.
    //
    // Field report: an early `figure.get` (fired before the
    // backend has the target PNG yet) answers with `{error,
    // code}` — this used to warn-and-drop with no event at
    // all, so `url` never left `figure_pending` and every
    // later reload skipped it forever (dispatch_pending_figures
    // treats "pending" as "already in flight, don't refire").
    // No separate check for the error envelope is needed:
    // `PreviewGetRes` requires `mime` and `blob`, neither of
    // which an `{error, code}` payload carries, so it always
    // falls into the parse-failure arm below — one path,
    // both causes, both terminate as `FigureGetFailed`.
    match serde_json::from_value::<PreviewGetRes>(frame.payload) {
        Ok(res) => {
            emit(IncomingEvt::FigureLoaded {
                url,
                mime: res.mime,
                bytes: blob.unwrap_or_default(),
            });
        }
        Err(e) => {
            tracing::warn!(error = %e, %url, "figure.get failed or unparseable");
            emit(IncomingEvt::FigureGetFailed { url });
        }
    }
    return;
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
