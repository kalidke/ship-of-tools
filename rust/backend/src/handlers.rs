// handlers.rs — op dispatch for the M1 spike.
//
// Each handler takes a parsed Frame (the codec already verified envelope +
// blob), returns a (Frame, Option<Vec<u8>>) tuple the connection task writes
// back. Handlers borrow the Session for state mutations.
//
// All content here is hardcoded for the spike. The eventual kernel-driven
// path replaces these stubs with calls into ShipToolsKernel over its own pipe;
// the on-the-wire Frame shape stays the same.

use anyhow::{Context, Result};
use serde_json::json;
use sot_protocol::{
    op, AgentSendReq, AgentSendRes, BlobDescriptor, ConceptListRes, ConceptReadReq, ConceptReadRes,
    ConceptWriteReq, ConceptWriteRes, DocsOpenReq, DocsOpenRes, FeCommandEvt, FeCommandSendReq,
    FeCommandSendRes, FileChunk, FileDeleteReq, FileDeleteRes, FileDownloadReq, FileReadReq,
    FileReadRes, FileUploadAck, FileUploadReq, FileWriteReq, FileWriteRes, Frame, HelloReq,
    HelloRes, ImageCropReq, ImageCropRes, KernelRequestReq, MathRenderReq, MathRenderRes,
    PlutoOpenReq, PlutoOpenRes, PreviewGetReq, PreviewGetRes, QuartoOpenReq, QuartoOpenRes,
    ReplErrorOut, ReplExecuteInput, ReplExecuteReq, ReplExecuteRes, ReplValueOut, StackFrame,
    TmuxCapturePaneReq, TmuxCapturePaneRes, TmuxCreateSessionReq, TmuxKillSessionReq,
    TmuxListPanesReq, TmuxListPanesRes, TmuxListSessionsRes, TmuxPane, TmuxSession,
    ToggleHiddenReq, ToggleHiddenRes, TreeChildrenReq, TreeChildrenRes, TreeRootReq, TreeRootRes,
    VideoOpenReq, VideoOpenRes,
};

use crate::file_io::{self, WriteResult};
use crate::files_mode::{mime_for_path, FilesMode};
use crate::kernel::Kernel;
use crate::mathjax::MathJax;
use crate::repl::ReplFrameMsg;
use crate::pluto::Pluto;
use crate::session::Session;
use crate::tmux::TmuxClient;
use crate::workspaces::{AgentMessage, WorkspaceChanged, Workspaces};
use tokio::sync::broadcast;

/// Output of an op handler. The first frame is the response to the request;
/// any additional frames are emitted in order and represent things like ring
/// replay on hello.
pub type HandlerOutput = Vec<(Frame, Option<Vec<u8>>)>;

/// Fallback markdown served when the requested node id is the project-root
/// directory itself — directories don't have meaningful byte content, but the
/// frontend still asks for a preview, so we give it a short blurb describing
/// where it is. File-backed nodes serve their actual bytes.
const ROOT_PREVIEW_TEMPLATE: &str =
    "# {root}\n\nFiles-mode root. Navigate the tree to preview individual files.\n";

/// Cap on the file size we'll send through `preview.get` for text/* mimes,
/// where truncating mid-stream is lossy-but-rendersable. Phase-1 frontends
/// don't yet have scrolling affordances and pulling a 100 MiB log through
/// the wire isn't useful.
const PREVIEW_BYTE_CAP: usize = 2 * 1024 * 1024;

/// Cap on binary mimes (image/*, application/*, etc.) where mid-stream
/// truncation corrupts the format and produces undecodable garbage on the
/// frontend. Files larger than this are refused with a text/plain blurb
/// rather than shipped as broken bytes. Set generously — scientific PNGs
/// in the hundreds of MB are real, and the wire path can handle them; the
/// frontend downsamples decoded textures that exceed the GPU's max
/// dimension so a huge image renders at reduced resolution rather than
/// failing validation.
const PREVIEW_BINARY_CAP: usize = 512 * 1024 * 1024;

/// Preview-time downsample: an oversize raster is decoded and scaled so its
/// longest side is <= this, then re-encoded as PNG before shipping. The FE
/// already downscales decoded textures to the GPU's max dimension for *display*,
/// so a preview-sized raster is visually identical — but shipping the raw file
/// (a 473 MB real-world posterior render is real) would hold the connection minutes
/// draining bytes the FE immediately shrinks (even with the size-scaled write
/// deadline). Cap kept well above any preview pane's pixel budget so zoom keeps
/// detail. (2026-06-30, a large real-world image dir: posterior_image.png ~473 MB
/// plus several 30-70 MB renders.)
const PREVIEW_DOWNSAMPLE_MAX_DIM: u32 = 6000;

/// Only decode+downsample when the raw file exceeds this — smaller rasters ship
/// as-is (exact bytes, no decode cost). ~20 MB drains in a couple seconds even
/// over a tunnel, so the footgun is only the much larger renders.
const PREVIEW_DOWNSAMPLE_TRIGGER: usize = 20 * 1024 * 1024;

/// Decode-alloc ceiling for the backend preview downsample (mirrors the FE's
/// lifted limit). A raster whose decoded RGBA exceeds this errors cleanly and we
/// ship the raw bytes (the size-scaled write deadline still delivers them)
/// rather than OOM the daemon; a multi-gigapixel monster wants tiled decode.
const MAX_PREVIEW_DECODE_ALLOC: u64 = 4 * 1024 * 1024 * 1024;

/// Streaming-decode media (video) whose preview plugin produces a bounded
/// payload — a poster frame + metadata — regardless of input size, because it
/// shells out to ffmpeg rather than reading the file into the payload. Used to
/// exempt these from the input-size gate in `try_plugin_preview`. Keep in sync
/// with `ShipToolsVideoFile`'s `VIDEO_EXTENSIONS`.
fn is_streaming_media(path: &std::path::Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("mp4" | "webm" | "mov" | "mkv" | "m4v")
    )
}

/// Extensions whose preview plugin produces BOUNDED output regardless of input
/// size, so the `PREVIEW_BYTE_CAP` input gate in `try_plugin_preview` must NOT
/// skip them. The gate proxies "big input → big output", which is FALSE here:
///
/// - **video** (`is_streaming_media`): ffmpeg poster + metadata, never reads the
///   container into the payload.
/// - **HDF5** (`.h5`/`.hdf5`/`.hdf`): the `HDF5Preview` plugin walks group/dataset
///   *metadata only* (names, shapes, dtypes, attrs) and never reads dataset
///   contents — an 8 GB file yields the same small tree as an 8 KB one.
///
/// Gating these defeats the feature: multi-GB scientific `.h5` files are the
/// whole use case, and skipping the plugin sends them to the bytes-level reader
/// which returns raw binary. (Principled follow-up: have the kernel declare
/// per-FileType whether output is bounded, instead of duplicating extension
/// knowledge here — but that's a bigger change than this urgent fix warrants.)
fn is_bounded_output_plugin(path: &std::path::Path) -> bool {
    if is_streaming_media(path) {
        return true;
    }
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        // pdf: ADR 0021 — output is one page-sized PNG regardless of
        // document size, so the input-size cap must not gate it.
        Some("h5" | "hdf5" | "hdf" | "pdf")
    )
}

/// Outcome of the FE↔BE protocol handshake gate (ADR 0030 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProtocolGate {
    /// Client protocol equals ours — proceed cleanly.
    Accept,
    /// Client is pre-versioning (protocol == 0) and we're still at
    /// PROTOCOL_VERSION 1 — accept under the one-time transition grace, but
    /// warn so the skew is visible.
    AcceptLegacy,
    /// Protocols differ — reject the hello with a structured mismatch error.
    Reject,
}

/// Gate the handshake on protocol integer equality (ADR 0030 §2).
///
/// Accepts when the client's protocol equals ours. As a one-time transition
/// grace, a pre-versioning frontend (`protocol == 0`, i.e. it predates the
/// versioned handshake and simply omitted the field) is also accepted WHILE
/// our `PROTOCOL_VERSION` is still 1. The moment we bump to protocol 2, that
/// grace evaporates and `0` is rejected like any other mismatch: a peer that
/// can't even name its protocol can't be trusted on a v2 wire.
fn protocol_gate(client_protocol: u32) -> ProtocolGate {
    if client_protocol == sot_protocol::PROTOCOL_VERSION {
        ProtocolGate::Accept
    } else if client_protocol == 0 && sot_protocol::PROTOCOL_VERSION == 1 {
        ProtocolGate::AcceptLegacy
    } else {
        ProtocolGate::Reject
    }
}

pub async fn handle_hello(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    expected_token: &Option<String>,
    files_mode: &FilesMode,
    label: Option<&str>,
    clients: &crate::clients::Clients,
) -> Result<HandlerOutput> {
    let req: HelloReq = serde_json::from_value(payload_json).context("hello payload")?;
    let (session_id, revision) = session.snapshot().await;

    // App-level token gate — vestigial since 0.4.0 removed the daemon TCP
    // listener (the only transport that resolved a token): `expected_token`
    // is always `None` now, so this gate never fires. Kept (with its
    // constant-time compare and the empty-string filter) rather than ripped
    // out because the hello `token` wire field survives for cross-version
    // compat and the gate is the tested, safe shape if a gated transport
    // ever returns. `.filter(|s| !s.is_empty())` guards the one place an
    // empty expected token would matter (an unauthenticated client's
    // `req.token` also defaults to `""` below, so `Some("")` would match
    // trivially and authenticate with no real secret).
    if let Some(expected) = expected_token.as_deref().filter(|s| !s.is_empty()) {
        let presented = req.token.as_deref().unwrap_or("");
        if !constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
            tracing::warn!(
                client_id = %req.client_id,
                "hello rejected: token mismatch"
            );
            let payload = serde_json::json!({
                "error": "authentication failed",
                "code": "token_mismatch",
            });
            return Ok(vec![(
                Frame::res(req_id, op::HELLO, payload).with_rev(revision),
                None,
            )]);
        }
    }

    // Protocol version gate (ADR 0030 §2). Mirrors the token-mismatch shape
    // above: a structured `{error, code}` envelope that does NOT deserialize
    // as `HelloRes`, so the frontend surfaces a clear "update needed" screen
    // instead of failing on a later op with a cryptic frame-parse error.
    match protocol_gate(req.protocol) {
        ProtocolGate::Accept => {}
        ProtocolGate::AcceptLegacy => {
            tracing::warn!(
                client_id = %req.client_id,
                client_protocol = req.protocol,
                backend_protocol = sot_protocol::PROTOCOL_VERSION,
                "hello: pre-versioning frontend accepted under ADR 0030 transition grace"
            );
        }
        ProtocolGate::Reject => {
            let frontend_version = if req.app_version.is_empty() {
                "<pre-versioning>".to_string()
            } else {
                req.app_version.clone()
            };
            let message = format!(
                "protocol mismatch: backend {} (protocol {}) vs frontend {} (protocol {}) \
                 — update the older side",
                sot_protocol::app_version(),
                sot_protocol::PROTOCOL_VERSION,
                frontend_version,
                req.protocol,
            );
            tracing::warn!(
                client_id = %req.client_id,
                client_protocol = req.protocol,
                backend_protocol = sot_protocol::PROTOCOL_VERSION,
                "hello rejected: {message}"
            );
            let payload = serde_json::json!({
                "error": message,
                "code": "protocol_mismatch",
                "backend_protocol": sot_protocol::PROTOCOL_VERSION,
                "frontend_protocol": req.protocol,
                "backend_version": sot_protocol::app_version(),
                "frontend_version": req.app_version,
            });
            return Ok(vec![(
                Frame::res(req_id, op::HELLO, payload).with_rev(revision),
                None,
            )]);
        }
    }

    // Replay policy:
    //   - First-time client (no session_id): nothing to replay.
    //   - Session matches: replay every ring entry newer than last_seen_revision.
    //   - Session mismatches (e.g. backend restarted): snapshot_pending; client
    //     needs to refetch state from scratch.
    // If `last_seen_revision` is older than the ring's low watermark,
    // session.replay_after returns None, and we mark snapshot_pending too.
    let replay = match req.session_id.as_deref() {
        None => Some(Vec::new()),
        Some(sid) if sid == session_id => session.replay_after(req.last_seen_revision).await,
        Some(_) => None,
    };
    let snapshot_pending = replay.is_none();
    let replay_entries = replay.unwrap_or_default();

    tracing::info!(
        client_id = %req.client_id,
        client_session = ?req.session_id,
        client_rev = req.last_seen_revision,
        session_id = %session_id,
        revision,
        replay_count = replay_entries.len(),
        snapshot_pending,
        "hello"
    );

    // Surface backend identity to the chrome so users can tell where
    // they're connected. `gethostname` falls back to "unknown" on the
    // off chance the kernel returns an error; `root_path` is the
    // configured --project-root (absolute, canonicalised on startup).
    let host = gethostname::gethostname()
        .into_string()
        .ok()
        .filter(|s| !s.is_empty());
    let project_root = Some(files_mode.root_path().display().to_string());

    let res = HelloRes {
        session_id,
        revision,
        snapshot_pending,
        host,
        project_root,
        label: label.map(str::to_string),
        // Includes the connection this hello answers — it registers in
        // `handle_connection` before this handler runs (ADR 0010/0013).
        clients_connected: clients.count(),
        // ADR 0030 §2: report our wire-contract protocol + product version so
        // the frontend can warn on a legacy backend (protocol 0) and surface
        // both sides' versions if a later skew check needs them.
        protocol: sot_protocol::PROTOCOL_VERSION,
        app_version: sot_protocol::app_version(),
    };

    let mut out: HandlerOutput = Vec::with_capacity(1 + replay_entries.len());
    out.push((
        Frame::res(req_id, op::HELLO, serde_json::to_value(res)?).with_rev(revision),
        None,
    ));
    for entry in replay_entries {
        out.push((
            Frame::evt(&entry.op, entry.payload).with_rev(entry.revision),
            None,
        ));
    }
    Ok(out)
}

