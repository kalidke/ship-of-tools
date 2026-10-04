//! Sidecar ops: `kernel.request` (the row's Julia kernel), `math.render` (MathJax), `pluto.open` (Pluto, for a notebook inside a registered row)
//! and `monitor.subscribe`, `monitor.unsubscribe` and `monitor.history` (the host monitor).

use anyhow::Context;
use anyhow::Result;
use serde_json::json;
use sot_protocol::op;
use sot_protocol::BlobDescriptor;
use sot_protocol::Frame;
use sot_protocol::KernelRequestReq;
use sot_protocol::MathRenderReq;
use sot_protocol::MathRenderRes;
use sot_protocol::MonitorHistoryReq;
use sot_protocol::MonitorHistoryRes;
use sot_protocol::MonitorSubscribeRes;
use sot_protocol::PlutoOpenReq;
use sot_protocol::PlutoOpenRes;
use crate::mathjax::MathJax;
use crate::pluto::Pluto;
use crate::session::Session;
use crate::workspaces::Workspaces;
use crate::handlers::{HandlerOutput, canonicalize_within_any_workspace};

pub async fn handle_kernel_request(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: KernelRequestReq =
        serde_json::from_value(payload_json).context("kernel.request payload")?;
    tracing::info!(
        kernel_op = %req.kernel_op,
        workspace_id = req.workspace_id.as_deref().unwrap_or("<default>"),
        "kernel.request"
    );

    let Some(ws) = workspaces.resolve(req.workspace_id.as_deref()) else {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::KERNEL_REQUEST,
                json!({
                    "error": format!("unknown workspace: {:?}", req.workspace_id),
                    "code": "unknown_workspace",
                }),
            ),
            None,
        )]);
    };
    let kernel = ws.kernel();
    let result = kernel.request(&req.kernel_op, req.kernel_payload).await;
    let (_, rev) = session.snapshot().await;
    let payload = match result {
        Ok(v) => v,
        // The kernel being unavailable (dead OR still starting) gets its own
        // code + a "Julia kernel unavailable: <reason>" message so callers
        // (Modules mode, any other kernel.request consumer) can distinguish
        // it from a live request that failed for some other reason (bad op,
        // a real wire/protocol error).
        Err(e) => match e.downcast_ref::<crate::kernel::KernelUnavailable>() {
            Some(unavailable) => json!({
                "error": format!("Julia kernel unavailable: {unavailable}"),
                "code": "kernel_unavailable",
                "kernel_op": req.kernel_op,
            }),
            None => json!({
                "error": format!("{e:#}"),
                "code": "kernel_request_failed",
                "kernel_op": req.kernel_op,
            }),
        },
    };
    Ok(vec![(
        Frame::res(req_id, op::KERNEL_REQUEST, payload).with_rev(rev),
        None,
    )])
}

pub async fn handle_math_render(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    mathjax: &MathJax,
) -> Result<HandlerOutput> {
    let req: MathRenderReq = serde_json::from_value(payload_json).context("math.render payload")?;
    tracing::info!(latex = %req.latex, display = req.display, "math.render");

    match mathjax.render(&req.latex, req.display).await {
        Ok(rendered) => {
            let bytes = rendered.svg;
            let res = MathRenderRes {
                blob: BlobDescriptor {
                    len: bytes.len() as u64,
                    mime: "image/svg+xml".to_string(),
                },
                ex: rendered.ex,
                display: req.display,
            };
            // math.render doesn't bump the session revision — it's a stateless
            // transform, not a state change. Replay would mean re-issuing the
            // request, not replaying the result.
            let (_, rev) = session.snapshot().await;
            Ok(vec![(
                Frame::res(req_id, op::MATH_RENDER, serde_json::to_value(res)?).with_rev(rev),
                Some(bytes),
            )])
        }
        Err(e) => {
            tracing::warn!(error = %e, "math.render failed");
            let payload = json!({
                "error": format!("{e:#}"),
                "code": "mathjax_render_failed",
            });
            Ok(vec![(Frame::res(req_id, op::MATH_RENDER, payload), None)])
        }
    }
}