pub async fn handle_tree_root(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: TreeRootReq = serde_json::from_value(payload_json).context("tree.root payload")?;
    tracing::info!(
        mode = %req.mode,
        workspace_id = req.workspace_id.as_deref().unwrap_or("<default>"),
        "tree.root"
    );

    // Only files-mode for now; the other six modes live behind their own
    // verbs once kernel-side Mode dispatch is wired (post-phase-1).
    if req.mode != "files" {
        let payload = json!({
            "error": format!("unknown mode: {}", req.mode),
            "code": "unknown_mode",
        });
        return Ok(vec![(Frame::res(req_id, op::TREE_ROOT, payload), None)]);
    }

    let Some(ws) = workspaces.resolve(req.workspace_id.as_deref()) else {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::TREE_ROOT,
                json!({
                    "error": format!("unknown workspace: {:?}", req.workspace_id),
                    "code": "unknown_workspace",
                }),
            ),
            None,
        )]);
    };
    let files_mode = match ws.files_mode() {
        Ok(fm) => fm,
        Err(e) => {
            return Ok(vec![(
                Frame::res(
                    req_id,
                    op::TREE_ROOT,
                    json!({
                        "error": format!("files_mode init failed: {e:#}"),
                        "code": "files_mode_init_failed",
                    }),
                ),
                None,
            )]);
        }
    };
    let root = files_mode.root_node();
    let children = files_mode
        .children_of(&root.id)
        .context("listing project root")?;
    let res = TreeRootRes {
        node: root,
        children,
    };

    let rev = session
        .bump("tree.invalidate", json!({ "scope": req.mode }))
        .await;

    Ok(vec![(
        Frame::res(req_id, op::TREE_ROOT, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}

pub async fn handle_tree_children(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: TreeChildrenReq =
        serde_json::from_value(payload_json).context("tree.children payload")?;
    tracing::info!(
        node_id = %req.node_id,
        workspace_id = req.workspace_id.as_deref().unwrap_or("<default>"),
        "tree.children"
    );

    let Some(ws) = workspaces.resolve(req.workspace_id.as_deref()) else {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::TREE_CHILDREN,
                json!({
                    "error": format!("unknown workspace: {:?}", req.workspace_id),
                    "code": "unknown_workspace",
                }),
            ),
            None,
        )]);
    };
    let files_mode = ws.files_mode().context("files_mode init")?;
    let children = match files_mode.children_of(&req.node_id) {
        Ok(c) => c,
        Err(e) => {
            let payload = json!({
                "error": format!("{e:#}"),
                "code": "tree_children_failed",
                "node_id": req.node_id,
            });
            return Ok(vec![(Frame::res(req_id, op::TREE_CHILDREN, payload), None)]);
        }
    };

    let res = TreeChildrenRes { children };
    let (_, rev) = session.snapshot().await;
    Ok(vec![(
        Frame::res(req_id, op::TREE_CHILDREN, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}

/// Flip the workspace's Files-mode "show hidden files" flag and invalidate the
/// files tree. Mirrors `handle_tree_root`'s workspace resolution + the same
/// `tree.invalidate` bump so a reconnecting client re-fetches; the live
/// frontend re-fetches `tree.root` right after this op. The flag lives on the
/// cached `Arc<FilesMode>` (interior mutability), so subsequent
/// `tree.children` / `tree.root` walks pick up the new visibility.
pub async fn handle_nav_toggle_hidden(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: ToggleHiddenReq =
        serde_json::from_value(payload_json).context("nav.toggle_hidden payload")?;
    tracing::info!(
        workspace_id = req.workspace_id.as_deref().unwrap_or("<default>"),
        mode = req.mode.as_deref().unwrap_or("files"),
        "nav.toggle_hidden"
    );

    let Some(ws) = workspaces.resolve(req.workspace_id.as_deref()) else {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::NAV_TOGGLE_HIDDEN,
                json!({
                    "error": format!("unknown workspace: {:?}", req.workspace_id),
                    "code": "unknown_workspace",
                }),
            ),
            None,
        )]);
    };
    let files_mode = match ws.files_mode() {
        Ok(fm) => fm,
        Err(e) => {
            return Ok(vec![(
                Frame::res(
                    req_id,
                    op::NAV_TOGGLE_HIDDEN,
                    json!({
                        "error": format!("files_mode init failed: {e:#}"),
                        "code": "files_mode_init_failed",
                    }),
                ),
                None,
            )]);
        }
    };
    let show_hidden = files_mode.toggle_hidden();

    let rev = session
        .bump("tree.invalidate", json!({ "scope": "files" }))
        .await;

    let res = ToggleHiddenRes { show_hidden };
    Ok(vec![(
        Frame::res(req_id, op::NAV_TOGGLE_HIDDEN, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}

/// Raster mimes we can decode + re-encode for a downsized preview. SVG is vector
/// (handled elsewhere); video/HDF5 come through bounded-output plugins already.
fn is_downsampleable_raster(mime: &str) -> bool {
    matches!(
        mime,
        "image/png" | "image/jpeg" | "image/webp" | "image/bmp" | "image/tiff" | "image/gif"
    )
}

/// If `bytes` is an oversize raster, decode it and — when its longest side
/// exceeds [`PREVIEW_DOWNSAMPLE_MAX_DIM`] — scale it down and re-encode as PNG.
/// Returns `Some(png)` ONLY when it actually shrank the image; `None` (ship raw)
/// when the mime isn't a raster, the file is under the trigger, the dimensions
/// already fit, or decode/encode fails. CPU-bound — call via `spawn_blocking`.
fn downsample_oversize_raster(mime: &str, bytes: &[u8]) -> Option<Vec<u8>> {
    use image::ImageEncoder;
    if bytes.len() <= PREVIEW_DOWNSAMPLE_TRIGGER || !is_downsampleable_raster(mime) {
        return None;
    }
    // Peek dimensions (header only) before committing to a full decode: a big
    // file with modest dimensions ships raw without paying the decode.
    let (w, h) = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;
    if w.max(h) <= PREVIEW_DOWNSAMPLE_MAX_DIM {
        return None;
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::no_limits();
    limits.max_alloc = Some(MAX_PREVIEW_DECODE_ALLOC);
    reader.limits(limits);
    let rgba = reader.decode().ok()?.to_rgba8();
    let scale = PREVIEW_DOWNSAMPLE_MAX_DIM as f32 / w.max(h) as f32;
    let nw = ((w as f32 * scale).floor() as u32).max(1);
    let nh = ((h as f32 * scale).floor() as u32).max(1);
    // `thumbnail` (area-averaging) matches the FE's downsample: fast on huge
    // sources with good quality for a shrink.
    let small = image::imageops::thumbnail(&rgba, nw, nh);
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(small.as_raw(), nw, nh, image::ExtendedColorType::Rgba8)
        .ok()?;
    tracing::info!(
        orig_bytes = bytes.len(),
        orig_w = w,
        orig_h = h,
        new_bytes = out.len(),
        new_w = nw,
        new_h = nh,
        "downsampled oversize raster for preview"
    );
    Some(out)
}

/// Resolve the bytes actually put on the wire: a downsized PNG for an oversize
/// raster, else the input unchanged. Owns its args so it can run in
/// `spawn_blocking` and always hand the (possibly original) bytes back.
fn preview_bytes_for_wire(mime: String, bytes: Vec<u8>) -> (String, Vec<u8>) {
    match downsample_oversize_raster(&mime, &bytes) {
        Some(png) => ("image/png".to_string(), png),
        None => (mime, bytes),
    }
}

pub async fn handle_preview_get(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: PreviewGetReq = serde_json::from_value(payload_json).context("preview.get payload")?;
    tracing::info!(
        node_id = %req.node_id,
        workspace_id = req.workspace_id.as_deref().unwrap_or("<default>"),
        "preview.get"
    );

    let Some(ws) = workspaces.resolve(req.workspace_id.as_deref()) else {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::PREVIEW_GET,
                json!({
                    "error": format!("unknown workspace: {:?}", req.workspace_id),
                    "code": "unknown_workspace",
                }),
            ),
            None,
        )]);
    };
    let files_mode = match ws.files_mode() {
        Ok(fm) => fm,
        Err(e) => {
            return Ok(vec![(
                Frame::res(
                    req_id,
                    op::PREVIEW_GET,
                    json!({
                        "error": format!("files_mode init failed: {e:#}"),
                        "code": "files_mode_init_failed",
                    }),
                ),
                None,
            )]);
        }
    };
    let kernel = ws.kernel();

    let path = match files_mode.node_id_to_path(&req.node_id) {
        Ok(p) => p,
        Err(e) => {
            let payload = json!({
                "error": format!("{e:#}"),
                "code": "bad_node_id",
            });
            return Ok(vec![(Frame::res(req_id, op::PREVIEW_GET, payload), None)]);
        }
    };

    let (mime, bytes, extras) = if path.is_dir() {
        // Directory preview: short markdown stub naming the dir. Frontend
        // would otherwise render an empty pane on dir selection.
        let label = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_else(|| path.to_str().unwrap_or("/"));
        let md = ROOT_PREVIEW_TEMPLATE.replace("{root}", label);
        ("text/markdown".to_string(), md.into_bytes(), None)
    } else if let Some(out) =
        try_plugin_preview(&kernel, &path, &req.node_id, req.page, req.fit_w, req.fit_h).await
    {
        // Plugin-routed preview: a loaded FileType plugin claimed this
        // path. Use the plugin's mime + decoded blob — that's how
        // HDF5Preview, JuliaSource, MarkdownDoc, and any future plugin
        // surface their previews end-to-end.
        out
    } else {
        // No plugin claim (or kernel unavailable / errored — logged in
        // `try_plugin_preview`). Fall back to the bytes-level reader so
        // files outside any plugin's coverage still get served, with
        // mime inferred from extension.
        match read_bytes_preview(&path, &req.node_id) {
            Ok((mime, bytes)) => (mime, bytes, None),
            Err(e) => {
                let payload = json!({
                    "error": format!("read {path:?}: {e}"),
                    "code": "io_error",
                });
                return Ok(vec![(Frame::res(req_id, op::PREVIEW_GET, payload), None)]);
            }
        }
    };

    // Ship an oversize raster as a preview-sized PNG instead of the raw file.
    // Only oversize rasters take the spawn_blocking detour (decode is CPU-heavy
    // and must stay off the async reactor); everything else passes through
    // untouched with zero overhead. A panicking decode is near-impossible (the
    // helper is all `.ok()?`), so a JoinError propagating as a handler error is
    // acceptable and rare.
    let (mime, bytes) =
        if bytes.len() > PREVIEW_DOWNSAMPLE_TRIGGER && is_downsampleable_raster(&mime) {
            tokio::task::spawn_blocking(move || preview_bytes_for_wire(mime, bytes))
                .await
                .context("preview downsample task")?
        } else {
            (mime, bytes)
        };

    let res = PreviewGetRes {
        mime: mime.clone(),
        blob: BlobDescriptor {
            len: bytes.len() as u64,
            mime,
        },
        extras,
    };

    let rev = session
        .bump("preview.served", json!({ "node_id": req.node_id }))
        .await;

    Ok(vec![(
        Frame::res(req_id, op::PREVIEW_GET, serde_json::to_value(res)?).with_rev(rev),
        Some(bytes),
    )])
}

/// Crop a region out of an image node and write it as a PNG under
/// `<workspace_root>/.sot/captures/` (ADR 0022). The crop comes from the
/// *source* file at full fidelity — not a screen grab — so a deep zoom stays
/// sharp. Returns the backend path; the in-pane `claude` reads it directly.
pub async fn handle_image_crop(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: ImageCropReq = serde_json::from_value(payload_json).context("image.crop payload")?;
    tracing::info!(
        node_id = %req.node_id,
        x = req.x, y = req.y, w = req.w, h = req.h,
        workspace_id = req.workspace_id.as_deref().unwrap_or("<default>"),
        "image.crop"
    );

    let err = |code: &str, msg: String| -> Result<HandlerOutput> {
        Ok(vec![(
            Frame::res(
                req_id,
                op::IMAGE_CROP,
                json!({ "error": msg, "code": code }),
            ),
            None,
        )])
    };

    let Some(ws) = workspaces.resolve(req.workspace_id.as_deref()) else {
        return err(
            "unknown_workspace",
            format!("unknown workspace: {:?}", req.workspace_id),
        );
    };
    let files_mode = match ws.files_mode() {
        Ok(fm) => fm,
        Err(e) => return err("files_mode_init_failed", format!("{e:#}")),
    };
    let path = match files_mode.node_id_to_path(&req.node_id) {
        Ok(p) => p,
        Err(e) => return err("bad_node_id", format!("{e:#}")),
    };
    // Only crop things that decode as images.
    if !mime_for_path(&path).starts_with("image/") {
        return err(
            "not_an_image",
            format!("{path:?} is not an image (mime {})", mime_for_path(&path)),
        );
    }

    // Decode + clamp + crop + write happen on a blocking thread: `image::open`
    // and `save` are synchronous CPU+IO and a large decode would otherwise
    // stall the tokio executor (reviewed on PR #9). All inputs are owned
    // into the closure; it returns the clamped rect + source dims, or a
    // (code, message) pair the caller turns into an error frame. The write
    // lands in `<project_root>/.sot/captures/` (watcher-exempt, gitignored);
    // the filename carries the source stem + a microsecond stamp so repeated
    // captures don't clobber.
    let (req_x, req_y, req_w, req_h) = (req.x, req.y, req.w, req.h);
    let captures_dir = ws.project_root.join(".sot").join("captures");
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("image")
        .to_string();
    let path_for_blk = path.clone();
    type CropOk = (std::path::PathBuf, u32, u32, u32, u32, u32, u32);
    let crop_result =
        tokio::task::spawn_blocking(move || -> std::result::Result<CropOk, (String, String)> {
            let img = image::open(&path_for_blk).map_err(|e| {
                (
                    "decode_failed".into(),
                    format!("decode {path_for_blk:?}: {e}"),
                )
            })?;
            let (src_w, src_h) = (img.width(), img.height());
            if src_w == 0 || src_h == 0 {
                return Err((
                    "empty_image".into(),
                    format!("{path_for_blk:?} has zero dimension"),
                ));
            }
            let x = req_x.min(src_w - 1);
            let y = req_y.min(src_h - 1);
            let w = req_w.clamp(1, src_w - x);
            let h = req_h.clamp(1, src_h - y);
            let cropped = img.crop_imm(x, y, w, h);
            std::fs::create_dir_all(&captures_dir)
                .map_err(|e| ("io_error".into(), format!("create {captures_dir:?}: {e}")))?;
            let micros = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_micros())
                .unwrap_or(0);
            let out_path = captures_dir.join(format!("{stem}-roi-{micros}.png"));
            cropped
                .save(&out_path)
                .map_err(|e| ("io_error".into(), format!("write {out_path:?}: {e}")))?;
            Ok((out_path, x, y, w, h, src_w, src_h))
        })
        .await;
    let (out_path, x, y, w, h, src_w, src_h) = match crop_result {
        Ok(Ok(v)) => v,
        Ok(Err((code, msg))) => return err(&code, msg),
        Err(e) => return err("crop_task_panicked", format!("crop task failed: {e}")),
    };

    let rev = session
        .bump("image.cropped", json!({ "node_id": req.node_id }))
        .await;
    let res = ImageCropRes {
        path: out_path.to_string_lossy().into_owned(),
        x,
        y,
        w,
        h,
        src_w,
        src_h,
    };
    Ok(vec![(
        Frame::res(req_id, op::IMAGE_CROP, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}

/// Ask the kernel `file.preview {path}` and decode the response. Returns
/// `Some((mime, bytes))` if a loaded plugin claimed the path; `None` if no
/// plugin matched, the kernel was unreachable, or the response was malformed
/// — in those cases the caller falls back to the bytes-level reader. Errors
/// are logged but not propagated; preview failures should never break the
/// frontend's chrome.
async fn try_plugin_preview(
    kernel: &Kernel,
    path: &std::path::Path,
    node_id: &str,
    page: Option<u32>,
    fit_w: Option<u32>,
    fit_h: Option<u32>,
) -> Option<(String, Vec<u8>, Option<serde_json::Value>)> {
    // Gate on input size before invoking the plugin. Plugins for structured
    // mimes (e.g. `application/vnd.sot.tokens+json` from JuliaSource)
    // produce output proportional to input; if we let the plugin run on a
    // 50 MiB file we'd either have to ship 50 MiB across the wire or
    // truncate the JSON mid-array — both wrong. Skipping the plugin path
    // sends the caller to the bytes-level reader, which truncates safely
    // (text mime, byte-cut at any offset is still valid bytes).
    // Bounded-output plugins (video, HDF5 — see `is_bounded_output_plugin`) are
    // EXEMPT: their payload is bounded regardless of input size, so the
    // input-size-proxies-output-size assumption behind the cap is wrong for
    // them. Without the exemption a real (>2 MiB) video skips the plugin and
    // shows a blank pane, and — the bug this fixes — a multi-GB `.h5` (the whole
    // point of the HDF5 feature) skips the metadata-only plugin and falls back
    // to the bytes reader, which returns raw HDF5 binary.
    match path.metadata() {
        Ok(md) if md.len() as usize > PREVIEW_BYTE_CAP && !is_bounded_output_plugin(path) => {
            tracing::info!(
                %node_id,
                size = md.len(),
                cap = PREVIEW_BYTE_CAP,
                "skipping plugin path on oversize input; falling back to bytes-level reader"
            );
            return None;
        }
        _ => {}
    }
    // Request params ride a nested `params` object (ADR 0021) so the kernel
    // payload stays open for future per-request knobs (dpi, sheet, …)
    // without the backend learning what they mean.
    let mut params = serde_json::Map::new();
    if let Some(p) = page {
        params.insert("page".into(), p.into());
    }
    if let Some(w) = fit_w {
        params.insert("fit_w".into(), w.into());
    }
    if let Some(h) = fit_h {
        params.insert("fit_h".into(), h.into());
    }
    let payload = if params.is_empty() {
        json!({ "path": path.to_string_lossy() })
    } else {
        json!({ "path": path.to_string_lossy(), "params": params })
    };
    let v = match kernel.request("file.preview", payload).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                %node_id,
                error = %e,
                "kernel.file.preview failed; falling back to bytes-level reader"
            );
            return None;
        }
    };
    let matched = v.get("matched").and_then(|m| m.as_bool()).unwrap_or(false);
    if !matched {
        return None;
    }
    let mime = v.get("mime").and_then(|m| m.as_str())?.to_string();
    // Prefer `blob_base64` (canonical, supports binary). Plain `text` is also
    // emitted for text/* mimes — but decoding base64 still gives the right
    // bytes either way, so we route everything through the same path.
    let b64 = v.get("blob_base64").and_then(|b| b.as_str())?;
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    let mut bytes = match STANDARD.decode(b64) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                %node_id,
                error = %e,
                "kernel.file.preview returned undecodable blob_base64; falling back"
            );
            return None;
        }
    };
    if bytes.len() > PREVIEW_BYTE_CAP {
        // Defense in depth: input-size gate above usually catches this, but
        // a plugin could still emit output larger than its input (e.g. token
        // JSON for a dense file). Truncating a structured mime mid-stream
        // corrupts it; fall back instead. `text/*` is the only family that
        // tolerates an arbitrary byte cut.
        if mime.starts_with("text/") {
            bytes.truncate(PREVIEW_BYTE_CAP);
            tracing::warn!(
                %node_id,
                size = bytes.len(),
                mime = %mime,
                "plugin-rendered text preview truncated at PREVIEW_BYTE_CAP"
            );
        } else {
            tracing::warn!(
                %node_id,
                size = bytes.len(),
                mime = %mime,
                cap = PREVIEW_BYTE_CAP,
                "plugin output exceeds cap on non-text mime; falling back to bytes-level reader"
            );
            return None;
        }
    }
    // Plugin-reported metadata (page/page_count, …) — forwarded verbatim,
    // opaque here (ADR 0021).
    let extras = v.get("extras").cloned();
    Some((mime, bytes, extras))
}

/// Bytes-level reader: read the file at `path`, infer mime from extension,
/// truncate text mimes at PREVIEW_BYTE_CAP, refuse oversized binary mimes
/// with a text/plain blurb (truncating a PNG/JPEG mid-stream produces an
/// undecodable file, which is worse than not rendering at all). Used when
/// no plugin claims the path.
fn read_bytes_preview(path: &std::path::Path, node_id: &str) -> std::io::Result<(String, Vec<u8>)> {
    // A video reaching the bytes-level reader means no plugin claimed it —
    // the ShipToolsVideoFile kernel plugin isn't loaded (or the kernel is down).
    // Don't ship the raw container (the frontend can't render it and it may be
    // huge); surface why instead of a silent blank pane.
    if is_streaming_media(path) {
        tracing::warn!(%node_id, "video reached bytes-level reader — video plugin not loaded");
        let msg = "# video preview unavailable\n\nNo plugin decoded this video — the `ShipToolsVideoFile` kernel plugin isn't loaded (or the kernel is down). Ensure the kernel env has it (`Pkg.develop(path=\"julia/plugins/video-file\")`) and that `ffmpeg`/`ffprobe` are on PATH.\n".to_string();
        return Ok(("text/markdown".to_string(), msg.into_bytes()));
    }
    let mime = mime_for_path(path).to_string();
    let mut bytes = std::fs::read(path)?;
    let is_text = mime.starts_with("text/");
    if is_text && bytes.len() > PREVIEW_BYTE_CAP {
        let orig = bytes.len();
        bytes.truncate(PREVIEW_BYTE_CAP);
        tracing::warn!(
            %node_id,
            size = orig,
            "text preview truncated at PREVIEW_BYTE_CAP"
        );
    } else if !is_text && bytes.len() > PREVIEW_BINARY_CAP {
        let mib = bytes.len() as f64 / (1024.0 * 1024.0);
        let cap_mib = PREVIEW_BINARY_CAP / (1024 * 1024);
        let msg = format!(
            "# preview too large\n\nfile is {mib:.1} MiB; binary preview cap is {cap_mib} MiB.\n\nbinary mimes can't be safely truncated — a partial PNG/JPEG won't decode.\n",
        );
        tracing::warn!(
            %node_id,
            size = bytes.len(),
            mime = %mime,
            "binary preview refused: exceeds PREVIEW_BINARY_CAP"
        );
        return Ok(("text/markdown".to_string(), msg.into_bytes()));
    }
    Ok((mime, bytes))
}

pub async fn handle_concept_read(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: ConceptReadReq =
        serde_json::from_value(payload_json).context("concept.read payload")?;
    tracing::info!(
        target = %req.target,
        workspace_id = req.workspace_id.as_deref().unwrap_or("<default>"),
        "concept.read"
    );

    let Some(ws) = workspaces.resolve(req.workspace_id.as_deref()) else {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::CONCEPT_READ,
                json!({
                    "error": format!("unknown workspace: {:?}", req.workspace_id),
                    "code": "unknown_workspace",
                }),
            ),
            None,
        )]);
    };
    let concept = ws.concept();
    let (exists, content) = match concept.read(&req.target) {
        Ok(v) => v,
        Err(e) => {
            let payload = json!({
                "error": format!("{e:#}"),
                "code": "concept_read_failed",
                "target": req.target,
            });
            return Ok(vec![(Frame::res(req_id, op::CONCEPT_READ, payload), None)]);
        }
    };

    let res = ConceptReadRes {
        target: req.target.clone(),
        exists,
        content,
    };
    let (_, rev) = session.snapshot().await;
    Ok(vec![(
        Frame::res(req_id, op::CONCEPT_READ, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}

pub async fn handle_concept_write(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: ConceptWriteReq =
        serde_json::from_value(payload_json).context("concept.write payload")?;
    tracing::info!(
        target = %req.target,
        len = req.content.len(),
        expected_set = req.expected_ast_hash.is_some(),
        workspace_id = req.workspace_id.as_deref().unwrap_or("<default>"),
        "concept.write"
    );

    let Some(ws) = workspaces.resolve(req.workspace_id.as_deref()) else {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::CONCEPT_WRITE,
                json!({
                    "error": format!("unknown workspace: {:?}", req.workspace_id),
                    "code": "unknown_workspace",
                }),
            ),
            None,
        )]);
    };
    let concept = ws.concept();

    // Optimistic-concurrency check: if the client passed
    // `expected_ast_hash`, compare it against the on-disk annotation's
    // frontmatter `synced_against`. The check exists so a stale frontend
    // doesn't silently clobber an annotation that was already updated to
    // track a newer entity hash. If `expected_ast_hash` is None or the
    // on-disk file has no frontmatter, the write proceeds (phase-1 back-
    // compat).
    if let Some(expected) = req.expected_ast_hash.as_deref() {
        match concept.read_synced_against(&req.target) {
            Ok(Some(actual)) if actual != expected => {
                let payload = json!({
                    "error": "stale write: on-disk synced_against differs from expected",
                    "code": "stale_write",
                    "target": req.target,
                    "expected": expected,
                    "actual": actual,
                });
                return Ok(vec![(Frame::res(req_id, op::CONCEPT_WRITE, payload), None)]);
            }
            Ok(_) => {} // no frontmatter or no field on disk — nothing to be stale against
            Err(e) => {
                // I/O failure reading the on-disk file — surface as a distinct
                // failure code so the client can distinguish from a true
                // stale_write.
                let payload = json!({
                    "error": format!("{e:#}"),
                    "code": "concept_read_for_check_failed",
                    "target": req.target,
                });
                return Ok(vec![(Frame::res(req_id, op::CONCEPT_WRITE, payload), None)]);
            }
        }
    }

    let (path, written) = match concept.write(&req.target, &req.content) {
        Ok(v) => v,
        Err(e) => {
            let payload = json!({
                "error": format!("{e:#}"),
                "code": "concept_write_failed",
                "target": req.target,
            });
            return Ok(vec![(Frame::res(req_id, op::CONCEPT_WRITE, payload), None)]);
        }
    };

    let res = ConceptWriteRes {
        target: req.target.clone(),
        path: path.to_string_lossy().to_string(),
        written,
    };
    // Annotation writes mutate the project, so bump the session revision —
    // a reconnecting client wants to know a concept file changed.
    let rev = session
        .bump("concept.written", json!({ "target": req.target }))
        .await;
    Ok(vec![(
        Frame::res(req_id, op::CONCEPT_WRITE, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}

/// Read a source file's full text for the in-frontend editor. Unlike
/// `preview.get` (kernel-rendered), this is raw backend byte IO — no kernel
/// dependency — returning the text plus a content `version` for the matching
/// conflict-aware `file.write`.
pub async fn handle_file_read(
    req_id: u64,
    payload_json: serde_json::Value,
    _session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: FileReadReq = serde_json::from_value(payload_json).context("file.read payload")?;
    tracing::info!(
        node_id = %req.node_id,
        workspace_id = req.workspace_id.as_deref().unwrap_or("<default>"),
        "file.read"
    );

    let path = match resolve_file_node(
        op::FILE_READ,
        req_id,
        &req.node_id,
        req.workspace_id.as_deref(),
        workspaces,
        false,
    ) {
        Ok(p) => p,
        Err(out) => return Ok(out),
    };

    match file_io::read_file(&path) {
        Ok(Some(r)) => {
            let res = FileReadRes {
                node_id: req.node_id,
                exists: true,
                content: r.content,
                version: r.version,
            };
            Ok(vec![(
                Frame::res(req_id, op::FILE_READ, serde_json::to_value(res)?),
                None,
            )])
        }
        Ok(None) => {
            let res = FileReadRes {
                node_id: req.node_id,
                exists: false,
                content: String::new(),
                version: String::new(),
            };
            Ok(vec![(
                Frame::res(req_id, op::FILE_READ, serde_json::to_value(res)?),
                None,
            )])
        }
        Err(e) => Ok(vec![(
            Frame::res(
                req_id,
                op::FILE_READ,
                json!({ "error": format!("{e:#}"), "code": "file_read_failed", "node_id": req.node_id }),
            ),
            None,
        )]),
    }
}

/// Write a source file from the in-frontend editor with optimistic concurrency.
/// When `expected_version` is set and the on-disk content has changed since the
/// matching `file.read`, the write is refused with `code: "conflict"` and the
/// response carries the current on-disk content/version for reconciliation.
pub async fn handle_file_write(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: FileWriteReq = serde_json::from_value(payload_json).context("file.write payload")?;
    tracing::info!(
        node_id = %req.node_id,
        len = req.content.len(),
        expected_set = req.expected_version.is_some(),
        workspace_id = req.workspace_id.as_deref().unwrap_or("<default>"),
        "file.write"
    );

    let path = match resolve_file_node(
        op::FILE_WRITE,
        req_id,
        &req.node_id,
        req.workspace_id.as_deref(),
        workspaces,
        true,
    ) {
        Ok(p) => p,
        Err(out) => return Ok(out),
    };

    match file_io::write_file(&path, &req.content, req.expected_version.as_deref()) {
        Ok(WriteResult::Written { version }) => {
            let res = FileWriteRes {
                node_id: req.node_id.clone(),
                path: path.to_string_lossy().to_string(),
                version,
                written: req.content.len() as u64,
            };
            // A source write mutates the project; bump the revision so a
            // reconnecting client (and the file-watcher consumers) know.
            let rev = session
                .bump("file.written", json!({ "node_id": req.node_id }))
                .await;
            Ok(vec![(
                Frame::res(req_id, op::FILE_WRITE, serde_json::to_value(res)?).with_rev(rev),
                None,
            )])
        }
        Ok(WriteResult::Conflict {
            current_content,
            current_version,
        }) => Ok(vec![(
            Frame::res(
                req_id,
                op::FILE_WRITE,
                json!({
                    "error": "conflict: on-disk content changed since read",
                    "code": "conflict",
                    "node_id": req.node_id,
                    "current_content": current_content,
                    "current_version": current_version,
                }),
            ),
            None,
        )]),
        Err(e) => Ok(vec![(
            Frame::res(
                req_id,
                op::FILE_WRITE,
                json!({ "error": format!("{e:#}"), "code": "file_write_failed", "node_id": req.node_id }),
            ),
            None,
        )]),
    }
}

/// Trash a file from Files-mode nav (FE Ctrl+D). v1 contract: directories are
/// refused (`code: "is_directory"`) and nothing is ever hard-unlinked —
/// `file_io::trash_file` goes to the system trash (`gio trash`) or falls back
/// to `<workspace_root>/.sot-trash/` (the response's `trash_path` says
/// which). Bumps the session revision like file.write so the watcher and
/// reconnecting clients refresh.
pub async fn handle_file_delete(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: FileDeleteReq = serde_json::from_value(payload_json).context("file.delete payload")?;
    tracing::info!(
        node_id = %req.node_id,
        workspace_id = req.workspace_id.as_deref().unwrap_or("<default>"),
        "file.delete"
    );

    let path = match resolve_file_node(
        op::FILE_DELETE,
        req_id,
        &req.node_id,
        req.workspace_id.as_deref(),
        workspaces,
        true,
    ) {
        Ok(p) => p,
        Err(out) => return Ok(out),
    };

    // symlink_metadata: a symlink *to* a directory is still trashable as a
    // file (we move the link, never its target); only real directories are
    // refused in v1.
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) => {
            return Ok(vec![(
                Frame::res(
                    req_id,
                    op::FILE_DELETE,
                    json!({ "error": format!("{e:#}"), "code": "not_found", "node_id": req.node_id }),
                ),
                None,
            )]);
        }
    };
    if meta.is_dir() {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::FILE_DELETE,
                json!({ "error": "directories are not deletable in v1", "code": "is_directory", "node_id": req.node_id }),
            ),
            None,
        )]);
    }

    // resolve_file_node already validated the workspace; re-resolve for the
    // project root the fallback trash dir lives under.
    let Some(ws) = workspaces.resolve(req.workspace_id.as_deref()) else {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::FILE_DELETE,
                json!({ "error": format!("unknown workspace: {:?}", req.workspace_id), "code": "unknown_workspace" }),
            ),
            None,
        )]);
    };

    match file_io::trash_file(&path, &ws.project_root) {
        Ok(trash_path) => {
            let res = FileDeleteRes {
                node_id: req.node_id.clone(),
                path: path.to_string_lossy().to_string(),
                trashed: true,
                trash_path: trash_path.map(|p| p.to_string_lossy().to_string()),
            };
            let rev = session
                .bump("file.deleted", json!({ "node_id": req.node_id }))
                .await;
            Ok(vec![(
                Frame::res(req_id, op::FILE_DELETE, serde_json::to_value(res)?).with_rev(rev),
                None,
            )])
        }
        Err(e) => Ok(vec![(
            Frame::res(
                req_id,
                op::FILE_DELETE,
                json!({ "error": format!("{e:#}"), "code": "file_delete_failed", "node_id": req.node_id }),
            ),
            None,
        )]),
    }
}

/// Shared workspace → FilesMode → safe path resolution for the file.read /
/// file.write / file.delete handlers. On any failure returns the error
/// `HandlerOutput` to send back (tagged with `op`); on success returns the
/// resolved absolute path. Both resolvers reject `..`/absolute ids;
/// `confined` selects the WRITE resolver (`node_id_to_path_confined`, the
/// symlink escape guard — mutations can't leave the project root) vs the
/// READ resolver (follows user symlinks, e.g. NAS mounts — see
/// files_mode.rs).
fn resolve_file_node(
    op_name: &'static str,
    req_id: u64,
    node_id: &str,
    workspace_id: Option<&str>,
    workspaces: &Workspaces,
    confined: bool,
) -> std::result::Result<std::path::PathBuf, HandlerOutput> {
    let Some(ws) = workspaces.resolve(workspace_id) else {
        return Err(vec![(
            Frame::res(
                req_id,
                op_name,
                json!({ "error": format!("unknown workspace: {workspace_id:?}"), "code": "unknown_workspace" }),
            ),
            None,
        )]);
    };
    let files_mode = match ws.files_mode() {
        Ok(fm) => fm,
        Err(e) => {
            return Err(vec![(
                Frame::res(
                    req_id,
                    op_name,
                    json!({ "error": format!("files_mode init failed: {e:#}"), "code": "files_mode_init_failed" }),
                ),
                None,
            )]);
        }
    };
    let resolved = if confined {
        files_mode.node_id_to_path_confined(node_id)
    } else {
        files_mode.node_id_to_path(node_id)
    };
    match resolved {
        Ok(p) => Ok(p),
        Err(e) => Err(vec![(
            Frame::res(
                req_id,
                op_name,
                json!({ "error": format!("{e:#}"), "code": "bad_node_id" }),
            ),
            None,
        )]),
    }
}

pub async fn handle_concept_list(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let workspace_id = payload_json
        .get("workspace_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    tracing::info!(
        workspace_id = workspace_id.as_deref().unwrap_or("<default>"),
        "concept.list"
    );
    let Some(ws) = workspaces.resolve(workspace_id.as_deref()) else {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::CONCEPT_LIST,
                json!({
                    "error": format!("unknown workspace: {:?}", workspace_id),
                    "code": "unknown_workspace",
                }),
            ),
            None,
        )]);
    };
    let concept = ws.concept();
    let targets = match concept.list() {
        Ok(v) => v,
        Err(e) => {
            let payload = json!({
                "error": format!("{e:#}"),
                "code": "concept_list_failed",
            });
            return Ok(vec![(Frame::res(req_id, op::CONCEPT_LIST, payload), None)]);
        }
    };
    let res = ConceptListRes { targets };
    let (_, rev) = session.snapshot().await;
    Ok(vec![(
        Frame::res(req_id, op::CONCEPT_LIST, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}

pub async fn handle_repl_eval(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let workspace_id = payload_json
        .get("workspace_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    let eval_id = payload_json.get("eval_id").and_then(|v| v.as_u64());
    tracing::info!(
        workspace_id = workspace_id.as_deref().unwrap_or("<default>"),
        eval_id,
        "repl.eval"
    );
    let Some(ws) = workspaces.resolve(workspace_id.as_deref()) else {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::REPL_EVAL,
                json!({
                    "error": format!("unknown workspace: {:?}", workspace_id),
                    "code": "unknown_workspace",
                }),
            ),
            None,
        )]);
    };
    let repl = ws.repl(workspaces.repl_frame_tx());
    // Fire-and-forget: queue the eval at the supervisor and return an ack
    // immediately. The eval's frames (stdout/value/error/done) stream as
    // separate `repl.frame` evts over the broadcast bus; the frontend keys
    // completion off the terminal `done` frame, not this ack. Returning here
    // (instead of awaiting the eval) keeps the connection loop free to read a
    // mid-eval `repl.interrupt`.
    if let Err(e) = repl.submit(op::REPL_EVAL, payload_json.clone()).await {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::REPL_EVAL,
                json!({
                    "error": format!("{e:#}"),
                    "code": "repl_eval_failed",
                }),
            ),
            None,
        )]);
    }
    let (_, rev) = session.snapshot().await;
    Ok(vec![(
        Frame::res(
            req_id,
            op::REPL_EVAL,
            // Full ReplEvalRes shape (frames:[] + elapsed_ms) so the FE
            // deserializes cleanly and its empty-frames guard fires (content
            // already streamed via repl.frame evts). Real elapsed_ms rides the
            // done frame.
            json!({ "eval_id": eval_id, "elapsed_ms": 0, "frames": [], "accepted": true }),
        )
        .with_rev(rev),
        None,
    )])
}