pub async fn handle_pluto_open(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    pluto: &Pluto,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: PlutoOpenReq = serde_json::from_value(payload_json).context("pluto.open payload")?;
    tracing::info!(path = %req.path, "pluto.open");

    let raw_path = std::path::Path::new(&req.path);

    // Confine pluto.open to a KNOWN workspace (security review): accepted if
    // it canonicalizes under ANY currently-registered workspace's project
    // root (not just the default one — a non-default-workspace open would
    // otherwise be wrongly rejected). Canonicalize exactly ONCE here and use
    // `path` for everything below rather than `req.path`/`raw_path` again, so
    // the checked path and the acted-upon path can't diverge (TOCTOU).
    // Without this, any absolute path handed to pluto.open would spin up
    // Pluto (a code-execution surface) on a file completely outside every
    // known project.
    let Some(path) = canonicalize_within_any_workspace(raw_path, workspaces) else {
        let payload = json!({
            "error": format!("{} is outside every known workspace root", req.path),
            "code": "outside_workspace",
        });
        return Ok(vec![(Frame::res(req_id, op::PLUTO_OPEN, payload), None)]);
    };

    // Pluto-flavored check — frontend dispatches on .jl extension
    // alone (it can't see raw file bytes through the plugin's
    // tokens-JSON preview), so the header gate lives here. Read the
    // first 96 bytes only.
    match tokio::fs::File::open(&path).await {
        Ok(mut f) => {
            use tokio::io::AsyncReadExt;
            let mut head = [0u8; 96];
            let n = f.read(&mut head).await.unwrap_or(0);
            const MARKER: &[u8] = b"### A Pluto.jl notebook ###";
            let line_end = head[..n].iter().position(|&b| b == b'\n').unwrap_or(n);
            let flavored = head[..line_end].windows(MARKER.len()).any(|w| w == MARKER);
            if !flavored {
                let payload = json!({
                    "error": "file does not start with the Pluto header `### A Pluto.jl notebook ###`",
                    "code": "not_pluto_flavored",
                });
                return Ok(vec![(Frame::res(req_id, op::PLUTO_OPEN, payload), None)]);
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, path = %req.path,
                "pluto.open · file open failed (path resolution bug? wrong workspace root?)");
            let payload = json!({
                "error": format!("could not read file: {e}"),
                "code": "pluto_open_failed",
            });
            return Ok(vec![(Frame::res(req_id, op::PLUTO_OPEN, payload), None)]);
        }
    }

    match pluto.open_notebook(&path).await {
        Ok(url) => {
            let res = PlutoOpenRes { url };
            let (_, rev) = session.snapshot().await;
            Ok(vec![(
                Frame::res(req_id, op::PLUTO_OPEN, serde_json::to_value(res)?).with_rev(rev),
                None,
            )])
        }
        Err(e) => {
            tracing::warn!(error = %e, path = %req.path, "pluto.open failed");
            let payload = json!({
                "error": format!("{e:#}"),
                "code": "pluto_open_failed",
            });
            Ok(vec![(Frame::res(req_id, op::PLUTO_OPEN, payload), None)])
        }
    }
}

/// Answers `monitor.subscribe` with the host roster and the base cadence.
pub(crate) fn handle_monitor_subscribe(req_id: u64, workspaces: &Workspaces) -> Result<HandlerOutput> {
    let hosts = workspaces
        .monitor_hub()
        .map(|h| h.host_names())
        .unwrap_or_default();
    let res = MonitorSubscribeRes {
        interval_s: 1.0,
        hosts,
    };
    Ok(vec![(
        Frame::res(req_id, op::MONITOR_SUBSCRIBE, serde_json::to_value(res)?),
        None,
    )])
}

/// Answers `monitor.unsubscribe` with a bare ack.
pub(crate) fn handle_monitor_unsubscribe(req_id: u64) -> Result<HandlerOutput> {
    Ok(vec![(
        Frame::res(req_id, op::MONITOR_UNSUBSCRIBE, serde_json::json!({})),
        None,
    )])
}

/// Answers `monitor.history` with the sampled history of every host in the window.
pub(crate) fn handle_monitor_history(req_id: u64, payload_json: serde_json::Value, workspaces: &Workspaces) -> Result<HandlerOutput> {
    serde_json::from_value::<MonitorHistoryReq>(payload_json)
        .context("monitor.history payload")
        .and_then(|req| {
            let hosts = workspaces
                .monitor_hub()
                .map(|h| h.history(&req))
                .unwrap_or_default();
            let res = MonitorHistoryRes { hosts };
            Ok(vec![(
                Frame::res(req_id, op::MONITOR_HISTORY, serde_json::to_value(res)?),
                None,
            )])
        })
}