pub async fn handle_repl_run_file(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let workspace_id = payload_json
        .get("workspace_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    let fresh = payload_json
        .get("fresh")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let path_str = payload_json
        .get("path")
        .and_then(|v| v.as_str())
        .map(String::from);
    let eval_id = payload_json.get("eval_id").and_then(|v| v.as_u64());
    tracing::info!(
        workspace_id = workspace_id.as_deref().unwrap_or("<default>"),
        fresh,
        eval_id,
        path = path_str.as_deref().unwrap_or(""),
        "repl.run_file"
    );
    let Some(ws) = workspaces.resolve(workspace_id.as_deref()) else {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::REPL_RUN_FILE,
                json!({
                    "error": format!("unknown workspace: {:?}", workspace_id),
                    "code": "unknown_workspace",
                }),
            ),
            None,
        )]);
    };
    let repl = ws.repl(workspaces.repl_frame_tx());

    // Priority J: `r` in NavTree maps to fresh=true. Resolve the file's
    // closest-ancestor Project.toml *here* on Rust, bounce the REPL
    // supervisor into that project, then forward a fresh=false submission
    // so the Julia side just `include`s in the now-correct env. The
    // dead-code subprocess branch in ShipToolsRepl.handle_run_file never
    // runs anymore (commented in the Julia source).
    //
    // With streamed frames (Option B), the run no longer rides a single
    // response: the Julia child emits the include's stdout/value/error/done
    // as `repl.frame` evts over the broadcast bus, and this handler returns
    // only an immediate ack. The reset banner that previously rode the
    // response as a synthetic stderr frame is now the Julia shim's job to
    // emit as a frame (or it shows up implicitly via the fresh env), so the
    // Rust-side response post-processing (project_dir override, banner
    // prepend) is gone — there's no response payload to fold it into.
    let mut forwarded_payload = payload_json.clone();
    // Captured for the ack so the FE's fresh-`r` status line can show the
    // project the file was bounced into. Only resolved for fresh runs; a
    // fresh=false include leaves them None → FE degrades to "(no project)".
    let mut project_dir_str: Option<String> = None;
    let mut project_source_str: Option<String> = None;
    if fresh {
        let Some(ref p) = path_str else {
            return Ok(vec![(
                Frame::res(
                    req_id,
                    op::REPL_RUN_FILE,
                    json!({
                        "error": "fresh=true requires a path",
                        "code": "bad_request",
                    }),
                ),
                None,
            )]);
        };
        let abs_path = {
            let pb = std::path::PathBuf::from(p);
            if pb.is_absolute() {
                pb
            } else {
                std::env::current_dir().unwrap_or_default().join(pb)
            }
        };
        let (project_dir, project_source) =
            closest_project_dir(&abs_path).unwrap_or_else(|| (ws.project_root.clone(), "fallback"));
        project_dir_str = Some(project_dir.display().to_string());
        project_source_str = Some(project_source.to_string());
        if let Err(e) = repl.restart_with_project(&project_dir).await {
            return Ok(vec![(
                Frame::res(
                    req_id,
                    op::REPL_RUN_FILE,
                    json!({
                        "error": format!("repl restart failed: {e:#}"),
                        "code": "repl_restart_failed",
                        "project_dir": project_dir.display().to_string(),
                        "project_source": project_source,
                    }),
                ),
                None,
            )]);
        }
        // Rewrite the forwarded payload to ask the Julia side for a plain
        // include — we've already done the env bounce here.
        if let Some(obj) = forwarded_payload.as_object_mut() {
            obj.insert("fresh".to_string(), serde_json::Value::Bool(false));
        }
    }

    // Fire-and-forget: queue the run and ack immediately. Frames stream as
    // `repl.frame` evts; the frontend keys completion off the `done` frame.
    if let Err(e) = repl.submit(op::REPL_RUN_FILE, forwarded_payload).await {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::REPL_RUN_FILE,
                json!({
                    "error": format!("{e:#}"),
                    "code": "repl_run_file_failed",
                }),
            ),
            None,
        )]);
    }
    let (_, rev) = session.snapshot().await;
    Ok(vec![(
        Frame::res(
            req_id,
            op::REPL_RUN_FILE,
            // Full ReplRunFileRes shape (frames:[] + required fields) so the
            // FE deserializes it cleanly and its empty-frames guard fires
            // (content already streamed via repl.frame evts). Real elapsed_ms
            // rides the done frame; project_dir/source drive the fresh-`r`
            // status line.
            json!({
                "eval_id": eval_id,
                "path": path_str.clone().unwrap_or_default(),
                "fresh": fresh,
                "elapsed_ms": 0,
                "project_dir": project_dir_str,
                "project_source": project_source_str,
                "frames": [],
                "accepted": true
            }),
        )
        .with_rev(rev),
        None,
    )])
}

/// Walk up from `path`'s parent looking for the nearest `Project.toml`.
/// Returns `(dir, "discovered")` if found, `None` to let the caller
/// fall back. Mirrors the kernel's `discover_project` shape so behavior
/// matches what the frontend sees from `kernel.request project.discover`.
fn closest_project_dir(path: &std::path::Path) -> Option<(std::path::PathBuf, &'static str)> {
    let mut dir = path.parent()?.to_path_buf();
    loop {
        if dir.join("Project.toml").is_file() {
            return Some((dir, "discovered"));
        }
        if !dir.pop() {
            return None;
        }
    }
}

pub async fn handle_repl_interrupt(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let workspace_id = payload_json
        .get("workspace_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    tracing::info!(
        workspace_id = workspace_id.as_deref().unwrap_or("<default>"),
        "repl.interrupt"
    );
    let Some(ws) = workspaces.resolve(workspace_id.as_deref()) else {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::REPL_INTERRUPT,
                json!({
                    "error": format!("unknown workspace: {:?}", workspace_id),
                    "code": "unknown_workspace",
                }),
            ),
            None,
        )]);
    };
    let repl = ws.repl(workspaces.repl_frame_tx());
    let result = repl.request("repl.interrupt", payload_json).await;
    let (_, rev) = session.snapshot().await;
    let payload = match result {
        Ok(v) => v,
        Err(e) => json!({
            "error": format!("{e:#}"),
            "code": "repl_interrupt_failed",
        }),
    };
    Ok(vec![(
        Frame::res(req_id, op::REPL_INTERRUPT, payload).with_rev(rev),
        None,
    )])
}

/// Backend-issued eval_id space for `repl.execute` runs (ADR 0033). Starts at
/// 2^40 so it never collides with a frontend's small per-workspace
/// `repl.eval` counter, while staying a positive integer well under 2^53 (safe
/// for JSON/`jq` consumers) — unlike a high-bit-set id. The `run_id` string
/// returned to the caller is derived from it.
static EXEC_EVAL_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1 << 40);

fn next_exec_eval_id() -> u64 {
    EXEC_EVAL_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

const EXEC_DEFAULT_TIMEOUT_MS: u64 = 120_000;
const EXEC_MIN_TIMEOUT_MS: u64 = 1_000;
const EXEC_MAX_TIMEOUT_MS: u64 = 1_800_000;
/// Per-field inline cap for `value` / `error` text — stdout/stderr are already
/// bounded by `EXEC_TEXT_CAP` in the collector; this guards against one giant
/// `show` repr blowing the 1 MiB envelope.
const EXEC_FIELD_CAP: usize = 64 * 1024;

fn exec_truncate_field(s: &mut String) {
    if s.len() > EXEC_FIELD_CAP {
        let mut cut = EXEC_FIELD_CAP;
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push_str("\n…[truncated]");
    }
}

fn exec_mime_ext(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/svg+xml" => "svg",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        _ => "bin",
    }
}

fn exec_err_frame(req_id: u64, run_id: &str, ws_id: &str, outcome: &str, msg: String) -> HandlerOutput {
    let res = ReplExecuteRes {
        run_id: run_id.to_string(),
        workspace_id: ws_id.to_string(),
        outcome: outcome.to_string(),
        elapsed_ms: 0,
        stdout: String::new(),
        stderr: String::new(),
        values: Vec::new(),
        error: Some(ReplErrorOut {
            message: msg,
            stacktrace: Vec::new(),
        }),
        figures: Vec::new(),
        truncated: false,
        project_dir: None,
        project_source: None,
    };
    vec![(
        Frame::res(
            req_id,
            op::REPL_EXECUTE,
            serde_json::to_value(res).unwrap_or_else(|_| json!({})),
        ),
        None,
    )]
}

/// `repl.execute` (ADR 0033): run a `.jl` file (or code chunk) in a workspace's
/// persistent REPL and return the COLLECTED output as one authoritative
/// response. See `op::REPL_EXECUTE`. The output is gathered off a dedicated
/// per-run collector in the supervisor (loss-free, unlike the broadcast bus),
/// completion keys off the shim's terminal `res` (reliable even when no `done`
/// frame is emitted), figures spill to `<ws>/.sot/runs/<run_id>/`, and a
/// timeout returns `outcome:"timeout"` WITHOUT interrupting the run.
pub async fn handle_repl_execute(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: ReplExecuteReq = match serde_json::from_value(payload_json) {
        Ok(r) => r,
        Err(e) => {
            return Ok(vec![(
                Frame::res(
                    req_id,
                    op::REPL_EXECUTE,
                    json!({ "error": format!("bad repl.execute payload: {e}"), "code": "bad_request" }),
                ),
                None,
            )]);
        }
    };

    let eval_id = next_exec_eval_id();
    let run_id = format!("exec-{eval_id}");
    let ws_id = req.workspace_id.clone();
    tracing::info!(workspace_id = %ws_id, run_id = %run_id, "repl.execute");

    let Some(ws) = workspaces.resolve(Some(ws_id.as_str())) else {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::REPL_EXECUTE,
                json!({ "error": format!("unknown workspace: {ws_id}"), "code": "unknown_workspace" }),
            ),
            None,
        )]);
    };

    // Build the inner op + payload + drawer display; validate a run_file path.
    let (inner_op, inner_payload, display) = match &req.input {
        ReplExecuteInput::RunFile { path } => {
            let joined = {
                let pb = std::path::PathBuf::from(path);
                if pb.is_absolute() {
                    pb
                } else {
                    ws.project_root.join(pb)
                }
            };
            let abs = match joined.canonicalize() {
                Ok(p) => p,
                Err(e) => {
                    return Ok(exec_err_frame(
                        req_id,
                        &run_id,
                        &ws_id,
                        "error",
                        format!("cannot resolve path {path:?}: {e}"),
                    ))
                }
            };
            let root = ws.project_root.canonicalize().unwrap_or_else(|_| ws.project_root.clone());
            if !abs.starts_with(&root) {
                return Ok(exec_err_frame(
                    req_id,
                    &run_id,
                    &ws_id,
                    "error",
                    format!("path {} is outside workspace {}", abs.display(), root.display()),
                ));
            }
            if !abs.is_file() || abs.extension().and_then(|s| s.to_str()) != Some("jl") {
                return Ok(exec_err_frame(
                    req_id,
                    &run_id,
                    &ws_id,
                    "error",
                    format!("not an existing .jl file: {}", abs.display()),
                ));
            }
            let disp = format!(
                "run {}",
                abs.file_name().and_then(|s| s.to_str()).unwrap_or("?.jl")
            );
            (
                op::REPL_RUN_FILE,
                json!({
                    "eval_id": eval_id,
                    "path": abs.to_string_lossy(),
                    "fresh": false,
                    "workspace_id": ws_id,
                }),
                disp,
            )
        }
        ReplExecuteInput::Eval { code, mode } => {
            let mut p = json!({ "eval_id": eval_id, "code": code, "workspace_id": ws_id });
            if let Some(m) = mode {
                if let Some(obj) = p.as_object_mut() {
                    obj.insert("mode".to_string(), json!(m));
                }
            }
            let first = code.lines().next().unwrap_or("").trim();
            let disp = if first.chars().count() > 60 {
                format!("{}…", first.chars().take(60).collect::<String>())
            } else {
                first.to_string()
            };
            (op::REPL_EVAL, p, disp)
        }
    };

    // Phase 2 (ADR 0033): broadcast a `started` control frame so an attached
    // front-end pre-registers this run in the user's drawer (submission order),
    // then routes the streamed output frames + terminal `done` to that entry.
    // Stamp the workspace SLUG, not the canonical `workspace_id`: the FE keys its
    // active workspace + repl snapshots by slug (`current_workspace_key()`), and
    // the `started` handler is the one place that compares the frame's ws against
    // that key to pick which drawer the entry pre-registers in. Output frames
    // route by `eval_id` (their ws hint is ignored), so they still land on the
    // same entry. Stamping the canonical id here made that compare never match →
    // the entry was dropped down the "no snapshot" path and every session run
    // orphaned as "repl.frame dropped: no in-flight entry".
    let origin = req.origin.clone().unwrap_or_else(|| "session".to_string());
    let frame_ws = ws.slug.clone();
    let frame_tx = workspaces.repl_frame_tx();
    let _ = frame_tx.send(ReplFrameMsg {
        eval_id,
        workspace_id: Some(frame_ws.clone()),
        frame: json!({
            "kind": "started",
            "run_id": run_id.clone(),
            "origin": origin,
            "display": display,
        }),
    });

    let repl = ws.repl(workspaces.repl_frame_tx());
    let (reply_rx, collector) = match repl.execute(inner_op, inner_payload).await {
        Ok(x) => x,
        Err(e) => {
            return Ok(exec_err_frame(
                req_id,
                &run_id,
                &ws_id,
                "repl_died",
                format!("repl submit failed: {e:#}"),
            ))
        }
    };

    let timeout_ms = req
        .timeout_ms
        .unwrap_or(EXEC_DEFAULT_TIMEOUT_MS)
        .clamp(EXEC_MIN_TIMEOUT_MS, EXEC_MAX_TIMEOUT_MS);
    let start = std::time::Instant::now();
    let awaited = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), reply_rx).await;
    let elapsed_ms = start.elapsed().as_millis() as u64;

    // Base terminal state from the await. On timeout we deliberately do NOT
    // send an interrupt (that could race and kill a subsequent user eval — the
    // run keeps going and its frames still reach the drawer).
    let (base_outcome, res_payload): (&str, Option<serde_json::Value>) = match awaited {
        Ok(Ok(Ok(v))) => ("completed", Some(v)),
        Ok(Ok(Err(_))) => ("repl_died", None),
        Ok(Err(_)) => ("repl_died", None),
        Err(_) => ("timeout", None),
    };

    // Snapshot the loss-free collector.
    let (frames, truncated) = {
        let acc = collector.lock().unwrap_or_else(|e| e.into_inner());
        (acc.frames.clone(), acc.truncated)
    };

    // Terminal error carried by the shim's res (bad_request / io_error /
    // repl_exception) — authoritative over frame inspection.
    let mut res_code_error = false;
    let mut error_out: Option<ReplErrorOut> = None;
    let mut project_dir: Option<String> = None;
    let mut project_source: Option<String> = None;
    if let Some(res) = &res_payload {
        project_dir = res.get("project_dir").and_then(|v| v.as_str()).map(String::from);
        project_source = res.get("project_source").and_then(|v| v.as_str()).map(String::from);
        if let Some(code) = res.get("code").and_then(|v| v.as_str()) {
            res_code_error = true;
            let msg = res.get("error").and_then(|v| v.as_str()).unwrap_or(code).to_string();
            error_out = Some(ReplErrorOut { message: msg, stacktrace: Vec::new() });
        }
    }

    // Split collected frames.
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut values: Vec<ReplValueOut> = Vec::new();
    let mut image_frames: Vec<(String, String)> = Vec::new();
    let mut frame_error_kind: Option<&str> = None;
    for f in &frames {
        match f.get("kind").and_then(|v| v.as_str()) {
            Some("stdout") => {
                if let Some(t) = f.get("text").and_then(|v| v.as_str()) {
                    stdout.push_str(t);
                }
            }
            Some("stderr") => {
                if let Some(t) = f.get("text").and_then(|v| v.as_str()) {
                    stderr.push_str(t);
                }
            }
            Some("value") => {
                let mime = f.get("mime").and_then(|v| v.as_str()).unwrap_or("text/plain").to_string();
                let mut text = f.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
                exec_truncate_field(&mut text);
                values.push(ReplValueOut { mime, text });
            }
            Some("image") => {
                let mime = f.get("mime").and_then(|v| v.as_str()).unwrap_or("image/png").to_string();
                if let Some(b64) = f.get("data_base64").and_then(|v| v.as_str()) {
                    image_frames.push((mime, b64.to_string()));
                }
            }
            Some("error") => {
                let message = f.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let k = if message.contains("REPL busy") {
                    "busy"
                } else if message.contains("InterruptException") {
                    "interrupted"
                } else {
                    "error"
                };
                // Strongest-wins: busy > interrupted > error.
                frame_error_kind = Some(match (frame_error_kind, k) {
                    (Some("busy"), _) | (_, "busy") => "busy",
                    (Some("interrupted"), _) | (_, "interrupted") => "interrupted",
                    _ => "error",
                });
                if error_out.is_none() {
                    let stack: Vec<StackFrame> = f
                        .get("stacktrace")
                        .cloned()
                        .and_then(|v| serde_json::from_value(v).ok())
                        .unwrap_or_default();
                    let mut msg = message.clone();
                    exec_truncate_field(&mut msg);
                    error_out = Some(ReplErrorOut { message: msg, stacktrace: stack });
                }
            }
            _ => {}
        }
    }

    // Final outcome precedence: timeout / repl_died (from the await) win, then a
    // shim res error code, then frame classification (busy > interrupted >
    // error), else ok.
    let outcome: &str = match base_outcome {
        "timeout" => "timeout",
        "repl_died" => "repl_died",
        _ if res_code_error => "error",
        _ => frame_error_kind.unwrap_or("ok"),
    };

    // Phase 2: finalize the drawer entry for outcomes where the shim's own
    // `done` frame won't arrive — timeout (the run is still going) or repl_died
    // (child gone). For ok/error/busy the shim already emitted `done`.
    if outcome == "timeout" || outcome == "repl_died" {
        let _ = frame_tx.send(ReplFrameMsg {
            eval_id,
            workspace_id: Some(frame_ws.clone()),
            frame: json!({ "kind": "done", "eval_id": eval_id, "elapsed_ms": elapsed_ms }),
        });
    }

    // Spill figures to files so the response never inlines base64 (1 MiB cap).
    let mut figures: Vec<String> = Vec::new();
    if !image_frames.is_empty() {
        let runs_dir = ws.project_root.join(".sot").join("runs").join(&run_id);
        let run_id_blk = run_id.clone();
        let spill = tokio::task::spawn_blocking(move || -> std::result::Result<Vec<String>, String> {
            use base64::engine::general_purpose::STANDARD;
            use base64::Engine as _;
            std::fs::create_dir_all(&runs_dir).map_err(|e| format!("create {runs_dir:?}: {e}"))?;
            let mut out = Vec::new();
            for (i, (mime, b64)) in image_frames.iter().enumerate() {
                let bytes = STANDARD.decode(b64).map_err(|e| format!("fig {i} base64: {e}"))?;
                let p = runs_dir.join(format!("fig-{i}.{}", exec_mime_ext(mime)));
                std::fs::write(&p, &bytes).map_err(|e| format!("write {p:?}: {e}"))?;
                out.push(p.to_string_lossy().into_owned());
            }
            Ok(out)
        })
        .await;
        match spill {
            Ok(Ok(paths)) => figures = paths,
            Ok(Err(e)) => tracing::warn!(run_id = %run_id_blk, "figure spill failed: {e}"),
            Err(e) => tracing::warn!(run_id = %run_id_blk, "figure spill task panicked: {e}"),
        }
    }

    let res = ReplExecuteRes {
        run_id: run_id.clone(),
        workspace_id: ws_id.clone(),
        outcome: outcome.to_string(),
        elapsed_ms,
        stdout,
        stderr,
        values,
        error: error_out,
        figures,
        truncated,
        project_dir,
        project_source,
    };
    let (_, rev) = session.snapshot().await;
    Ok(vec![(
        Frame::res(req_id, op::REPL_EXECUTE, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}

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
        Err(e) => json!({
            "error": format!("{e:#}"),
            "code": "kernel_request_failed",
            "kernel_op": req.kernel_op,
        }),
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

/// Canonicalizes `path` and returns it if it resolves under `root`'s
/// canonical form — `None` on any canonicalization failure (missing path,
/// dangling symlink, ...) or if it escapes `root`.
fn canonical_under_root(
    path: &std::path::Path,
    root: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let canon_path = path.canonicalize().ok()?;
    let canon_root = root.canonicalize().ok()?;
    canon_path.starts_with(&canon_root).then_some(canon_path)
}

/// Confines `path` to ANY currently-registered workspace, not just the
/// default one (a non-default-workspace open would otherwise be wrongly
/// rejected) — the guard shared by `pluto.open` and `docs.open` (security
/// review). Returns the canonical path; callers MUST use this value for
/// everything downstream rather than re-deriving from the raw input, so the
/// checked path and the acted-upon path can't diverge (TOCTOU).
fn canonicalize_within_any_workspace(
    path: &std::path::Path,
    workspaces: &Workspaces,
) -> Option<std::path::PathBuf> {
    workspaces
        .list()
        .iter()
        .find_map(|ws| canonical_under_root(path, &ws.project_root))
}

/// Constant-time byte comparison for secrets (the app-level auth token here;
/// `site_serve` duplicates this for its pool-port cookie secret). No `subtle`
/// crate in the dependency tree — this is the standard XOR-accumulate idiom,
/// not worth pulling one in for a couple of call sites. Differing lengths
/// short-circuit (that timing leak reveals far less than per-byte content
/// would), but for equal lengths every byte position is compared regardless
/// of an earlier mismatch, so a match/no-match decision doesn't leak WHICH
/// byte differed via timing.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Strict allowlist for names that flow into a tmux/pty/shell invocation
/// (security review): tmux session names (`tmux.create_session`/`kill_session`,
/// `pty.open`'s `target`) and `workspace.create`'s `agent_name`, which
/// `pty::boot_wrapper_command` splices RAW into a shell command string with
/// no quoting. `1..=64` ASCII alphanumerics, `.`, `_`, `-` only — no shell
/// metacharacters, no `|` (which would also corrupt `tmux.rs`'s naive
/// `|`-delimited `list-sessions`/`list-panes` parsing), no whitespace/control
/// bytes. `pub(crate)` so `server.rs` can reuse it for `pty.open`.
pub(crate) fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
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

/// `video.open` — return a loopback HTTP URL for the cursored video file so
/// the frontend can hand it to the OS browser's HTML5 <video> (native
/// hardware decode + smooth playback, far better than streaming decoded frames
/// in-pane). The backend's `http_serve` server (spawned at startup) serves the
/// file with byte-range support; the launcher SSH-forwards the port. ADR 0018.
pub async fn handle_video_open(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
) -> Result<HandlerOutput> {
    let req: VideoOpenReq = serde_json::from_value(payload_json).context("video.open payload")?;
    tracing::info!(path = %req.path, "video.open");

    let path = std::path::Path::new(&req.path);
    if !crate::http_serve::is_servable_video(path) {
        let payload = json!({
            "error": format!("not a servable video file: {}", req.path),
            "code": "not_video",
        });
        return Ok(vec![(Frame::res(req_id, op::VIDEO_OPEN, payload), None)]);
    }
    match tokio::fs::metadata(path).await {
        Ok(m) if m.is_file() => {}
        _ => {
            let payload = json!({
                "error": format!("no such file: {}", req.path),
                "code": "io_error",
            });
            return Ok(vec![(Frame::res(req_id, op::VIDEO_OPEN, payload), None)]);
        }
    }

    // Register this ONE file under an opaque token rather than handing the
    // frontend a URL that embeds the raw filesystem path (security review:
    // the http_serve port has no auth of its own, so a URL shaped like
    // `http://127.0.0.1:1235/<abs-path>` let any local user GET any
    // owner-readable video — or, worse, anything else that path pointed at).
    // `None` means the CSPRNG read failed — fail closed rather than mint a
    // guessable token (security review).
    let Some(token) = crate::http_serve::register_video(path.to_path_buf()) else {
        let payload = json!({
            "error": "could not mint a secure grant token (system RNG unavailable) — try again",
            "code": "rng_unavailable",
        });
        return Ok(vec![(Frame::res(req_id, op::VIDEO_OPEN, payload), None)]);
    };
    let url = format!(
        "http://127.0.0.1:{}/{}",
        crate::http_serve::video_port(),
        token
    );
    let res = VideoOpenRes { url };
    let (_, rev) = session.snapshot().await;
    Ok(vec![(
        Frame::res(req_id, op::VIDEO_OPEN, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}

/// `docs.open` — open a static site/page under the workspace root in the OS
/// browser with full CSS/JS/sub-page fidelity. (Op name is legacy from the
/// Documenter first cut; it now serves any directory, not just
/// `docs/build`.) `req.path` is the cursored file's absolute backend path;
/// the handler roots the `site_serve` server at that file's **own
/// directory** (its site root) and returns the URL. The launcher
/// SSH-forwards the port. ADR 0024.
///
/// Confined to the workspace's project root (security review): `site_serve`'s
/// port has no auth of its own, so rooting it at an arbitrary absolute
/// directory would let `docs.open` turn it into a general-purpose file server
/// for anything the daemon's owner can read, reachable by any local user.
///
/// Rooting rule (so both relative AND root-relative `/asset` links resolve):
/// - cursor on a directory → serve it, open `/` (its `index.html`);
/// - cursor on `index.html`/`index.htm` → serve its parent, open `/`;
/// - cursor on any other file → serve its parent, open `/<filename>`.
pub async fn handle_docs_open(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    serial: Option<u64>,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: DocsOpenReq = serde_json::from_value(payload_json).context("docs.open payload")?;
    tracing::info!(path = %req.path, "docs.open");

    let err = |msg: String, code: &str| -> HandlerOutput {
        vec![(
            Frame::res(req_id, op::DOCS_OPEN, json!({ "error": msg, "code": code })),
            None,
        )]
    };

    // The requesting connection's serial keys its per-connection site root
    // internally (ADR 0029); `site_serve::set_root` mints the unguessable
    // nonce that actually becomes the URL's first path segment (security
    // review — see site_serve.rs). `None` only if hello hasn't registered
    // the connection yet — it always precedes docs.open in practice.
    let serial = match serial {
        Some(s) => s,
        None => {
            return Ok(err(
                "no connection context for docs.open (hello not received yet)".into(),
                "no_conn",
            ))
        }
    };

    if req.path.is_empty() {
        return Ok(err(
            "nothing selected to open — put the cursor on an .html file or a directory".into(),
            "no_selection",
        ));
    }

    let p = std::path::Path::new(&req.path);
    let meta = match tokio::fs::metadata(p).await {
        Ok(m) => m,
        Err(e) => return Ok(err(format!("no such path: {} ({e})", req.path), "io_error")),
    };

    // Confine the SELECTED PATH ITSELF to a KNOWN workspace (security
    // review), not just its derived parent/root: canonicalizing only the
    // parent let a symlink sitting AT `p` escape the workspace — the parent
    // dir canonicalizes fine even though the symlink's target doesn't — while
    // the raw `p` still got read/scanned as `entry` below (exfil via e.g. a
    // symlinked `page.html -> /home/victim/secret.html`, or a DoS via one
    // pointed at `/dev/zero` — `tokio::fs::read` never sees EOF on that).
    // Canonicalize `p` itself here, ONCE, under ANY currently-registered
    // workspace (not just the default — same check as `pluto.open`'s), and
    // derive (root, rel, entry) from THIS canonical path for everything
    // downstream. The raw `p`/`req.path` is never read or scanned again below.
    let canon_p = match canonicalize_within_any_workspace(p, workspaces) {
        Some(c) => c,
        None => {
            return Ok(err(
                format!("{} is outside every known workspace root", req.path),
                "outside_workspace",
            ));
        }
    };

    // Derive (site root, URL path, entry page) — all off the canonical path.
    // `canon_p.parent()` inherits confinement from the check above (a path
    // under a workspace root is still under it once its last component is
    // dropped), so no second canonicalize/check is needed for `root`.
    let (root, rel, entry): (std::path::PathBuf, String, std::path::PathBuf) = if meta.is_dir() {
        (canon_p.clone(), String::new(), canon_p.join("index.html"))
    } else {
        let parent = canon_p
            .parent()
            .unwrap_or_else(|| std::path::Path::new("/"))
            .to_path_buf();
        let fname = canon_p
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        if fname.eq_ignore_ascii_case("index.html") || fname.eq_ignore_ascii_case("index.htm") {
            (parent, String::new(), canon_p.clone())
        } else {
            (parent, fname, canon_p.clone())
        }
    };

    // A directory must have an index to open at `/`.
    if meta.is_dir() {
        let has_index = tokio::fs::metadata(&entry)
            .await
            .map(|m| m.is_file())
            .unwrap_or(false);
        if !has_index {
            return Ok(err(
                format!("no index.html in {}", root.display()),
                "no_index",
            ));
        }
    }

    // Loud root-relative guard (ADR 0029). The per-connection scheme serves under
    // `/<serial>/`, so page-relative links resolve but ROOT-relative ones
    // (`/assets/x.css`) escape the prefix and 404. Documenter output is clean; a
    // a project's `__site` / genhtml coverage tree is not. Scan the entry HTML and
    // refuse with a clear error rather than serve a silently-broken page (Option B's
    // per-port pool is the deferred fix for those).
    let entry_is_html = entry
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"))
        .unwrap_or(false);
    // Root-relative detection is now a ROUTER, not a refusal (ADR 0029 Option B,
    // implemented per maintainer decision, 2026-07-03 — "make proper fix now"): a site whose
    // entry HTML uses `/asset`-style links can't ride the shared `/{serial}/`
    // prefix server (the link escapes the prefix and 404s), so it gets a
    // DEDICATED pool port — its own origin, served from its true root, both
    // link styles resolve. A project's `__site` / genhtml trees open via W again.
    // `root` is already canonical (the confinement check above), so this
    // reuses it directly instead of re-canonicalizing with an unchecked
    // fallback to a possibly-uncanonical path (security review — that
    // fallback was the TOCTOU: if a second canonicalize somehow failed, it
    // silently registered the raw, unverified root instead of erroring).
    let mut pool_port: Option<(u16, String)> = None;
    if entry_is_html {
        if let Ok(bytes) = tokio::fs::read(&entry).await {
            // Cap the scan — an index is small, but a bundled SPA can be large.
            let head = &bytes[..bytes.len().min(512 * 1024)];
            if has_root_relative_refs(&String::from_utf8_lossy(head)) {
                match crate::site_serve::assign_pool_port(serial, root.clone()) {
                    Some(assigned) => pool_port = Some(assigned),
                    None => {
                        return Ok(err(
                            format!(
                                "{} uses root-relative links and every dedicated \
                                 port is busy ({} of {} in use by other \
                                 connections), or a secure token couldn't be minted \
                                 — close another root-relative site, reconnect, or retry",
                                entry.display(),
                                crate::site_serve::pool_in_use(),
                                crate::site_serve::POOL_SIZE,
                            ),
                            "root_relative_pool_busy",
                        ));
                    }
                }
            }
        }
    }

    let url = if let Some((port, secret)) = pool_port {
        // Dedicated origin: root-relative links resolve, but the port itself
        // has no auth (security review). The ONE-TIME secret in this URL
        // authenticates the FIRST request; the pool server then sets an
        // HttpOnly cookie so later same-page asset fetches — which can't
        // carry a query string — authenticate via the cookie instead. See
        // site_serve.rs's `ServeMode::Pool` auth check.
        format!(
            "http://127.0.0.1:{}/{}?secret={}",
            port,
            crate::site_serve::encode_url_path(&rel),
            secret,
        )
    } else {
        // Shared prefix server: point this connection's slot at the site root
        // and hand back a URL whose first path segment is the unguessable
        // nonce `set_root` minted (security review — not the raw serial).
        // `None` means the CSPRNG read failed — fail closed rather than mint
        // a guessable nonce.
        let Some(nonce) = crate::site_serve::set_root(serial, root) else {
            return Ok(err(
                "could not mint a secure site token (system RNG unavailable) — try again".into(),
                "rng_unavailable",
            ));
        };
        format!(
            "http://127.0.0.1:{}/{}/{}",
            crate::site_serve::site_port(),
            nonce,
            crate::site_serve::encode_url_path(&rel),
        )
    };

    let res = DocsOpenRes { url };
    let (_, rev) = session.snapshot().await;
    Ok(vec![(
        Frame::res(req_id, op::DOCS_OPEN, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}

/// Does this HTML contain ROOT-relative `href="/…"` / `src="/…"` references
/// (ADR 0029)? Protocol-relative `//host` URLs (external CDN) are NOT
/// root-relative and must not trip the guard. Both quote styles are checked.
fn has_root_relative_refs(html: &str) -> bool {
    for needle in ["href=\"/", "src=\"/", "href='/", "src='/"] {
        let mut from = 0;
        while let Some(pos) = html[from..].find(needle) {
            let after = from + pos + needle.len();
            // The char right after the leading `/`: another `/` means `//host`
            // (protocol-relative, external) — not a root-relative path.
            if html[after..].chars().next() != Some('/') {
                return true;
            }
            from = after;
        }
    }
    false
}

/// `quarto.open` — render a Quarto/markdown doc to a self-contained HTML on
/// the backend host (which has quarto + the RAM) and return the bytes, base64.
/// `execute = false` (`o`) = `--no-execute`: fast, quarto-only, no code run.
/// `execute = true` (`O`) runs code chunks — needs the language kernels on
/// this host. Renders into a unique temp subdir of the doc's own directory so
/// relative resources resolve (and `--embed-resources` can inline them), then
/// deletes it — the user's tree is left untouched, never clobbering a
/// hand-rendered `<doc>.html`.
pub async fn handle_quarto_open(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
) -> Result<HandlerOutput> {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;

    let req: QuartoOpenReq = serde_json::from_value(payload_json).context("quarto.open payload")?;
    tracing::info!(path = %req.path, execute = req.execute, "quarto.open");

    let err = |msg: String, code: &str| -> Result<HandlerOutput> {
        Ok(vec![(
            Frame::res(
                req_id,
                op::QUARTO_OPEN,
                json!({ "error": msg, "code": code }),
            ),
            None,
        )])
    };

    let src = std::path::Path::new(&req.path);
    let (Some(parent), Some(file_name)) = (src.parent(), src.file_name()) else {
        return err(format!("bad path: {}", req.path), "bad_path");
    };
    match tokio::fs::metadata(src).await {
        Ok(m) if m.is_file() => {}
        _ => return err(format!("no such file: {}", req.path), "io_error"),
    }

    // Render to a unique temp output *file* in the doc's own dir (= cwd), then
    // delete it. Use `--output <name>`, NOT `--output-dir`: `--output-dir` puts
    // quarto into project-render mode, which creates a `.quarto` cache dir and
    // then exits 1 on a "directory not empty" cleanup race even though the HTML
    // rendered fine — and litters `.quarto` in the user's tree. `--output
    // <name>` renders single-file in place (exit 0, no `.quarto`), resolves
    // relative resources, and the distinctive temp name avoids clobbering a
    // user's hand-rendered `<doc>.html`.
    let out_name = format!("__sot-qmd-{req_id}.html");
    let html_path = parent.join(&out_name);

    let mut cmd = tokio::process::Command::new("quarto");
    cmd.current_dir(parent)
        .arg("render")
        .arg(file_name)
        .arg("--to")
        .arg("html")
        .arg("--embed-resources")
        .arg("--output")
        .arg(&out_name);
    if !req.execute {
        cmd.arg("--no-execute");
    }

    let output = match cmd.output().await {
        Ok(o) => o,
        Err(e) => {
            return err(
                format!("failed to spawn quarto (is it installed on this host?): {e}"),
                "spawn_failed",
            );
        }
    };
    if !output.status.success() {
        let _ = tokio::fs::remove_file(&html_path).await;
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail = stderr
            .lines()
            .rev()
            .take(10)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        return err(
            format!(
                "quarto render failed (execute={}){}",
                req.execute,
                if tail.trim().is_empty() {
                    String::new()
                } else {
                    format!(":\n{tail}")
                }
            ),
            "quarto_render_failed",
        );
    }

    let bytes = match tokio::fs::read(&html_path).await {
        Ok(b) => b,
        Err(e) => {
            let _ = tokio::fs::remove_file(&html_path).await;
            return err(
                format!(
                    "quarto succeeded but output unreadable ({}): {e}",
                    html_path.display()
                ),
                "output_missing",
            );
        }
    };
    let _ = tokio::fs::remove_file(&html_path).await;

    let res = QuartoOpenRes {
        html_base64: STANDARD.encode(&bytes),
    };
    let (_, rev) = session.snapshot().await;
    Ok(vec![(
        Frame::res(req_id, op::QUARTO_OPEN, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}

/// `file.download` — stream a backend-host file to the frontend in <=1 MiB
/// `FileChunk` frames (bytes as each frame's trailing blob), all sharing
/// `req_id`; the `eof` frame carries the last chunk. Reads any path the backend
/// can read (matches `preview.get` reach — files outside the project root are
/// fine). Writes straight to the connection's outbound `tx`, so memory stays
/// bounded to one chunk regardless of file size. On open/read failure, sends a
/// single `{error, code}` frame instead.
pub async fn stream_file_download<W>(
    tx: &mut W,
    req_id: u64,
    payload_json: serde_json::Value,
) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncReadExt;
    const CHUNK: usize = 1024 * 1024;

    // Parse failure answers with an error frame (no chunks written yet, so
    // the response shape is unambiguous) — connection containment happens
    // before streaming starts. Mid-stream read errors below stay
    // connection-fatal on purpose: chunks are already on the wire and a
    // shape-switch mid-stream would leave the frontend's downloader hanging.
    let req: FileDownloadReq = match serde_json::from_value(payload_json) {
        Ok(r) => r,
        Err(e) => {
            let f = Frame::res(
                req_id,
                op::FILE_DOWNLOAD,
                json!({ "error": format!("file.download payload: {e}"), "code": "bad_request" }),
            );
            sot_protocol::write_frame(tx, &f, None).await?;
            return Ok(());
        }
    };
    tracing::info!(path = %req.path, "file.download");

    let path = std::path::Path::new(&req.path);
    let total = match tokio::fs::metadata(path).await {
        Ok(m) if m.is_file() => m.len(),
        _ => {
            let f = Frame::res(
                req_id,
                op::FILE_DOWNLOAD,
                json!({ "error": format!("no such file: {}", req.path), "code": "io_error" }),
            );
            sot_protocol::write_frame(tx, &f, None).await?;
            return Ok(());
        }
    };
    let mut file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) => {
            let f = Frame::res(
                req_id,
                op::FILE_DOWNLOAD,
                json!({ "error": format!("open failed: {e}"), "code": "io_error" }),
            );
            sot_protocol::write_frame(tx, &f, None).await?;
            return Ok(());
        }
    };

    let mut offset: u64 = 0;
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = file.read(&mut buf).await.context("file.download read")?;
        let eof = n == 0 || offset + n as u64 >= total;
        // The `blob` descriptor is REQUIRED: codec::read_frame only consumes
        // the appended bytes when `payload.blob.len` is present. Without it the
        // frontend skips this chunk's bytes and parses raw file data as the
        // next envelope → desync → reconnect loop. (Mirrors preview.get.)
        let chunk = FileChunk {
            offset,
            total,
            eof,
            blob: BlobDescriptor {
                len: n as u64,
                mime: "application/octet-stream".to_string(),
            },
        };
        let frame = Frame::res(req_id, op::FILE_DOWNLOAD, serde_json::to_value(&chunk)?);
        sot_protocol::write_frame(tx, &frame, Some(&buf[..n])).await?;
        offset += n as u64;
        if eof {
            break;
        }
    }
    Ok(())
}

/// `file.upload` — write one uploaded chunk into the cursored backend directory.
/// Stateless per chunk: on `offset == 0` it sanitizes `name` to a plain
/// basename (rejecting `/`, `\`, `.`, `..` so it can't escape `dir`),
/// de-duplicates against existing files with a ` (1)` suffix, and creates +
/// truncates the file; later chunks (which carry the resolved name back from
/// the ack) open it and write at `offset`. Acks each chunk, returning the
/// resolved `final_name` on the first and final chunks.
pub async fn handle_file_upload(
    req_id: u64,
    payload_json: serde_json::Value,
) -> Result<HandlerOutput> {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    use tokio::io::{AsyncSeekExt, AsyncWriteExt};

    let req: FileUploadReq = serde_json::from_value(payload_json).context("file.upload payload")?;

    let err = |msg: String, code: &str| -> Result<HandlerOutput> {
        Ok(vec![(
            Frame::res(
                req_id,
                op::FILE_UPLOAD,
                json!({ "error": msg, "code": code }),
            ),
            None,
        )])
    };

    let name = req.name.trim();
    if !is_safe_upload_name(name) {
        return err(format!("unsafe upload name: {:?}", req.name), "bad_name");
    }
    let dir = std::path::Path::new(&req.dir);
    match tokio::fs::metadata(dir).await {
        Ok(m) if m.is_dir() => {}
        _ => return err(format!("upload dir not found: {}", req.dir), "no_dir"),
    }
    let bytes = match STANDARD.decode(req.data_b64.as_bytes()) {
        Ok(b) => b,
        Err(e) => return err(format!("chunk base64 decode: {e}"), "bad_chunk"),
    };

    // First chunk resolves the (de-duplicated) destination name; later chunks
    // carry that resolved name back, so they just write at offset.
    let final_name = if req.offset == 0 {
        dedup_upload_name(dir, name)
    } else {
        name.to_string()
    };
    let path = dir.join(&final_name);

    let write_res = async {
        let mut f = if req.offset == 0 {
            tokio::fs::File::create(&path).await?
        } else {
            tokio::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .await?
        };
        f.seek(std::io::SeekFrom::Start(req.offset)).await?;
        f.write_all(&bytes).await?;
        f.flush().await?;
        Ok::<(), std::io::Error>(())
    }
    .await;
    if let Err(e) = write_res {
        return err(format!("write {} failed: {e}", path.display()), "io_error");
    }

    let ack = FileUploadAck {
        offset: req.offset,
        done: req.eof,
        final_name: (req.offset == 0 || req.eof).then(|| final_name.clone()),
    };
    Ok(vec![(
        Frame::res(req_id, op::FILE_UPLOAD, serde_json::to_value(ack)?),
        None,
    )])
}

/// A safe upload basename: non-empty, a single path component (no `/` or `\`),
/// and not `.`/`..` — so a chunk's `name` can never escape its target `dir`.
fn is_safe_upload_name(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty() && !n.contains('/') && !n.contains('\\') && n != "." && n != ".."
}

/// De-duplicate `name` within `dir`: returns `name` if free, else inserts
/// ` (1)`, ` (2)`, … before the extension until a free name is found.
fn dedup_upload_name(dir: &std::path::Path, name: &str) -> String {
    if !dir.join(name).exists() {
        return name.to_string();
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    for n in 1..100_000 {
        let cand = format!("{stem} ({n}){ext}");
        if !dir.join(&cand).exists() {
            return cand;
        }
    }
    format!("{stem}-{}{ext}", std::process::id())
}

// ─── Backend-sessions / tmux registry (ADR 0013) ────────────────────────
//
// These shell out to the host tmux server. They don't bump the session ring
// — tmux is its own source of truth, and the frontend polls (or watches via
// later tmux-event wiring) rather than relying on replay. Failures are
// returned as `{error, code}` responses, not propagated, so a missing tmux
// binary or a kill-of-nonexistent doesn't tear down the connection.

const TMUX_CAPTURE_LINES_CAP: u32 = 5000;

pub async fn handle_tmux_list_sessions(
    req_id: u64,
    _payload_json: serde_json::Value,
    session: &Session,
) -> Result<HandlerOutput> {
    tracing::debug!("tmux.list_sessions");
    let result = tokio::task::spawn_blocking(|| TmuxClient::new().list_sessions())
        .await
        .context("spawn_blocking list-sessions")?;
    let (_, rev) = session.snapshot().await;
    match result {
        Ok(sessions) => {
            let res = TmuxListSessionsRes {
                sessions: sessions.into_iter().map(into_proto_session).collect(),
            };
            Ok(vec![(
                Frame::res(req_id, op::TMUX_LIST_SESSIONS, serde_json::to_value(res)?)
                    .with_rev(rev),
                None,
            )])
        }
        Err(e) => Ok(vec![(
            tmux_error_frame(req_id, op::TMUX_LIST_SESSIONS, e),
            None,
        )]),
    }
}

pub async fn handle_tmux_list_panes(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
) -> Result<HandlerOutput> {
    let req: TmuxListPanesReq =
        serde_json::from_value(payload_json).context("tmux.list_panes payload")?;
    tracing::debug!(session = ?req.session, "tmux.list_panes");
    let session_arg = req.session.clone();
    let result =
        tokio::task::spawn_blocking(move || TmuxClient::new().list_panes(session_arg.as_deref()))
            .await
            .context("spawn_blocking list-panes")?;
    let (_, rev) = session.snapshot().await;
    match result {
        Ok(panes) => {
            let res = TmuxListPanesRes {
                panes: panes.into_iter().map(into_proto_pane).collect(),
            };
            Ok(vec![(
                Frame::res(req_id, op::TMUX_LIST_PANES, serde_json::to_value(res)?).with_rev(rev),
                None,
            )])
        }
        Err(e) => Ok(vec![(
            tmux_error_frame(req_id, op::TMUX_LIST_PANES, e),
            None,
        )]),
    }
}

pub async fn handle_tmux_create_session(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
) -> Result<HandlerOutput> {
    let req: TmuxCreateSessionReq =
        serde_json::from_value(payload_json).context("tmux.create_session payload")?;
    tracing::info!(name = %req.name, "tmux.create_session");
    // Name validation (security review): a `|`-containing name would corrupt
    // `tmux.rs`'s naive `|`-delimited `list-sessions`/`list-panes` parsing for
    // every session, not just this one; other odd bytes just confuse tmux.
    // Reject outright rather than silently mangling the requested name.
    if !valid_name(&req.name) {
        return Ok(vec![(
            tmux_error_frame(
                req_id,
                op::TMUX_CREATE_SESSION,
                anyhow::anyhow!(
                    "invalid session name {:?} (want 1-64 chars of [A-Za-z0-9._-])",
                    req.name
                ),
            ),
            None,
        )]);
    }
    let name = req.name.clone();
    let command = req.command.clone();
    let cwd = req.cwd.clone();
    let result = tokio::task::spawn_blocking(move || {
        let cwd_path = cwd.as_ref().map(std::path::PathBuf::from);
        // Generic (non-workspace) session — no slug; still stamped with
        // SOT_SESSION/SOT_WORKSPACE_ROOT/SOT_MANUAL awareness.
        TmuxClient::new().create_session(&name, command.as_deref(), cwd_path.as_deref(), None)
    })
    .await
    .context("spawn_blocking create-session")?;
    let rev = session
        .bump("tmux.session_created", json!({ "name": req.name }))
        .await;
    match result {
        Ok(()) => {
            let payload = json!({ "name": req.name });
            Ok(vec![(
                Frame::res(req_id, op::TMUX_CREATE_SESSION, payload).with_rev(rev),
                None,
            )])
        }
        Err(e) => Ok(vec![(
            tmux_error_frame(req_id, op::TMUX_CREATE_SESSION, e),
            None,
        )]),
    }
}

pub async fn handle_tmux_kill_session(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
) -> Result<HandlerOutput> {
    let req: TmuxKillSessionReq =
        serde_json::from_value(payload_json).context("tmux.kill_session payload")?;
    tracing::info!(name = %req.name, "tmux.kill_session");
    // Name validation (security review) — same allowlist as tmux.create_session.
    if !valid_name(&req.name) {
        return Ok(vec![(
            tmux_error_frame(
                req_id,
                op::TMUX_KILL_SESSION,
                anyhow::anyhow!(
                    "invalid session name {:?} (want 1-64 chars of [A-Za-z0-9._-])",
                    req.name
                ),
            ),
            None,
        )]);
    }
    let name = req.name.clone();
    let result = tokio::task::spawn_blocking(move || TmuxClient::new().kill_session(&name))
        .await
        .context("spawn_blocking kill-session")?;
    let rev = session
        .bump("tmux.session_killed", json!({ "name": req.name }))
        .await;
    match result {
        Ok(()) => {
            let payload = json!({ "name": req.name });
            Ok(vec![(
                Frame::res(req_id, op::TMUX_KILL_SESSION, payload).with_rev(rev),
                None,
            )])
        }
        Err(e) => Ok(vec![(
            tmux_error_frame(req_id, op::TMUX_KILL_SESSION, e),
            None,
        )]),
    }
}

pub async fn handle_tmux_capture_pane(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
) -> Result<HandlerOutput> {
    let req: TmuxCapturePaneReq =
        serde_json::from_value(payload_json).context("tmux.capture_pane payload")?;
    let lines = req.lines.min(TMUX_CAPTURE_LINES_CAP);
    tracing::debug!(target = %req.target, lines, "tmux.capture_pane");
    let target = req.target.clone();
    let result =
        tokio::task::spawn_blocking(move || TmuxClient::new().capture_pane(&target, lines))
            .await
            .context("spawn_blocking capture-pane")?;
    let (_, rev) = session.snapshot().await;
    match result {
        Ok(text) => {
            let res = TmuxCapturePaneRes { text };
            Ok(vec![(
                Frame::res(req_id, op::TMUX_CAPTURE_PANE, serde_json::to_value(res)?).with_rev(rev),
                None,
            )])
        }
        Err(e) => Ok(vec![(
            tmux_error_frame(req_id, op::TMUX_CAPTURE_PANE, e),
            None,
        )]),
    }
}

pub async fn handle_workspace_create(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
    ws_events: &broadcast::Sender<WorkspaceChanged>,
) -> Result<HandlerOutput> {
    use sot_protocol::{WorkspaceCreateReq, WorkspaceCreateRes};
    // ADR 0023 §3 daemon-boot trigger — read off the raw payload (it is not a
    // `WorkspaceCreateReq` struct field: adding one would force the frozen FE's
    // struct literal to set it). serde ignores it on the typed deserialize below.
    let boot = payload_json
        .get("boot")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let req: WorkspaceCreateReq =
        serde_json::from_value(payload_json).context("workspace.create payload")?;
    tracing::info!(label = %req.label, project_root = %req.project_root, boot, "workspace.create");

    let project_root = std::path::PathBuf::from(&req.project_root);
    if !project_root.exists() {
        let payload = json!({
            "error": format!("project_root does not exist: {}", req.project_root),
            "code": "no_such_path",
        });
        return Ok(vec![(
            Frame::res(req_id, op::WORKSPACE_CREATE, payload),
            None,
        )]);
    }
    if !project_root.is_dir() {
        let payload = json!({
            "error": format!("project_root is not a directory: {}", req.project_root),
            "code": "not_a_directory",
        });
        return Ok(vec![(
            Frame::res(req_id, op::WORKSPACE_CREATE, payload),
            None,
        )]);
    }

    // Name validation (security review): `agent_name` is persisted and later
    // spliced RAW (no quoting) into a shell command string by
    // `pty::boot_wrapper_command` (`export SOT_COMM_NAME={agent_name}; …`).
    // Empty is a legitimate "no agent name" sentinel (boot_wrapper_command
    // skips the export then); anything non-empty must match the strict
    // allowlist or this is rejected outright rather than silently sanitized.
    if !req.agent_name.is_empty() && !valid_name(&req.agent_name) {
        let payload = json!({
            "error": format!(
                "invalid agent_name {:?} (want 1-64 chars of [A-Za-z0-9._-])",
                req.agent_name
            ),
            "code": "bad_agent_name",
        });
        return Ok(vec![(
            Frame::res(req_id, op::WORKSPACE_CREATE, payload),
            None,
        )]);
    }

    // Register the workspace in memory + on disk first; the tmux session
    // is a UX nicety that the user can always re-create later, so we
    // don't fail the op if tmux misbehaves.
    // ADR 0031: resolve the agent kind. Explicit `agent` wins; absent derives
    // from the legacy `autostart_claude` flag.
    let agent_kind: String = if !req.agent.is_empty() {
        match req.agent.as_str() {
            "claude" | "codex" | "none" => req.agent.clone(),
            other => {
                let payload = json!({
                    "error": format!("unknown agent kind '{other}' (want claude | codex | none)"),
                    "code": "bad_agent",
                });
                return Ok(vec![(
                    Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                    None,
                )]);
            }
        }
    } else if req.autostart_claude {
        "claude".to_string()
    } else {
        "none".to_string()
    };
    let autostart = agent_kind != "none";
    let mut ws_seed = crate::workspaces::Workspace::from_label(
        &req.label,
        project_root.clone(),
        autostart,
        agent_kind.clone(),
        req.agent_name.clone(),
        req.task.clone(),
    );
    // Keep the home/default workspace's display label sticky as ".SoT" — mirror
    // the daemon-seed override in server.rs. A workspace.create for slug "sot"
    // (e.g. comm-spawn pointed at the sot repo root) would otherwise clobber the
    // cosmetic ".SoT" home label back to "sot" until the next restart. Slug (and
    // hence tmux/handle) is untouched — label only.
    if ws_seed.slug == "sot" {
        ws_seed.label = ".SoT".to_string();
    }
    let ws_handle = workspaces.insert(ws_seed);
    if let Err(e) = crate::workspaces::save(&ws_handle) {
        tracing::warn!(error = %e, "workspace toml persist failed; workspace is in-memory only");
    }

    // Create the per-workspace tmux session so BL-pane attach works.
    // UNIFIED SPAWN (ADR 0023): EVERY `autostart_claude` workspace — a background
    // comm-spawn (`boot:true`) AND an FE nav-pane create — gets the wait-for-attach
    // wrapper (`boot_wrapper_command`) as its pane START COMMAND. The wrapper
    // `exec`s `ccb` the moment a client attaches (the boot-pty for a background
    // spawn, or the FE's own attach on switch), so claude is the pane's process —
    // never typed into a shell, which raced the prompt. This retires the FE
    // autostart-on-attach typing: one race-free boot path for both cases.
    let tmux_session = ws_handle.tmux_session.clone();
    let cwd = project_root.clone();
    let ws_slug = ws_handle.slug.clone();
    let boot_cmd: Option<String> = if autostart || boot {
        Some(crate::pty::boot_wrapper_command(
            &tmux_session,
            &req.agent_name,
            &agent_kind,
        ))
    } else {
        None
    };
    let tmux_result = tokio::task::spawn_blocking(move || {
        crate::tmux::TmuxClient::new().create_session(
            &tmux_session,
            boot_cmd.as_deref(),
            Some(&cwd),
            Some(&ws_slug),
        )
    })
    .await
    .context("spawn_blocking workspace tmux create")?;
    let tmux_ok = tmux_result.is_ok();
    if let Err(e) = tmux_result {
        tracing::warn!(error = %e, "workspace tmux session create failed; workspace registered without one");
    }

    // ADR 0023 §3 (UNIFIED): daemon-side claude boot via a throwaway boot-pty —
    // open a real pty client to the new session so the wrapper's wait-for-attach
    // is satisfied, poll until claude is foreground, then detach (claude survives;
    // the FE client takes over). Runs for EVERY `autostart_claude` create, not
    // just comm-spawn `boot=true`. WHY nav-pane needs it too: the ADR-0014 single
    // foreground pty re-target is NOT a stable init client, so without the boot-pty
    // a nav-pane claude dies during init and the daemon falls back to home (the
    // "sitting in home" bug). The boot-pty is the SAME stable client that makes
    // comm-spawn boot reliably — confirmed the missing-client delta is the cause.
    // Detached `tokio::spawn` (polls up to ~45s, must not block the response);
    // skipped when the tmux session failed to create.
    if (autostart || boot) && tmux_ok {
        let boot_session = ws_handle.tmux_session.clone();
        let boot_agent = req.agent_name.clone();
        let boot_cwd = project_root.clone();
        let boot_slug = ws_handle.slug.clone();
        tracing::info!(session = %boot_session, agent = %boot_agent, boot,
            "workspace.create autostart — spawning daemon boot-pty for claude (stable init client)");
        tokio::spawn(async move {
            crate::pty::boot_workspace_claude(boot_session, boot_agent, boot_cwd, boot_slug).await;
        });
    }

    let res = WorkspaceCreateRes {
        workspace_id: ws_handle.workspace_id.clone(),
        slug: ws_handle.slug.clone(),
        label: ws_handle.label.clone(),
        project_root: ws_handle.project_root.to_string_lossy().into_owned(),
        tmux_session: ws_handle.tmux_session.clone(),
    };
    let rev = session
        .bump(
            "workspace.created",
            json!({ "workspace_id": ws_handle.workspace_id, "slug": ws_handle.slug }),
        )
        .await;
    // Live-push to every connected frontend so the Sessions strip refreshes
    // without a manual workspace.list poll. Send error means no subscribers;
    // harmless.
    let _ = ws_events.send(WorkspaceChanged {
        action: "created".into(),
        slug: ws_handle.slug.clone(),
        workspace_id: ws_handle.workspace_id.clone(),
    });
    Ok(vec![(
        Frame::res(req_id, op::WORKSPACE_CREATE, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}

pub async fn handle_workspace_destroy(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
    ws_events: &broadcast::Sender<WorkspaceChanged>,
) -> Result<HandlerOutput> {
    use sot_protocol::{WorkspaceDestroyReq, WorkspaceDestroyRes};
    let req: WorkspaceDestroyReq =
        serde_json::from_value(payload_json).context("workspace.destroy payload")?;
    tracing::info!(workspace_id = %req.workspace_id, "workspace.destroy");

    let Some(ws) = workspaces.resolve(Some(&req.workspace_id)) else {
        let payload = json!({
            "error": format!("unknown workspace: {}", req.workspace_id),
            "code": "unknown_workspace",
        });
        return Ok(vec![(
            Frame::res(req_id, op::WORKSPACE_DESTROY, payload),
            None,
        )]);
    };

    // Refuse to destroy the default workspace — it's the daemon's
    // anchor, and there's no fallback target to swap ops to. The user
    // can change which workspace is the default via daemon restart
    // with a different `--project-root`; destroying the running one
    // would leave the registry incoherent.
    if workspaces.default_id().as_deref() == Some(ws.workspace_id.as_str()) {
        let payload = json!({
            "error": format!(
                "cannot destroy default workspace '{}'",
                ws.label
            ),
            "code": "default_workspace_not_destroyable",
        });
        return Ok(vec![(
            Frame::res(req_id, op::WORKSPACE_DESTROY, payload),
            None,
        )]);
    }

    let slug = ws.slug.clone();
    let label = ws.label.clone();
    let workspace_id = ws.workspace_id.clone();
    let tmux_session = ws.tmux_session.clone();
    let agent_name = ws.agent_name.clone();

    // Kill the tmux session. Failure is non-fatal — usually means the
    // session wasn't running anyway. We surface the bool so the
    // frontend can decide whether to surface the discrepancy.
    let tmux_target = tmux_session.clone();
    let tmux_killed = tokio::task::spawn_blocking(move || {
        crate::tmux::TmuxClient::new()
            .kill_session(&tmux_target)
            .is_ok()
    })
    .await
    .unwrap_or(false);

    // Prune the sot-comm registry. Killing the tmux session takes the agent
    // down before it can run its own comm-leave, so the killer must deregister
    // it — otherwise its row lingers as a ghost in `workspace.list`, which
    // merges the registry (see `resolve_handle`). Mirror that resolver's
    // matching so we drop exactly the rows this workspace owned: by stored
    // `agent_name`, and by tmux session-part (covers manually-joined agents
    // whose `ws.agent_name` was never set, plus any stale duplicate rows on the
    // same session). Best-effort + blocking (fs + file lock) → spawn_blocking,
    // non-fatal like the tmux kill above.
    let reg_session = tmux_session.clone();
    let reg_agent = agent_name.clone();
    let comm_removed = tokio::task::spawn_blocking(move || {
        remove_comm_agents_for_workspace(&reg_session, &reg_agent)
    })
    .await
    .unwrap_or_default();
    if !comm_removed.is_empty() {
        tracing::info!(
            removed = ?comm_removed,
            slug = %slug,
            "pruned sot-comm registry rows for destroyed workspace"
        );
    }

    // Remove the tomls from disk so neither registration path brings the
    // workspace back on next daemon startup: `scan_disk` reads the modern
    // workspaces/ toml, and the ADR-0013 migration reads the legacy
    // sessions/ toml. Best-effort: a missing file is success; a remove
    // error is logged + reported but doesn't block the in-memory removal.
    let mut toml_removed = true;
    for toml_path in [
        crate::workspaces::toml_path_for(&slug),
        crate::workspaces::legacy_toml_path_for(&slug),
    ] {
        match std::fs::remove_file(&toml_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(error = %e, path = ?toml_path, "workspace toml remove failed");
                toml_removed = false;
            }
        }
    }

    // Drop from in-memory registry last. The Arc<Workspace> dropped
    // here is also the one holding the kernel/repl handles; when the
    // last Arc dies their Drop impls run and the Julia children are
    // killed. Other Arc holders (e.g. mid-flight handlers) will keep
    // those processes alive until they finish.
    let _ = workspaces.remove_by_id(&workspace_id);

    // Live-push to every connected frontend so the Sessions strip refreshes
    // without a manual workspace.list poll (mirror the create path). Clone
    // because slug/workspace_id are consumed by the bump + response below.
    let _ = ws_events.send(WorkspaceChanged {
        action: "destroyed".into(),
        slug: slug.clone(),
        workspace_id: workspace_id.clone(),
    });

    let rev = session
        .bump(
            "workspace.destroyed",
            json!({ "workspace_id": workspace_id, "slug": slug }),
        )
        .await;

    let res = WorkspaceDestroyRes {
        workspace_id,
        slug,
        label,
        tmux_killed,
        toml_removed,
    };
    Ok(vec![(
        Frame::res(req_id, op::WORKSPACE_DESTROY, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}

/// Relay one agent-to-agent message (`agent.send`). Parse the request,
/// stamp an ISO-8601 UTC `ts`, publish onto the agent broadcast channel
/// (each connection turns it into an `agent.message` evt), and ack. The
/// publish is fire-and-forget: a send with no subscribers still acks ok.
/// Mirrors the `ws_events.send(...)` leg of `handle_workspace_create`.
pub async fn handle_agent_send(
    req_id: u64,
    payload_json: serde_json::Value,
    agent_tx: &broadcast::Sender<AgentMessage>,
) -> Result<HandlerOutput> {
    let req: AgentSendReq = serde_json::from_value(payload_json).context("agent.send payload")?;
    tracing::info!(from = %req.from, to = %req.to, "agent.send relay");
    let msg = AgentMessage {
        from: req.from,
        to: req.to,
        text: req.text,
        ts: iso8601_utc_now(),
    };
    // Fire-and-forget broadcast; send error means no subscribers, harmless.
    let _ = agent_tx.send(msg);
    Ok(vec![(
        Frame::res(
            req_id,
            op::AGENT_SEND,
            serde_json::to_value(AgentSendRes { ok: true })?,
        ),
        None,
    )])
}

/// `fe.command.send` (ADR 0025): parse the imperative UI command, build an
/// `FeCommandEvt { v:1, cmd, args, target }`, publish it onto the FE-command
/// broadcast channel (each connection turns it into an `fe.command` evt), and
/// ack. The publish is fire-and-forget: a send with no FE connected still acks
/// ok. Structurally mirrors `handle_agent_send` — the only daemon-side step is
/// re-emit; `target` routing is FE-side (the FE self-filters), so the daemon
/// broadcasts to every connection unconditionally (v1.1 will route here).
pub async fn handle_fe_command_send(
    req_id: u64,
    payload_json: serde_json::Value,
    fe_tx: &broadcast::Sender<FeCommandEvt>,
) -> Result<HandlerOutput> {
    let req: FeCommandSendReq =
        serde_json::from_value(payload_json).context("fe.command.send payload")?;
    tracing::info!(cmd = %req.cmd, target = ?req.target, "fe.command.send relay");
    let evt = FeCommandEvt {
        v: 1,
        cmd: req.cmd,
        args: req.args,
        target: req.target,
    };
    // Fire-and-forget broadcast; send error means no subscribers, harmless.
    let _ = fe_tx.send(evt);
    Ok(vec![(
        Frame::res(
            req_id,
            op::FE_COMMAND_SEND,
            serde_json::to_value(FeCommandSendRes { ok: true })?,
        ),
        None,
    )])
}

/// ISO-8601 UTC instant (e.g. `2026-05-29T14:30:05Z`) without pulling in
/// chrono — the backend has no time crate, so format the civil date from
/// the Unix timestamp directly. Used to stamp relayed agent messages.
fn iso8601_utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Days since the Unix epoch and seconds-of-day.
    let days = (secs / 86_400) as i64;
    let sod = secs % 86_400;
    let (hh, mm, ss) = (sod / 3_600, (sod % 3_600) / 60, sod % 60);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hh, mm, ss
    )
}

/// Howard Hinnant's days-from-civil inverse: convert days-since-epoch to a
/// (year, month, day) Gregorian date. Public-domain algorithm; avoids a
/// date crate dependency for the single timestamp we need.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as i64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

/// Resolve the sot-comm registry path: `$SOT_COMM_HOME/registry.json`
/// when the env var is set, else `$HOME/.sot-comm/registry.json`. Returns
/// `None` only when neither env var is available (no HOME) — every other
/// failure is the caller's to treat as "absent" (empty strings).
pub(crate) fn comm_registry_path() -> Option<std::path::PathBuf> {
    if let Some(v) = std::env::var_os("SOT_COMM_HOME") {
        let mut p = std::path::PathBuf::from(v);
        p.push("registry.json");
        return Some(p);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = std::path::PathBuf::from(home);
        p.push(".sot-comm");
        p.push("registry.json");
        return Some(p);
    }
    None
}

/// Read + parse the sot-comm registry, returning the `.agents` object as a
/// JSON value. Fully defensive: a missing file, unreadable path, or malformed
/// JSON all yield `None` so `workspace.list` never errors on the registry. The
/// FE can't read the registry (separate HOME), so we surface it here.
fn read_comm_agents() -> Option<serde_json::Value> {
    let path = comm_registry_path()?;
    let bytes = std::fs::read(&path).ok()?;
    let root: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    root.get("agents").cloned()
}

/// Remove the sot-comm registry rows owned by a workspace that is being
/// destroyed, returning the handles removed (for logging). A killed agent can't
/// run `comm-leave` for itself, so its row would otherwise persist and show as
/// a ghost in `workspace.list`. We mirror `handle_workspace_list`'s
/// `resolve_handle` matching — a row belongs to this workspace when its `tmux`
/// session-part equals `tmux_session`, or (fallback for not-yet-joined `spawning`
/// rows) when its handle equals the stored `agent_name`. ALL matching rows are
/// dropped, including stale duplicates on the same session.
///
/// Fully best-effort: a missing registry, malformed JSON, or any I/O failure
/// yields an empty result and never propagates — the destroy must not fail
/// because the registry couldn't be pruned. Honors comm-lib.sh's mkdir-spinlock
/// (`<comm_home>/.registry.lock`, 200×50ms then force-break) and writes via a
/// temp file + atomic rename so a concurrent bash mutator (comm-join /
/// comm-status / …) can't see a torn file.
fn remove_comm_agents_for_workspace(tmux_session: &str, agent_name: &str) -> Vec<String> {
    let Some(reg_path) = comm_registry_path() else {
        return Vec::new();
    };
    let Some(dir) = reg_path.parent().map(|p| p.to_path_buf()) else {
        return Vec::new();
    };
    let lock_dir = dir.join(".registry.lock");
    let tmp_path = dir.join("registry.json.tmp");

    // mkdir-spinlock, matching comm-lib.sh `with_lock`.
    let mut acquired = false;
    for _ in 0..200 {
        match std::fs::create_dir(&lock_dir) {
            Ok(()) => {
                acquired = true;
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => {
                tracing::warn!(error = %e, lock = ?lock_dir, "comm registry lock error");
                return Vec::new();
            }
        }
    }
    if !acquired {
        tracing::warn!(lock = ?lock_dir, "forcing presumed-stale comm registry lock");
        let _ = std::fs::remove_dir(&lock_dir);
        if std::fs::create_dir(&lock_dir).is_err() {
            return Vec::new();
        }
    }

    // Critical section — always release the lock on the way out.
    let removed = (|| -> Vec<String> {
        let bytes = match std::fs::read(&reg_path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => {
                tracing::warn!(error = %e, "comm registry read failed");
                return Vec::new();
            }
        };
        let mut root: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "comm registry parse failed");
                return Vec::new();
            }
        };
        let Some(agents) = root.get_mut("agents").and_then(|a| a.as_object_mut()) else {
            return Vec::new();
        };
        let to_remove: Vec<String> = agents
            .iter()
            .filter_map(|(handle, entry)| {
                let by_name = !agent_name.is_empty() && handle == agent_name;
                let by_tmux = !tmux_session.is_empty()
                    && entry
                        .get("tmux")
                        .and_then(|v| v.as_str())
                        .map(|t| t.split(':').next().unwrap_or("") == tmux_session)
                        .unwrap_or(false);
                (by_name || by_tmux).then(|| handle.clone())
            })
            .collect();
        if to_remove.is_empty() {
            return Vec::new();
        }
        for handle in &to_remove {
            agents.remove(handle);
        }
        let mut serialized = match serde_json::to_vec_pretty(&root) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "comm registry serialize failed");
                return Vec::new();
            }
        };
        serialized.push(b'\n');
        if let Err(e) = std::fs::write(&tmp_path, &serialized) {
            tracing::warn!(error = %e, "comm registry tmp write failed");
            return Vec::new();
        }
        if let Err(e) = std::fs::rename(&tmp_path, &reg_path) {
            tracing::warn!(error = %e, "comm registry rename failed");
            let _ = std::fs::remove_file(&tmp_path);
            return Vec::new();
        }
        to_remove
    })();

    let _ = std::fs::remove_dir(&lock_dir);
    removed
}

pub async fn handle_workspace_list(
    req_id: u64,
    _payload_json: serde_json::Value,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    use sot_protocol::{WorkspaceListEntry, WorkspaceListRes};
    let default_id = workspaces.default_id();
    // Read the sot-comm registry once per list call (fresh — picks up the
    // owning agents' latest `comm-status.sh` writes). `None` when the file is
    // absent/malformed; every lookup below then falls back to empty strings.
    let comm_agents = read_comm_agents();
    // Pull `.agents[agent_name].<field>` as an owned String, "" if anything is
    // missing or not a string.
    let agent_str = |agent_name: &str, field: &str| -> String {
        if agent_name.is_empty() {
            return String::new();
        }
        comm_agents
            .as_ref()
            .and_then(|a| a.get(agent_name))
            .and_then(|entry| entry.get(field))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    // Resolve the comm handle ACTUALLY running in a workspace's tmux session, so
    // manually-joined / pre-state-nav agents (whose `ws.agent_name` was never set
    // — only the spawn path writes it) still bind. The registry `tmux` field is
    // "<session>:<win>.<pane>"; match its session part against the workspace's
    // `tmux_session`. Falls back to the stored `agent_name` when there's no live
    // tmux match (e.g. a `spawning` row whose `tmux` is still "").
    let resolve_handle = |tmux_session: &str, stored: &str| -> String {
        if !tmux_session.is_empty() {
            if let Some(agents) = comm_agents.as_ref().and_then(|a| a.as_object()) {
                // Several rows can share a session (different panes, or a stale
                // duplicate handle); prefer the most-recently-seen so we bind the
                // live occupant, not a dead row. ISO `last_seen` compares lexically.
                let mut best: Option<(&str, &str)> = None;
                for (handle, entry) in agents {
                    let asess = entry
                        .get("tmux")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .split(':')
                        .next()
                        .unwrap_or("");
                    if !asess.is_empty() && asess == tmux_session {
                        let seen = entry
                            .get("last_seen")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if best.map_or(true, |(_, bseen)| seen > bseen) {
                            best = Some((handle.as_str(), seen));
                        }
                    }
                }
                if let Some((h, _)) = best {
                    return h.to_string();
                }
            }
        }
        stored.to_string()
    };
    let mut entries: Vec<WorkspaceListEntry> = workspaces
        .list()
        .into_iter()
        .map(|ws| {
            // Prefer the live tmux occupant; fall back to the stored agent_name.
            let handle = resolve_handle(&ws.tmux_session, &ws.agent_name);
            // Work-state merge: the registry `state` is what the agent *declared*
            // (set by the work-state hooks: UserPromptSubmit → "working",
            // Notification → "blocked", Stop → "idle"), while `pane` is the live
            // pane-scrape. The hooks are the source of truth — instant, automatic,
            // no model cooperation. Precedence:
            //   reg "blocked" BUT pane "working" → "working". A Notification hook
            //                       stamps "blocked", but resuming generation fires
            //                       no UserPromptSubmit to clear it, so the block
            //                       goes stale and the agent shows red while it is
            //                       actually working. The live footer showing active
            //                       generation is proof it is NOT waiting on you. (A
            //                       blocked/waiting agent is not generating, so its
            //                       pane never reads "working" — this only fires on a
            //                       genuinely stale block.)
            //   "working"/"blocked"/"waiting"/"done" → registry wins (an explicit hook state;
            //                       for "blocked" with a non-working pane the pane
            //                       cannot tell waiting-on-you from idle).
            //   registry idle/empty → fall back to the live pane (covers agents not
            //                       yet running the hooks, through the rollout; a
            //                       hooked agent's pane agrees anyway).
            let reg = agent_str(&handle, "state");
            let pane = workspaces.pane_activity(&ws.tmux_session);
            let agent_state = if (reg == "blocked" || reg == "waiting") && pane == "working" {
                "working".to_string()
            } else if reg == "working" || reg == "blocked" || reg == "done" || reg == "waiting" {
                reg
            } else if !pane.is_empty() {
                pane
            } else {
                reg
            };
            WorkspaceListEntry {
                workspace_id: ws.workspace_id.clone(),
                slug: ws.slug.clone(),
                label: ws.label.clone(),
                project_root: ws.project_root.to_string_lossy().into_owned(),
                tmux_session: ws.tmux_session.clone(),
                kernel_running: ws.kernel_built(),
                is_default: default_id.as_deref() == Some(ws.workspace_id.as_str()),
                autostart_claude: ws.autostart_claude,
                agent: ws.agent.clone(),
                agent_name: if handle.is_empty() {
                    ws.agent_name.clone()
                } else {
                    handle.clone()
                },
                task: ws.task.clone(),
                agent_state,
                agent_summary: agent_str(&handle, "summary"),
                agent_status_at: agent_str(&handle, "status_at"),
            }
        })
        .collect();
    // Pin the default workspace (the daemon's home/anchor — ".SoT") FIRST so it
    // sits leftmost in the FE session strip, which renders in received order.
    // Stable sort: every other workspace keeps its alphabetical-by-slug order.
    entries.sort_by(|a, b| b.is_default.cmp(&a.is_default));
    tracing::debug!(count = entries.len(), "workspace.list");
    let res = WorkspaceListRes {
        workspaces: entries,
    };
    Ok(vec![(
        Frame::res(req_id, op::WORKSPACE_LIST, serde_json::to_value(res)?),
        None,
    )])
}

pub async fn handle_directory_list(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
) -> Result<HandlerOutput> {
    use sot_protocol::{DirectoryEntry, DirectoryListReq, DirectoryListRes};
    let req: DirectoryListReq =
        serde_json::from_value(payload_json).context("directory.list payload")?;
    tracing::debug!(path = %req.path, include_hidden = req.include_hidden, "directory.list");

    let path = std::path::PathBuf::from(&req.path);
    let include_hidden = req.include_hidden;
    let result = tokio::task::spawn_blocking(move || -> Result<Vec<DirectoryEntry>> {
        let read = std::fs::read_dir(&path).with_context(|| format!("read_dir {path:?}"))?;
        let mut entries: Vec<DirectoryEntry> = Vec::new();
        for ent in read.flatten() {
            let name = match ent.file_name().into_string() {
                Ok(s) => s,
                Err(_) => continue, // non-UTF8 filename — skip
            };
            if !include_hidden && name.starts_with('.') {
                continue;
            }
            let p = ent.path();
            // Follow symlinks via metadata (not symlink_metadata) so a
            // symlink to a directory still surfaces as one entry.
            let is_dir = match std::fs::metadata(&p) {
                Ok(m) => m.is_dir(),
                Err(_) => continue,
            };
            if !is_dir {
                continue;
            }
            // Cheap has_children probe: try to open the dir and see if
            // any subdirectory exists. Don't recurse — just one read_dir
            // pass per entry.
            let has_children = std::fs::read_dir(&p)
                .ok()
                .map(|it| {
                    it.flatten().any(|c| {
                        let cn = c.file_name();
                        if !include_hidden {
                            if let Some(s) = cn.to_str() {
                                if s.starts_with('.') {
                                    return false;
                                }
                            }
                        }
                        std::fs::metadata(c.path())
                            .map(|m| m.is_dir())
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false);
            entries.push(DirectoryEntry {
                name,
                path: p.to_string_lossy().into_owned(),
                has_children,
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    })
    .await
    .context("spawn_blocking directory.list")?;

    let (_, rev) = session.snapshot().await;
    match result {
        Ok(entries) => {
            let res = DirectoryListRes {
                path: req.path,
                entries,
            };
            Ok(vec![(
                Frame::res(req_id, op::DIRECTORY_LIST, serde_json::to_value(res)?).with_rev(rev),
                None,
            )])
        }
        Err(e) => {
            let payload = json!({
                "error": format!("{e:#}"),
                "code": "directory_list_failed",
                "path": req.path,
            });
            Ok(vec![(
                Frame::res(req_id, op::DIRECTORY_LIST, payload),
                None,
            )])
        }
    }
}

fn tmux_error_frame(req_id: u64, op_str: &str, err: anyhow::Error) -> Frame {
    let msg = format!("{err:#}");
    tracing::warn!(op = op_str, error = %msg, "tmux op failed");
    let payload = json!({
        "error": msg,
        "code": "tmux_failed",
    });
    Frame::res(req_id, op_str, payload)
}

fn into_proto_session(s: crate::tmux::SessionInfo) -> TmuxSession {
    TmuxSession {
        name: s.name,
        created: s.created,
        attached: s.attached,
        windows: s.windows,
        width: s.width,
        height: s.height,
    }
}

fn into_proto_pane(p: crate::tmux::PaneInfo) -> TmuxPane {
    TmuxPane {
        id: p.id,
        session: p.session,
        window_index: p.window_index,
        pane_index: p.pane_index,
        title: p.title,
        command: p.command,
        pid: p.pid,
        width: p.width,
        height: p.height,
        active: p.active,
    }
}

#[cfg(test)]
mod valid_name_tests {
    use super::valid_name;

    #[test]
    fn accepts_typical_names() {
        assert!(valid_name("sot-be-myhost"));
        assert!(valid_name("myhost-dev"));
        assert!(valid_name("MyPackage.jl"));
        assert!(valid_name("a"));
        assert!(valid_name(&"a".repeat(64)));
    }

    #[test]
    fn rejects_empty_and_oversize() {
        assert!(!valid_name(""));
        assert!(!valid_name(&"a".repeat(65)));
    }

    #[test]
    fn rejects_shell_and_parser_metacharacters() {
        // The pipe is the specific `tmux.rs` list-parsing corruption vector;
        // the rest are generic shell-injection/whitespace rejects.
        for bad in [
            "a|b",
            "a;b",
            "a b",
            "a'b",
            "a$b",
            "a`b",
            "a\nb",
            "/etc/passwd",
        ] {
            assert!(!valid_name(bad), "expected {bad:?} to be rejected");
        }
    }
}

#[cfg(test)]
mod protocol_gate_tests {
    use super::{protocol_gate, ProtocolGate};

    #[test]
    fn accepts_matching_protocol() {
        // The backend's own PROTOCOL_VERSION always matches itself.
        assert_eq!(
            protocol_gate(sot_protocol::PROTOCOL_VERSION),
            ProtocolGate::Accept
        );
        // Concretely, protocol 1 is accepted today.
        assert_eq!(protocol_gate(1), ProtocolGate::Accept);
    }

    #[test]
    fn accepts_preversioning_under_grace_at_v1() {
        // A pre-versioning frontend (protocol == 0) is accepted under the
        // one-time transition grace WHILE PROTOCOL_VERSION is 1. This test is
        // meaningful only at v1; it documents the grace and will need updating
        // when we bump to v2 (at which point 0 must reject — see the next test's
        // rationale).
        assert_eq!(sot_protocol::PROTOCOL_VERSION, 1, "grace is v1-only");
        assert_eq!(protocol_gate(0), ProtocolGate::AcceptLegacy);
    }

    #[test]
    fn rejects_mismatched_protocol() {
        // A newer frontend on protocol 2 (or any non-equal, non-0 value) is
        // rejected — the FE renders the "update needed" screen.
        assert_eq!(protocol_gate(2), ProtocolGate::Reject);
        assert_eq!(protocol_gate(99), ProtocolGate::Reject);
    }
}

#[cfg(test)]
mod preview_gate_tests {
    use super::is_bounded_output_plugin;
    use std::path::Path;

    #[test]
    fn bounded_output_plugins_exempt_from_size_gate() {
        // HDF5 + video are metadata/poster-only → bounded output → must NOT be
        // skipped on big input (the multi-GB .h5 freeze bug).
        for p in [
            "data.h5",
            "scan.hdf5",
            "old.hdf",
            "DATA.H5",
            "clip.mp4",
            "v.mkv",
        ] {
            assert!(
                is_bounded_output_plugin(Path::new(p)),
                "{p} should be exempt"
            );
        }
        // Plugins whose output scales with input (or plain files) stay gated.
        for p in ["mod.jl", "notes.txt", "data.json", "big.csv", "noext"] {
            assert!(
                !is_bounded_output_plugin(Path::new(p)),
                "{p} should be gated"
            );
        }
    }
}

#[cfg(test)]
mod file_transfer_tests {
    use super::{dedup_upload_name, is_safe_upload_name};

    #[test]
    fn upload_name_safety_rejects_traversal() {
        // Accept plain basenames (incl. spaces + the de-dup suffix shape).
        assert!(is_safe_upload_name("data.csv"));
        assert!(is_safe_upload_name("my report (1).txt"));
        // Reject anything that could escape the target dir.
        assert!(!is_safe_upload_name(""));
        assert!(!is_safe_upload_name("   "));
        assert!(!is_safe_upload_name("../etc/passwd"));
        assert!(!is_safe_upload_name("a/b.txt"));
        assert!(!is_safe_upload_name("a\\b.txt"));
        assert!(!is_safe_upload_name("."));
        assert!(!is_safe_upload_name(".."));
    }

    #[test]
    fn dedup_suffixes_on_collision() {
        let dir = std::env::temp_dir().join(format!("sot-ul-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Free name → unchanged.
        assert_eq!(dedup_upload_name(&dir, "x.txt"), "x.txt");
        // Collisions insert ` (n)` before the extension.
        std::fs::write(dir.join("x.txt"), b"").unwrap();
        assert_eq!(dedup_upload_name(&dir, "x.txt"), "x (1).txt");
        std::fs::write(dir.join("x (1).txt"), b"").unwrap();
        assert_eq!(dedup_upload_name(&dir, "x.txt"), "x (2).txt");
        // Extensionless names get the suffix at the end.
        assert_eq!(dedup_upload_name(&dir, "data"), "data");
        std::fs::write(dir.join("data"), b"").unwrap();
        assert_eq!(dedup_upload_name(&dir, "data"), "data (1)");

        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod preview_downsample_tests {
    use super::{
        downsample_oversize_raster, is_downsampleable_raster, PREVIEW_DOWNSAMPLE_MAX_DIM,
        PREVIEW_DOWNSAMPLE_TRIGGER,
    };

    #[test]
    fn raster_mime_gate() {
        for m in [
            "image/png",
            "image/jpeg",
            "image/webp",
            "image/tiff",
            "image/gif",
            "image/bmp",
        ] {
            assert!(is_downsampleable_raster(m), "{m} should be downsampleable");
        }
        for m in [
            "image/svg+xml",
            "text/plain",
            "application/pdf",
            "video/mp4",
        ] {
            assert!(!is_downsampleable_raster(m), "{m} must NOT be downsampled");
        }
    }

    #[test]
    fn ships_raw_when_gates_fail() {
        // Under the size trigger → ship raw (None), no decode attempted.
        assert!(downsample_oversize_raster("image/png", &vec![0u8; 4096]).is_none());
        // Over the trigger but a non-raster mime → ship raw.
        let big = vec![0u8; PREVIEW_DOWNSAMPLE_TRIGGER + 1];
        assert!(downsample_oversize_raster("application/pdf", &big).is_none());
    }

    #[test]
    fn downsizes_oversize_raster_to_cap() {
        use image::ImageEncoder;
        // Build a raster whose longest side exceeds the cap and whose PNG clears
        // the byte trigger (xorshift-noise defeats DEFLATE so it stays large).
        let (w, h) = (PREVIEW_DOWNSAMPLE_MAX_DIM + 800, 1600u32);
        let mut buf = Vec::with_capacity((w * h * 4) as usize);
        let mut s: u32 = 0x9e3779b9;
        for _ in 0..(w * h) {
            for _ in 0..4 {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                buf.push((s & 0xff) as u8);
            }
        }
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&buf, w, h, image::ExtendedColorType::Rgba8)
            .unwrap();
        assert!(
            png.len() > PREVIEW_DOWNSAMPLE_TRIGGER,
            "noise PNG {} must exceed the {}-byte trigger to exercise downsample",
            png.len(),
            PREVIEW_DOWNSAMPLE_TRIGGER
        );

        let out = downsample_oversize_raster("image/png", &png).expect("should downsample");
        assert!(out.len() < png.len(), "downsized bytes must be smaller");
        let (ow, oh) = image::ImageReader::new(std::io::Cursor::new(&out))
            .with_guessed_format()
            .unwrap()
            .into_dimensions()
            .unwrap();
        assert!(
            ow.max(oh) <= PREVIEW_DOWNSAMPLE_MAX_DIM,
            "longest side {} must be <= cap {}",
            ow.max(oh),
            PREVIEW_DOWNSAMPLE_MAX_DIM
        );
        assert_eq!(
            ow.max(oh),
            PREVIEW_DOWNSAMPLE_MAX_DIM,
            "longest side scaled to the cap"
        );
    }
}
