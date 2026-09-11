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
    PlutoOpenReq, PlutoOpenRes, PreviewGetReq, PreviewGetRes, PreviewSetScaleReq, PtyCursor,
    PtyInputReq, PtyInputRes, PtyScreenReq, PtyScreenRes, QuartoOpenReq, QuartoOpenRes,
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
use crate::workspaces::{AgentMessage, Workspace, WorkspaceChanged, Workspaces};
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
        // ADR 0035: this daemon accepts `proxy.connect` — the FE arms its
        // lazy loopback proxy listeners so backend pages reach a remote FE
        // through the one control tunnel, no per-port ssh forward.
        proxy: true,
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

/// `version.query` (ADR 0030 §8 decision 31b, ADR 0043 decision 31): pure
/// in-memory, no fan-out, no supervisor probe — this daemon's own version
/// triple plus the roster of currently-attached frontends, sourced from
/// their hellos. Never fails: an empty `clients` list from a daemon with
/// zero OTHER attached frontends is a legitimate answer, not an error.
///
/// Each row also carries `fe_handle`/`active`: ONE `snapshot_with_active()`
/// call supplies both the roster and the winner from the same lock + the
/// same `now` (2026-09-08 review, finding 6 — two separate reads could
/// disagree under concurrent `fe.presence` traffic), and `active` is set by
/// comparing SERIALS, never handles, so a duplicate-handle connection that
/// isn't the winner is never marked active alongside it (finding 5).
pub async fn handle_version_query(
    req_id: u64,
    clients: &crate::clients::Clients,
) -> Result<HandlerOutput> {
    let daemon = sot_protocol::DaemonVersion {
        app_version: sot_protocol::app_version(),
        protocol: sot_protocol::PROTOCOL_VERSION,
        lane_build: sot_log::exchange::SUPERVISOR_LANE_BUILD_ID.to_string(),
        lane_proto: sot_log::wire::SUPERVISOR_PROTO_V1,
    };
    let snap = clients.snapshot_with_active();
    let clients = snap
        .clients
        .iter()
        .map(|c| sot_protocol::ClientVersion {
            client_id: c.client_id.clone(),
            app_version: c.app_version.clone(),
            protocol: c.protocol,
            fe_handle: c.fe_handle.clone(),
            active: snap.is_active_serial(c.serial),
        })
        .collect();
    let res = sot_protocol::VersionQueryRes { daemon, clients };
    Ok(vec![(
        Frame::res(req_id, op::VERSION_QUERY, serde_json::to_value(res)?),
        None,
    )])
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
/// Returns `Some((png, scale))` ONLY when it actually shrank the image (`scale`
/// is the linear downsample factor in (0,1), the same for both axes); `None`
/// (ship raw) when the mime isn't a raster, the file is under the trigger, the
/// dimensions already fit, or decode/encode fails. The caller uses `scale` to
/// rescale a `physical_scale` sidecar to per-served-pixel (ADR 0034 / F1).
/// CPU-bound — call via `spawn_blocking`.
fn downsample_oversize_raster(mime: &str, bytes: &[u8]) -> Option<(Vec<u8>, f32)> {
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
    Some((out, scale))
}

/// Resolve the bytes actually put on the wire: a downsized PNG for an oversize
/// raster, else the input unchanged. Owns its args so it can run in
/// `spawn_blocking` and always hand the (possibly original) bytes back.
fn preview_bytes_for_wire(mime: String, bytes: Vec<u8>) -> (String, Vec<u8>, Option<f32>) {
    match downsample_oversize_raster(&mime, &bytes) {
        Some((png, scale)) => ("image/png".to_string(), png, Some(scale)),
        None => (mime, bytes, None),
    }
}

/// Multiply every axis's `nm_per_px` in `extras.physical_scale` by `factor`.
/// Used after a preview downsample: the `<img>.scale.json` sidecar calibrates
/// per ORIGINAL pixel, but a downsampled preview is served at a smaller width,
/// and the FE keys the scalebar off the SERVED width — so per-served-pixel
/// nm_per_px = per-orig-px / downsample_scale (i.e. `factor = 1/scale > 1`).
/// The downsample is geometrically uniform, so the same `factor` applies to
/// every axis and any anisotropy (x vs z) is preserved. No-op when there is no
/// `physical_scale` (or it has no numeric `nm_per_px`).
fn rescale_physical_scale(
    extras: Option<serde_json::Value>,
    factor: f64,
) -> Option<serde_json::Value> {
    let mut root = match extras {
        Some(serde_json::Value::Object(m)) => m,
        other => return other,
    };
    if let Some(serde_json::Value::Object(ps)) = root.get_mut("physical_scale") {
        if let Some(serde_json::Value::Array(axes)) = ps.get_mut("axes") {
            for ax in axes.iter_mut() {
                if let Some(v) = ax.get_mut("nm_per_px") {
                    if let Some(n) = v.as_f64() {
                        *v = serde_json::json!(n * factor);
                    }
                }
            }
        }
    }
    Some(serde_json::Value::Object(root))
}

/// Shared preview assembly for `preview.get` AND `preview.set_scale`'s re-emit:
/// resolve the node → (dir stub | plugin preview | bytes fallback), merge the
/// `<image>.scale.json` scale sidecar, and apply the F1 downsample rescale.
///
/// The OUTER `anyhow::Result` carries INFRA failures (a downsample task panic)
/// so both callers' `?` yields the server's `handler_error` — preserving
/// `preview.get`'s prior behavior when the `spawn_blocking` join fails. The
/// INNER `Result` carries DOMAIN errors (bad node id / read failure) that the
/// caller turns into a frame under its own op. Uses `node_id_to_path` (the READ
/// resolver) — write confinement is the caller's concern.
async fn build_preview_payload(
    ws: &Workspace,
    req: &PreviewGetReq,
) -> anyhow::Result<
    std::result::Result<(String, Vec<u8>, Option<serde_json::Value>), (String, String)>,
> {
    let files_mode = match ws.files_mode() {
        Ok(fm) => fm,
        Err(e) => {
            return Ok(Err((
                "files_mode_init_failed".to_string(),
                format!("files_mode init failed: {e:#}"),
            )))
        }
    };
    let kernel = ws.kernel();

    let path = match files_mode.node_id_to_path(&req.node_id) {
        Ok(p) => p,
        Err(e) => return Ok(Err(("bad_node_id".to_string(), format!("{e:#}")))),
    };

    // A plugin match, resolved up front so the `is_dir`/plugin/fallback
    // three-way below can stay a plain `if`/`else if`/`else` — `Err` means
    // the kernel is unavailable for a file type with no sane bytes-level
    // fallback, and short-circuits this whole function.
    let plugin_matched = if path.is_dir() {
        None
    } else {
        match try_plugin_preview(&kernel, &path, &req.node_id, req.page, req.fit_w, req.fit_h)
            .await
        {
            Ok(matched) => matched,
            Err(code_msg) => return Ok(Err(code_msg)),
        }
    };

    // The 4th element is preview PROVENANCE: true only when `bytes` is the
    // file itself. `merge_png_phys_scale` is gated on it — see its doc for
    // why a plugin's generated blob must never be read for embedded scale.
    let (mime, bytes, extras, raw_file_bytes) = if path.is_dir() {
        // Directory preview: short markdown stub naming the dir. Frontend
        // would otherwise render an empty pane on dir selection.
        let label = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_else(|| path.to_str().unwrap_or("/"));
        let md = ROOT_PREVIEW_TEMPLATE.replace("{root}", label);
        ("text/markdown".to_string(), md.into_bytes(), None, false)
    } else if let Some((mime, bytes, extras)) = plugin_matched {
        // Plugin-routed preview: a loaded FileType plugin claimed this
        // path. Use the plugin's mime + decoded blob — that's how
        // HDF5Preview, JuliaSource, MarkdownDoc, and any future plugin
        // surface their previews end-to-end.
        (mime, bytes, extras, false)
    } else {
        // No plugin claim (or kernel unavailable / errored — logged in
        // `try_plugin_preview`). Fall back to the bytes-level reader so
        // files outside any plugin's coverage still get served, with
        // mime inferred from extension. `read_bytes_preview` is a
        // synchronous `std::fs::read` (up to `PREVIEW_BINARY_CAP` bytes) —
        // real blocking I/O, so it runs via `spawn_blocking` rather than
        // inline on this async task.
        let path_for_blk = path.clone();
        let node_id_for_blk = req.node_id.clone();
        match tokio::task::spawn_blocking(move || read_bytes_preview(&path_for_blk, &node_id_for_blk))
            .await
            .context("preview bytes-level read task")?
        {
            Ok((mime, bytes)) => (mime, bytes, None, true),
            Err(e) => return Ok(Err(("io_error".to_string(), format!("read {path:?}: {e}")))),
        }
    };

    // ADR 0034: attach a `<path>.scale.json` sidecar's contents as
    // `extras.physical_scale` so the FE can render a dynamic scalebar on
    // raster previews. Backend-side (rasters are served here, not via the
    // kernel) — `merge_scale_sidecar` is a synchronous `std::fs::read_to_string`,
    // so it runs via `spawn_blocking` too.
    let path_for_scale = path.clone();
    let mime_for_scale = mime.clone();
    let extras = tokio::task::spawn_blocking(move || {
        merge_scale_sidecar(&path_for_scale, &mime_for_scale, extras)
    })
    .await
    .context("preview scale-sidecar read task")?;
    // ADR 0034 tier 2 (embedded metadata, PNG half): a PNG that declares its
    // own density via `pHYs` gets a scalebar with no sidecar on disk. Fills
    // only when the sidecar tier produced nothing (resolution order, §1).
    let extras = merge_png_phys_scale(&bytes, &mime, extras, raw_file_bytes);

    // Ship an oversize raster as a preview-sized PNG instead of the raw file.
    // Only oversize rasters take the spawn_blocking detour (decode is CPU-heavy
    // and must stay off the async reactor). A panicking decode is near-impossible
    // (the helper is all `.ok()?`); a JoinError is INFRA — propagate via `?` so
    // the server returns `handler_error` exactly as it did before this refactor.
    let (mime, bytes, downsample_scale) =
        if bytes.len() > PREVIEW_DOWNSAMPLE_TRIGGER && is_downsampleable_raster(&mime) {
            tokio::task::spawn_blocking(move || preview_bytes_for_wire(mime, bytes))
                .await
                .context("preview downsample task")?
        } else {
            (mime, bytes, None)
        };

    // ADR 0034 / F1: when we ship a DOWNSAMPLED raster, the served image is
    // narrower than the source, but the sidecar `physical_scale` is calibrated
    // per ORIGINAL pixel. The FE keys the scalebar off the served width, so
    // rescale each axis to per-served-pixel (× 1/scale) — otherwise the bar is
    // wrong by the downsample factor (a 12000→6000 image labels every bar 2×).
    let extras = match downsample_scale {
        Some(scale) if scale > 0.0 => rescale_physical_scale(extras, 1.0 / scale as f64),
        _ => extras,
    };

    Ok(Ok((mime, bytes, extras)))
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

    match build_preview_payload(&ws, &req).await? {
        Ok((mime, bytes, extras)) => {
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
        Err((code, msg)) => Ok(vec![(
            Frame::res(req_id, op::PREVIEW_GET, json!({ "error": msg, "code": code })),
            None,
        )]),
    }
}

/// A `physical_scale` is valid iff it's an object with a non-empty `axes` array
/// where every axis has BOTH a string `name` (the FE labels each bar by it) and
/// a finite, strictly-positive numeric `nm_per_px`, plus a top-level string
/// `unit`. Guards `preview.set_scale` from persisting garbage that would then
/// mislabel (or fail to label) every bar.
fn physical_scale_is_valid(v: &serde_json::Value) -> bool {
    let Some(obj) = v.as_object() else {
        return false;
    };
    if !obj.get("unit").map(|u| u.is_string()).unwrap_or(false) {
        return false;
    }
    match obj.get("axes").and_then(|a| a.as_array()) {
        Some(axes) if !axes.is_empty() => axes.iter().all(|ax| {
            let has_name = ax.get("name").map(|n| n.is_string()).unwrap_or(false);
            let good_per_px = ax
                .get("nm_per_px")
                .and_then(|n| n.as_f64())
                .map(|n| n.is_finite() && n > 0.0)
                .unwrap_or(false);
            has_name && good_per_px
        }),
        _ => false,
    }
}

/// Monotone per-process counter so two concurrent `set_scale` writes to the same
/// image within one microsecond get DISTINCT temp names (see below).
static SCALE_TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Write `sidecar` (already built + workspace-confined by the caller) atomically:
/// a UNIQUE, `O_EXCL`-created temp in the same dir + rename. The `physical_scale`
/// is written VERBATIM — it is the per-original-px value the user typed; never
/// derive it from an emitted/rescaled value or a read-served → write-back would
/// compound the F1 ratio and corrupt the sidecar every round-trip.
///
/// The temp name is `<sidecar>.tmp.<pid>.<seq>.<micros>` AND created with
/// `create_new(true)` (`O_EXCL`), so two clients writing the same image can't
/// share a temp file (one would get interleaved/partial bytes then rename the
/// other's) — a name collision errors instead. Rename is atomic on one fs.
fn write_scale_sidecar_atomic(
    sidecar: &std::path::Path,
    physical_scale: &serde_json::Value,
) -> std::io::Result<()> {
    use std::io::Write;

    let seq = SCALE_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0);
    let mut tmp = sidecar.as_os_str().to_os_string();
    tmp.push(format!(".tmp.{}.{}.{}", std::process::id(), seq, micros));
    let tmp = std::path::PathBuf::from(tmp);

    let bytes = serde_json::to_vec(physical_scale)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    // create_new (O_EXCL): a temp-name COLLISION errors HERE — return without
    // touching `tmp`; it belongs to the other writer, not us. Only AFTER a
    // successful create is `tmp` ours to clean up.
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)?;
    let write_res = f.write_all(&bytes);
    drop(f); // close before rename (correct on every platform)
    if let Err(e) = write_res {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, sidecar) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// `preview.set_scale` (ADR 0034 §5): persist a user-entered physical scale as
/// an `<image>.scale.json` sidecar, then re-emit the preview so the FE renders
/// the scalebar. The reply is the same `PreviewGetRes` envelope as `preview.get`
/// (frame-id correlated by the FE), returned under `op::PREVIEW_SET_SCALE`.
pub async fn handle_preview_set_scale(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: PreviewSetScaleReq =
        serde_json::from_value(payload_json).context("preview.set_scale payload")?;
    tracing::info!(
        node_id = %req.node_id,
        workspace_id = req.workspace_id.as_deref().unwrap_or("<default>"),
        "preview.set_scale"
    );

    let err = |code: &str, msg: String| -> Result<HandlerOutput> {
        Ok(vec![(
            Frame::res(
                req_id,
                op::PREVIEW_SET_SCALE,
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
    // Resolve with the READ resolver — the SAME rule `preview.get` uses to serve
    // these bytes — so "can preview" and "can calibrate" AGREE. The confined
    // WRITE resolver rejected the STANDARD layout where a results dir is a
    // symlink to external storage (e.g. `data/results` -> /mnt/nas/...): the user
    // could view the render but not calibrate it (bad_node_id). If the read path
    // is trusted to serve an image's bytes, it's trusted to choose where the
    // image's own sidecar lands. String-level `../`/absolute node ids are still
    // rejected in the resolver; only user-created in-root symlinks are followed,
    // exactly as for reads, and the daemon runs as the user so OS permissions
    // still bound the write.
    let path = match files_mode.node_id_to_path(&req.node_id) {
        Ok(p) => p,
        Err(e) => return err("bad_node_id", format!("{e:#}")),
    };
    // Must be an EXISTING regular file: `mime_for_path` is extension-only, so a
    // DIRECTORY named `results.png` would otherwise pass the raster gate (and
    // get a markdown stub preview), and a since-deleted image would leave an
    // orphan sidecar.
    if !path.is_file() {
        return err(
            "not_a_file",
            format!("{path:?} is not an existing regular file"),
        );
    }
    if !is_downsampleable_raster(mime_for_path(&path)) {
        return err(
            "not_a_raster",
            format!(
                "{path:?} is not a scalebar-capable raster (mime {})",
                mime_for_path(&path)
            ),
        );
    }
    if !physical_scale_is_valid(&req.physical_scale) {
        return err(
            "bad_scale",
            "physical_scale must be {axes:[{name,nm_per_px>0}], unit:<string>}".to_string(),
        );
    }

    // The sidecar lives BESIDE the image (the read-resolved path, symlinks and
    // all — so on the NAS target if that's where the image lives). NO
    // project_root confinement: it would reject the symlinked-results layout
    // above, and the read path already trusts this resolution to serve the
    // image's bytes. Calibration is the image's metadata and must travel WITH
    // the data (other tools reading that dir find it). Write safety is preserved
    // by the gates above (is_file + raster + valid scale) and the O_EXCL temp +
    // atomic rename in `write_scale_sidecar_atomic`; the resolver already
    // rejected string-level escapes.
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(".scale.json");
    let sidecar = std::path::PathBuf::from(sidecar);

    if let Err(e) = write_scale_sidecar_atomic(&sidecar, &req.physical_scale) {
        return err("io_error", format!("write scale sidecar {sidecar:?}: {e}"));
    }

    // Re-emit the preview so the FE renders from OUR authoritative rescale (it
    // can't know the served/source ratio after a downsample). merge_scale_sidecar
    // re-reads the sidecar we just wrote; the F1 rescale runs inside the helper.
    let get_req = PreviewGetReq {
        node_id: req.node_id.clone(),
        workspace_id: req.workspace_id.clone(),
        page: None,
        fit_w: None,
        fit_h: None,
    };
    match build_preview_payload(&ws, &get_req).await? {
        Ok((mime, bytes, extras)) => {
            let res = PreviewGetRes {
                mime: mime.clone(),
                blob: BlobDescriptor {
                    len: bytes.len() as u64,
                    mime,
                },
                extras,
            };
            let rev = session
                .bump("preview.scale_set", json!({ "node_id": req.node_id }))
                .await;
            Ok(vec![(
                Frame::res(req_id, op::PREVIEW_SET_SCALE, serde_json::to_value(res)?).with_rev(rev),
                Some(bytes),
            )])
        }
        Err((code, msg)) => err(&code, msg),
    }
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
/// `Ok(Some(...))`: a plugin claimed the path and rendered it. `Ok(None)`:
/// no plugin matched, OR the kernel failed in some way that still leaves the
/// bytes-level fallback a REASONABLE degrade (a live kernel's wire/protocol
/// error, or the kernel being `Dead` for a file type the bytes-level reader
/// can still show something sane for, e.g. raw Julia source as plain text).
/// `Err((code, msg))`: the kernel is confirmed `Dead` (see `KernelDead`) AND
/// this path is a bounded-output-only plugin (`is_bounded_output_plugin` —
/// HDF5/video/PDF) where the bytes-level fallback would serve raw binary
/// nonsense instead of a preview; callers surface this straight to the FE
/// as "Julia kernel unavailable: <reason>" rather than silently degrading.
async fn try_plugin_preview(
    kernel: &Kernel,
    path: &std::path::Path,
    node_id: &str,
    page: Option<u32>,
    fit_w: Option<u32>,
    fit_h: Option<u32>,
) -> std::result::Result<Option<(String, Vec<u8>, Option<serde_json::Value>)>, (String, String)> {
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
            return Ok(None);
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
            // Kernel unavailable (dead or still starting): for a bounded-
            // output plugin (HDF5/video/PDF) the bytes-level fallback below
            // would serve raw binary nonsense, not a degraded-but-sane
            // preview — surface the reason instead. Every other file type
            // (and every other kind of kernel failure — a live kernel's own
            // wire/protocol error) keeps the existing silent fallback: raw
            // bytes are still a reasonable thing to show for, say, Julia
            // source.
            if let Some(unavailable) = e.downcast_ref::<crate::kernel::KernelUnavailable>() {
                if is_bounded_output_plugin(path) {
                    tracing::warn!(
                        %node_id,
                        %unavailable,
                        "kernel unavailable for a bounded-output-only file type; no usable fallback"
                    );
                    return Err((
                        "kernel_unavailable".to_string(),
                        format!("Julia kernel unavailable: {unavailable}"),
                    ));
                }
                tracing::warn!(%node_id, %unavailable, "kernel unavailable; falling back to bytes-level reader");
                return Ok(None);
            }
            tracing::warn!(
                %node_id,
                error = %e,
                "kernel.file.preview failed; falling back to bytes-level reader"
            );
            return Ok(None);
        }
    };
    let matched = v.get("matched").and_then(|m| m.as_bool()).unwrap_or(false);
    if !matched {
        return Ok(None);
    }
    let Some(mime) = v.get("mime").and_then(|m| m.as_str()) else {
        return Ok(None);
    };
    let mime = mime.to_string();
    // Prefer `blob_base64` (canonical, supports binary). Plain `text` is also
    // emitted for text/* mimes — but decoding base64 still gives the right
    // bytes either way, so we route everything through the same path.
    let Some(b64) = v.get("blob_base64").and_then(|b| b.as_str()) else {
        return Ok(None);
    };
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
            return Ok(None);
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
            return Ok(None);
        }
    }
    // Plugin-reported metadata (page/page_count, …) — forwarded verbatim,
    // opaque here (ADR 0021).
    let extras = v.get("extras").cloned();
    Ok(Some((mime, bytes, extras)))
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

/// ADR 0034: for a raster image preview, look for a `<path>.scale.json` sidecar
/// and surface its JSON as `extras.physical_scale` (the FE renders a dynamic
/// scalebar from it). Read backend-side because raster previews are served here
/// (`read_bytes_preview`), not via the kernel — and a JSON sidecar is not image
/// metadata, so it doesn't cross the "Rust never parses image metadata" line.
///
/// Best-effort + opaque: a missing/unparseable sidecar leaves `extras` untouched;
/// the sidecar's JSON (expected `{axes:[{name,nm_per_px}],unit}`) is passed
/// through as-is — the FE validates the shape. Merges into an existing `extras`
/// object (e.g. a plugin's) rather than clobbering it.
fn merge_scale_sidecar(
    path: &std::path::Path,
    mime: &str,
    extras: Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    if !is_downsampleable_raster(mime) {
        return extras; // not a raster we scalebar
    }
    // `<path>.scale.json` — append to the FULL path (so `img.png` → `img.png.scale.json`).
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(".scale.json");
    let sidecar = std::path::PathBuf::from(sidecar);
    let Ok(text) = std::fs::read_to_string(&sidecar) else {
        return extras; // no sidecar
    };
    let scale: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(sidecar = %sidecar.display(), error = %e,
                "scale sidecar is not valid JSON — ignoring");
            return extras;
        }
    };
    let mut obj = match extras {
        Some(serde_json::Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    obj.insert("physical_scale".to_string(), scale);
    Some(serde_json::Value::Object(obj))
}

/// ADR 0034 resolver tier 2 (embedded metadata), PNG half: read a `pHYs`
/// chunk's pixels-per-metre as `nm_per_px`. Returns `(x_nm_per_px,
/// y_nm_per_px)` when the PNG declares a metre-unit density (`unit == 1`)
/// with nonzero counts; `None` for an absent/other-unit `pHYs` (unit 0 is
/// aspect ratio only, dimensionless) or a malformed stream. Read-only: SoT
/// never WRITES `pHYs` (u32 px/metre quantizes nm-scale values — the
/// sidecar is the exact write channel; ADR 0034 §5).
///
/// A hand-rolled chunk walk, not an image decode: `pHYs` must precede
/// `IDAT`, so this touches a handful of header chunks and never the pixel
/// data. Bounds-checked throughout; any structural surprise returns `None`
/// (best-effort tier).
///
/// This is CALIBRATION, so the spec is enforced rather than approximated
/// (W3C PNG 3 §§5.3, 5.6, 11.3.4.3) — a scalebar that is confidently wrong
/// is worse than no scalebar:
///   * the chunk CRC is verified before the value is trusted, so bit-rot in
///     the density can't silently relabel an image;
///   * `pHYs` must be exactly 9 bytes, and a second one is a malformed
///     stream (the spec permits at most one) — both reject outright rather
///     than skipping to a later, more agreeable chunk;
///   * the walk is bounded. A chunk header is 12 bytes, so a large file can
///     declare tens of millions of empty chunks; this runs on the async
///     reactor, so cap the header scan instead of letting a crafted PNG
///     monopolize a worker. A conforming PNG has a handful before `IDAT`.
fn png_phys_nm_per_px(bytes: &[u8]) -> Option<(f64, f64)> {
    const SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    const MAX_HEADER_CHUNKS: usize = 4096;
    if bytes.len() < SIG.len() || bytes[..SIG.len()] != SIG {
        return None;
    }
    let mut off = SIG.len();
    let mut scale = None;
    let mut seen_phys = false;
    // Chunk layout: len(4) + type(4) + data(len) + crc(4).
    for _ in 0..MAX_HEADER_CHUNKS {
        if off + 8 > bytes.len() {
            return scale;
        }
        let len = u32::from_be_bytes(bytes[off..off + 4].try_into().ok()?) as usize;
        let type_start = off + 4;
        let data_start = off + 8;
        let data_end = data_start.checked_add(len)?;
        let crc_end = data_end.checked_add(4)?;
        if crc_end > bytes.len() {
            return None; // truncated chunk
        }
        match &bytes[type_start..data_start] {
            b"pHYs" => {
                // At most one, exactly 9 bytes — anything else is malformed.
                if seen_phys || len != 9 {
                    return None;
                }
                seen_phys = true;
                // CRC covers the type field AND the data, not the data alone.
                let declared = u32::from_be_bytes(bytes[data_end..crc_end].try_into().ok()?);
                if crc32fast::hash(&bytes[type_start..data_end]) != declared {
                    return None;
                }
                let d = &bytes[data_start..data_end];
                let ppm_x = u32::from_be_bytes(d[0..4].try_into().ok()?);
                let ppm_y = u32::from_be_bytes(d[4..8].try_into().ok()?);
                // unit 0 is a legal chunk carrying only an aspect ratio, so
                // it yields no scale but is not a malformed stream — keep
                // walking so a duplicate after it is still caught.
                if d[8] == 1 && ppm_x != 0 && ppm_y != 0 {
                    scale = Some((1e9 / ppm_x as f64, 1e9 / ppm_y as f64));
                }
            }
            b"IDAT" | b"IEND" => return scale, // pHYs must precede IDAT
            _ => {}
        }
        off = crc_end;
    }
    None // header-chunk budget exhausted: not a shape we trust
}

/// ADR 0034 tier 2 merge: when no higher tier (the sidecar) produced a
/// `physical_scale`, fall back to the PNG's own `pHYs` declaration — the
/// in-file channel scale-producing pipelines prefer, since the PNG stays a
/// single self-describing artifact. Emits the same `physical_scale` schema
/// as the sidecar (named x/y axes, `nm`), so the FE and the downsample
/// rescale can't tell tiers apart. The sidecar keeps priority: it carries
/// exact floats and is what `preview.set_scale` writes, so a user-entered
/// correction overrides a wrong in-file value.
///
/// `raw_file_bytes` gates the whole tier, and is NOT a formality. `bytes` at
/// the call site is either the file itself or a FileType plugin's GENERATED
/// blob, and for a plugin the two are not interchangeable: re-encoding can
/// drop the source `pHYs` (tier lost), resizing can preserve a now-wrong one
/// (bar wrong by the resize factor), and a rasterizer can emit its own render
/// DPI that has nothing to do with the subject — `pdftoppm`, behind the
/// shipped PDF plugin, stamps 144 DPI. No shipped plugin claims `.png` today
/// and `.pdf` is excluded from image previews FE-side, so this is latent
/// rather than live; the gate is what keeps it that way when someone writes a
/// PNG plugin. A plugin that knows its own scale should put `physical_scale`
/// in `extras`, which it can do exactly, instead of having it guessed from
/// its output.
fn merge_png_phys_scale(
    bytes: &[u8],
    mime: &str,
    extras: Option<serde_json::Value>,
    raw_file_bytes: bool,
) -> Option<serde_json::Value> {
    if !raw_file_bytes || mime != "image/png" {
        return extras;
    }
    if matches!(&extras, Some(serde_json::Value::Object(m)) if m.contains_key("physical_scale")) {
        return extras;
    }
    let Some((x_nm, y_nm)) = png_phys_nm_per_px(bytes) else {
        return extras;
    };
    // The FE renders ONE bar from `axes[0]` by design (gpu.rs: "Isotropic
    // sources ship two equal axes; Phase 1 renders one bar"). A sidecar is
    // hand-authored, so unequal axes there are a deliberate act; `pHYs` is
    // read automatically off any file that happens to have one, so emitting
    // unequal axes here would silently label an anisotropic image with its x
    // scale alone. Drop it instead — no bar beats a wrong bar — until the FE
    // grows the two-axis renderer.
    if x_nm != y_nm {
        tracing::debug!(
            x_nm_per_px = x_nm,
            y_nm_per_px = y_nm,
            "ignoring anisotropic PNG pHYs: the scalebar renders one axis"
        );
        return extras;
    }
    let mut obj = match extras {
        Some(serde_json::Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    obj.insert(
        "physical_scale".to_string(),
        json!({
            "axes": [
                { "name": "x", "nm_per_px": x_nm },
                { "name": "y", "nm_per_px": y_nm },
            ],
            "unit": "nm",
        }),
    );
    Some(serde_json::Value::Object(obj))
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
    // `ConceptStore::read` is a synchronous `std::fs::read_to_string` — real
    // blocking I/O, so it runs via `spawn_blocking` rather than inline on
    // this async task.
    let target_for_blk = req.target.clone();
    let read_result = tokio::task::spawn_blocking(move || concept.read(&target_for_blk))
        .await
        .context("concept.read task")?;
    let (exists, content) = match read_result {
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
    // Resolve a RELATIVE path against the WORKSPACE ROOT and forward it
    // ABSOLUTE. The Julia child resolves relative paths against its own cwd
    // — inherited from the daemon, whose cwd is launch-context-dependent
    // (observed `$HOME` after a script restart) — so a workspace-relative
    // path like `dev/output/x.jl` resolved to a nonexistent `$HOME/dev/…`
    // and the run died as a missing-file res that the fire-and-forget path
    // DROPS silently (2026-07-24 field report: "--fresh include never
    // runs"). `sot-fe repl run` documents its path as workspace-relative;
    // make the daemon honor that contract deterministically.
    let path_str = path_str.map(|p| {
        let pb = std::path::PathBuf::from(&p);
        if pb.is_absolute() {
            p
        } else {
            let abs = ws.project_root.join(pb).display().to_string();
            if let Some(obj) = forwarded_payload.as_object_mut() {
                obj.insert("path".to_string(), serde_json::Value::String(abs.clone()));
            }
            abs
        }
    });
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
        // The workspace REPL defaults to the WORKSPACE PACKAGE's env — the #44
        // per-package env-fix, and the owner directive (2026-07-22): "the repl
        // should have this repo as its default env … defaults to each package's
        // env." A --fresh restart therefore activates SOT_WORKSPACE_ROOT's
        // Project.toml, SAME as the default (non-fresh) REPL — NOT a project
        // walked up/discovered from the file's path, which mis-picked a parent
        // (e.g. ~/dev when a relative path resolved there) and broke
        // `using <workspace package>`. Walk-up discovery is only the fallback
        // when the workspace root itself is not a package (no Project.toml).
        let (project_dir, project_source) = if ws.project_root.join("Project.toml").is_file() {
            (ws.project_root.clone(), "workspace")
        } else {
            closest_project_dir(&abs_path).unwrap_or_else(|| (ws.project_root.clone(), "fallback"))
        };
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
    // Daemon-side interrupt guard (P0.5 pre-work; twin of the #96 CLI
    // pre-flight, now enforced for EVERY caller — FE Ctrl-C and raw-socket
    // included): an interrupt must never be the thing that spawns a kernel.
    // Only a `ready` child is forwarded to; `starting` is refused too (the
    // serve loop isn't consuming yet — boot ≠ wedge, and a queued interrupt
    // would land on the first legitimate eval instead). `request_if_running`
    // closes the remaining race: even a stale `ready` reading cannot respawn.
    let state = repl.state();
    let result = match state {
        crate::repl::ReplLifecycle::Ready => {
            repl.request_if_running("repl.interrupt", payload_json).await
        }
        _ => Ok(None),
    };
    let (_, rev) = session.snapshot().await;
    let payload = match result {
        Ok(Some(v)) => v,
        Ok(None) => {
            // `ready` reaching here means the state read raced a child death
            // (request_if_running found the sender closed) — name that fact
            // so the note isn't self-contradictory ("no child" + "ready").
            let note = if state == crate::repl::ReplLifecycle::Ready {
                "no running repl child (repl_state=ready but supervisor sender closed — child just exited)".to_string()
            } else {
                format!("no running repl child (repl_state={})", state.as_str())
            };
            json!({ "interrupted": false, "note": note })
        }
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
                    format!(
                        "repl run is confined to the workspace root ({}); {} is outside it — \
                         use `repl eval --code 'include(\"{}\")'` for files elsewhere",
                        root.display(),
                        abs.display(),
                        abs.display(),
                    ),
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

/// Duplicate-root gate lookup (ADR 0036 Phase 1): the first registered
/// workspace whose `project_root` canonicalizes to `candidate_canon` while
/// carrying a slug OTHER than `incoming_slug` — excluding the inert default
/// anchor (`Workspaces::is_inert_default_anchor`, ADR 0042 amendment): it is
/// not a session and never runs an agent, so a real session at its root (a
/// local host's home dir) is not the two-agents-one-tree collision this gate
/// refuses. Same-slug matches are deliberately invisible here — a same-slug
/// create is the id-preserving metadata refresh `Workspaces::insert` has
/// always performed, and boot/spawn flows rely on that idempotence. A
/// registered root that no longer canonicalizes (deleted dir, dangling
/// symlink) is skipped, not fatal: judging that workspace is the Phase 2
/// reap's job, not the create path's.
fn find_other_workspace_with_root(
    candidate_canon: &std::path::Path,
    incoming_slug: &str,
    workspaces: &crate::workspaces::Workspaces,
) -> Option<std::sync::Arc<crate::workspaces::Workspace>> {
    workspaces.list().into_iter().find(|w| {
        w.slug != incoming_slug
            && !workspaces.is_inert_default_anchor(w)
            && w.project_root
                .canonicalize()
                .map(|c| c == candidate_canon)
                .unwrap_or(false)
    })
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
    // ACTUAL bound port, never the preferred `video_port()`: when the
    // preferred bind lost to another user's daemon (shared host), a URL
    // built on the preferred port would send this user's grant token to the
    // OTHER user's video server — "no such grant" for the user, token leak
    // to a stranger's process (2026-07-23 shared-host incident).
    let Some(port) = crate::http_serve::bound_video_port() else {
        let payload = json!({
            "error": "video server is not running (both preferred and ephemeral binds failed at startup) — check the daemon log",
            "code": "video_server_down",
        });
        return Ok(vec![(Frame::res(req_id, op::VIDEO_OPEN, payload), None)]);
    };
    let url = format!("http://127.0.0.1:{port}/{token}");
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
        // ACTUAL bound port, never the preferred `site_port()` — same
        // reasoning as `video.open` above: on a shared host the preferred
        // port may belong to another user's daemon.
        let Some(port) = crate::site_serve::bound_site_port() else {
            return Ok(err(
                "static-site server is not running (both preferred and ephemeral binds failed at startup) — check the daemon log".into(),
                "site_server_down",
            ));
        };
        format!(
            "http://127.0.0.1:{}/{}/{}",
            port,
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

    // The HTML rides as the trailing blob, NOT base64 in the envelope: an
    // `--embed-resources` render routinely blows past the codec's 1 MiB
    // envelope cap (a 1.2 MiB doc base64'd to 1.6 MiB), which used to fail the
    // write and take the whole connection down mid-session. `len` MUST equal
    // the bytes handed to `write_frame` or the next frame desyncs onto raw
    // HTML — see codec's `file_chunk_blob_is_consumed_no_desync`. Mirrors
    // math.render. Dropping base64 also sheds its +33% inflation.
    let res = QuartoOpenRes {
        blob: BlobDescriptor {
            len: bytes.len() as u64,
            mime: "text/html".to_string(),
        },
    };
    let (_, rev) = session.snapshot().await;
    Ok(vec![(
        Frame::res(req_id, op::QUARTO_OPEN, serde_json::to_value(res)?).with_rev(rev),
        Some(bytes),
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

/// `PtyInputReq::origin` / `PtyScreenReq` share no size limit of their own
/// — this one is `origin`'s: ADR 0042 amendment §1, "≤128 bytes, else
/// `bad_origin`."
const MAX_PTY_INPUT_ORIGIN_LEN: usize = 128;

/// One capsule-lane op's absolute deadline (ADR 0042 amendment: "the whole
/// operation runs under ONE deadline (5 s): attach, checkpoint, take,
/// input, ack, detach"). Shared by both `pty.input` and `pty.screen`'s
/// capsule arms — gated like `capsule_workspace::headless` itself, since
/// only those arms ever read it.
#[cfg(any(windows, target_os = "linux"))]
const CAPSULE_OP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// What a capsule-runtime `pty.input`/`pty.screen` op's `spawn_blocking`
/// closure reports — computed OFF the async runtime (the phase probe and
/// the headless client both make blocking IPC calls), then translated to a
/// response frame back on the async side. Gated like `capsule_workspace::
/// headless` itself (windows/linux only — that module simply does not
/// exist on any other host, so neither can a variant naming its error
/// type); the `#[cfg(not(...))]` arms in `handle_pty_input`/
/// `handle_pty_screen` never construct this enum at all on those hosts.
#[cfg(any(windows, target_os = "linux"))]
enum CapsuleOpOutcome<T> {
    Ok(T),
    NotReady(&'static str),
    Headless(crate::capsule_workspace::headless::HeadlessError),
}

/// Translates a [`CapsuleOpOutcome::Headless`] error into the typed error
/// payload ADR 0042 amendment §2 pins. `fail_code` is `capsule_input_failed`
/// or `capsule_screen_failed` depending on the caller; a `phase` of
/// `"record"` — the daemon could not learn the record's own verdict,
/// whether because the wire said `input_delivery_unknown` or because the
/// deadline expired after the input had already been handed to the lane —
/// overrides it to `capsule_input_unknown` regardless (ADR 0042 amendment
/// §2's "capsule_input_unknown when the deadline expired AFTER the input
/// was submitted", generalized to the wire's own explicit "unknown" answer
/// too: both cases mean the same thing, "we do not know if this landed in
/// the record," and the daemon never retries either one on its own).
#[cfg(any(windows, target_os = "linux"))]
fn headless_error_payload(
    e: crate::capsule_workspace::headless::HeadlessError,
    fail_code: &'static str,
) -> serde_json::Value {
    let code = if e.phase == "record" { "capsule_input_unknown" } else { fail_code };
    json!({
        "error": e.detail,
        "code": code,
        "phase": e.phase,
        "submitted": e.submitted,
    })
}

/// Resolves `req.origin` into a controller id, or an early error frame for
/// `bad_origin` (ADR 0042 amendment §1/§2: `origin` is caller-supplied
/// ATTRIBUTION, never authentication — exactly `HelloReq::client_id`'s own
/// trust level, just named explicitly instead of read off the connection).
fn resolve_pty_input_controller_id(
    req_id: u64,
    op_str: &str,
    origin: Option<&str>,
    connection_client_id: &str,
) -> std::result::Result<String, Frame> {
    match origin {
        Some(o) if o.len() > MAX_PTY_INPUT_ORIGIN_LEN => {
            let payload = json!({
                "error": format!("origin exceeds {MAX_PTY_INPUT_ORIGIN_LEN} bytes"),
                "code": "bad_origin",
            });
            Err(Frame::res(req_id, op_str, payload))
        }
        Some(o) => Ok(o.to_string()),
        None => Ok(connection_client_id.to_string()),
    }
}

#[cfg(test)]
mod pty_input_controller_id_tests {
    use super::*;

    #[test]
    fn absent_origin_falls_back_to_the_connection_client_id() {
        let id = resolve_pty_input_controller_id(1, op::PTY_INPUT, None, "conn-client").unwrap();
        assert_eq!(id, "conn-client");
    }

    #[test]
    fn present_origin_wins_over_the_connection_client_id() {
        let id = resolve_pty_input_controller_id(1, op::PTY_INPUT, Some("kitt-dev"), "conn-client").unwrap();
        assert_eq!(id, "kitt-dev");
    }

    #[test]
    fn origin_at_exactly_the_bound_is_accepted() {
        let origin = "x".repeat(MAX_PTY_INPUT_ORIGIN_LEN);
        let id = resolve_pty_input_controller_id(1, op::PTY_INPUT, Some(&origin), "conn-client").unwrap();
        assert_eq!(id, origin);
    }

    #[test]
    fn origin_one_over_the_bound_is_bad_origin() {
        let origin = "x".repeat(MAX_PTY_INPUT_ORIGIN_LEN + 1);
        let frame = resolve_pty_input_controller_id(1, op::PTY_INPUT, Some(&origin), "conn-client")
            .expect_err("an over-length origin must be refused");
        assert_eq!(frame.payload["code"], "bad_origin");
    }
}

/// ADR 0042 amendment (2026-09-07), decision 1: a session types into
/// ANOTHER row's pane by `workspace_id`. Unlike `pty.write` (this
/// connection's own pty, fire-and-forget), this is ANSWERED — a caller
/// with no pane to look at needs the outcome. `controller_id` is the
/// connection's own `hello` `client_id`, used only when `req.origin` is
/// absent; both are attribution, never authentication (the record stores
/// who typed, how many bytes, and when — never the content, which is
/// redacted in the WAL — and this op grants no privilege either name alone
/// could forge).
pub async fn handle_pty_input(
    req_id: u64,
    payload_json: serde_json::Value,
    workspaces: &Workspaces,
    connection_client_id: &str,
) -> Result<HandlerOutput> {
    let req: PtyInputReq = serde_json::from_value(payload_json).context("pty.input payload")?;

    let controller_id = match resolve_pty_input_controller_id(
        req_id,
        op::PTY_INPUT,
        req.origin.as_deref(),
        connection_client_id,
    ) {
        Ok(id) => id,
        Err(frame) => return Ok(vec![(frame, None)]),
    };

    let Some(ws) = workspaces.resolve(Some(&req.workspace_id)) else {
        let payload = json!({
            "error": format!("unknown workspace: {}", req.workspace_id),
            "code": "unknown_workspace",
        });
        return Ok(vec![(Frame::res(req_id, op::PTY_INPUT, payload), None)]);
    };

    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    let bytes = match STANDARD.decode(req.data_b64.as_bytes()) {
        Ok(b) => b,
        Err(e) => {
            let payload = json!({ "error": format!("data_b64: {e}"), "code": "bad_request" });
            return Ok(vec![(Frame::res(req_id, op::PTY_INPUT, payload), None)]);
        }
    };

    match ws.runtime.as_str() {
        "tmux" => {
            let text = match String::from_utf8(bytes.clone()) {
                Ok(t) => t,
                Err(_) => {
                    let payload = json!({
                        "error": "pty.input payload is not valid UTF-8 text on a tmux row",
                        "code": "input_not_text",
                    });
                    return Ok(vec![(Frame::res(req_id, op::PTY_INPUT, payload), None)]);
                }
            };
            let session = ws.tmux_session.clone();
            let enter = req.enter;
            let byte_len = bytes.len();
            let result = tokio::task::spawn_blocking(move || {
                let c = TmuxClient::new();
                c.send_keys_literal(&session, &text)?;
                if enter {
                    // CLI/ADR-level `--enter`: a SEPARATE `send-keys Enter`,
                    // never a byte appended to the literal text (`ops.rs`'s
                    // own `PtyInputReq::enter` doc — tmux's `-l` would
                    // deliver an embedded newline as a literal byte, not a
                    // submitted line).
                    c.send_enter(&session)?;
                }
                Ok::<(), anyhow::Error>(())
            })
            .await
            .context("spawn_blocking pty.input tmux")?;
            match result {
                Ok(()) => {
                    let res = PtyInputRes { ok: true, runtime: "tmux".into(), bytes: byte_len };
                    Ok(vec![(
                        Frame::res(req_id, op::PTY_INPUT, serde_json::to_value(res)?),
                        None,
                    )])
                }
                Err(e) => Ok(vec![(tmux_error_frame(req_id, op::PTY_INPUT, e), None)]),
            }
        }
        "capsule" => {
            #[cfg(any(windows, target_os = "linux"))]
            {
                let Some(state_root) = sot_log::state_dir::sot_state_dir() else {
                    let payload = json!({
                        "error": format!(
                            "could not resolve this machine's state root ({} unset)",
                            crate::capsule_workspace::STATE_ROOT_HINT
                        ),
                        "code": "capsule_input_failed",
                        "phase": "attach",
                    });
                    return Ok(vec![(Frame::res(req_id, op::PTY_INPUT, payload), None)]);
                };
                let state_dir =
                    crate::capsule_workspace::state_dir_for(&state_root, &ws.workspace_id);
                let enter = req.enter;
                // The ORIGINAL payload length — `PtyInputRes::bytes`'s own
                // doc ("the enter byte, if requested, is not counted"), so
                // this is captured BEFORE the CR (if any) is appended below.
                let byte_len = bytes.len();
                // ADR 0043 decision 33: `resume_if_absent` in place of a
                // bare `phase_of` read — a row whose supervisor died
                // between two headless ops resumes itself, under its own
                // guard, rather than answering `NotReady` forever. A
                // resume failure (an unknown workspace, a spawn error)
                // is logged and folds into `UNREACHABLE_PHASE`, same
                // shape `phase_of` itself always reported for a dead lane.
                let workspace_id = ws.workspace_id.clone();
                let agent_kind = ws.agent.clone();
                let agent_name = ws.agent_name.clone();
                let slug = ws.slug.clone();
                let project_root = ws.project_root.clone();
                let workspaces_for_resume = workspaces.clone();
                let outcome = tokio::task::spawn_blocking(move || {
                    let phase = match crate::capsule_workspace::resume_if_absent(
                        &state_root,
                        &workspace_id,
                        &agent_kind,
                        &agent_name,
                        &slug,
                        &project_root,
                        workspaces_for_resume,
                    ) {
                        Ok(phase) => phase,
                        Err(e) => {
                            tracing::warn!(workspace_id = %workspace_id, error = %e, "pty.input: resume_if_absent failed");
                            crate::capsule_workspace::UNREACHABLE_PHASE
                        }
                    };
                    let ready_phase =
                        crate::capsule_workspace::phase_str(sot_log::wire::SupervisorPhase::Ready);
                    if phase != ready_phase {
                        return CapsuleOpOutcome::NotReady(phase);
                    }
                    // `--enter` on a capsule row: a literal CR (0x0d)
                    // appended to the payload — the byte a terminal itself
                    // sends for Enter — never a converted trailing newline
                    // inside the caller's own text (`ops.rs`'s own doc).
                    let mut payload_bytes = bytes;
                    if enter {
                        payload_bytes.push(0x0d);
                    }
                    let deadline = std::time::Instant::now() + CAPSULE_OP_DEADLINE;
                    match crate::capsule_workspace::headless::type_into(
                        &state_dir,
                        &controller_id,
                        &payload_bytes,
                        deadline,
                    ) {
                        Ok(n) => CapsuleOpOutcome::Ok(n),
                        Err(e) => CapsuleOpOutcome::Headless(e),
                    }
                })
                .await
                .context("spawn_blocking pty.input capsule")?;
                match outcome {
                    CapsuleOpOutcome::Ok(_) => {
                        let res = PtyInputRes { ok: true, runtime: "capsule".into(), bytes: byte_len };
                        Ok(vec![(
                            Frame::res(req_id, op::PTY_INPUT, serde_json::to_value(res)?),
                            None,
                        )])
                    }
                    CapsuleOpOutcome::NotReady(phase) => {
                        let payload = json!({
                            "error": format!("capsule row not ready (phase: {phase})"),
                            "code": "capsule_not_ready",
                            "phase": phase,
                        });
                        Ok(vec![(Frame::res(req_id, op::PTY_INPUT, payload), None)])
                    }
                    CapsuleOpOutcome::Headless(e) => {
                        let payload = headless_error_payload(e, "capsule_input_failed");
                        Ok(vec![(Frame::res(req_id, op::PTY_INPUT, payload), None)])
                    }
                }
            }
            #[cfg(not(any(windows, target_os = "linux")))]
            {
                // `controller_id` is only ever consumed by the
                // windows/linux arm above; on any other host it is
                // resolved (for `bad_origin` validation) but never used.
                let _ = &controller_id;
                let payload = json!({
                    "error": "capsule runtime not available on this host",
                    "code": "capsule_input_failed",
                    "phase": "attach",
                });
                Ok(vec![(Frame::res(req_id, op::PTY_INPUT, payload), None)])
            }
        }
        other => {
            let payload = json!({
                "error": format!("workspace runtime {other:?} has no pty.input path"),
                "code": "runtime_not_available",
            });
            Ok(vec![(Frame::res(req_id, op::PTY_INPUT, payload), None)])
        }
    }
}

/// ADR 0042 amendment (2026-09-07), decision 2: the CURRENT screen of a
/// named row — no scrollback, no history. Never takes the pen on a capsule
/// row (a WATCHER attach); never touches `tmux.capture_pane` (that op's
/// scrollback-including read stays exactly what it is, for its own
/// pane-id callers).
pub async fn handle_pty_screen(
    req_id: u64,
    payload_json: serde_json::Value,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: PtyScreenReq = serde_json::from_value(payload_json).context("pty.screen payload")?;

    let Some(ws) = workspaces.resolve(Some(&req.workspace_id)) else {
        let payload = json!({
            "error": format!("unknown workspace: {}", req.workspace_id),
            "code": "unknown_workspace",
        });
        return Ok(vec![(Frame::res(req_id, op::PTY_SCREEN, payload), None)]);
    };

    match ws.runtime.as_str() {
        "tmux" => {
            let session = ws.tmux_session.clone();
            let result = tokio::task::spawn_blocking(move || TmuxClient::new().visible_pane(&session))
                .await
                .context("spawn_blocking pty.screen tmux")?;
            match result {
                Ok((cols, rows, cursor_x, cursor_y, lines)) => {
                    let res = PtyScreenRes {
                        runtime: "tmux".into(),
                        cols,
                        rows,
                        lines,
                        cursor: Some(PtyCursor { row: cursor_y, col: cursor_x }),
                    };
                    Ok(vec![(
                        Frame::res(req_id, op::PTY_SCREEN, serde_json::to_value(res)?),
                        None,
                    )])
                }
                Err(e) => Ok(vec![(tmux_error_frame(req_id, op::PTY_SCREEN, e), None)]),
            }
        }
        "capsule" => {
            #[cfg(any(windows, target_os = "linux"))]
            {
                let Some(state_root) = sot_log::state_dir::sot_state_dir() else {
                    let payload = json!({
                        "error": format!(
                            "could not resolve this machine's state root ({} unset)",
                            crate::capsule_workspace::STATE_ROOT_HINT
                        ),
                        "code": "capsule_screen_failed",
                        "phase": "attach",
                    });
                    return Ok(vec![(Frame::res(req_id, op::PTY_SCREEN, payload), None)]);
                };
                let state_dir =
                    crate::capsule_workspace::state_dir_for(&state_root, &ws.workspace_id);
                // A pure watcher never takes, so this id never lands in any
                // input record — it exists only because `FeAttachClient::
                // attach`'s signature takes one; a fixed, self-describing
                // constant is honester than fabricating an identity this
                // read has no caller-supplied handle for.
                let controller_id = "sot-fe-screen".to_string();
                // ADR 0043 decision 33: `resume_if_absent` in place of a
                // bare `phase_of` read — see `pty.input`'s own comment.
                let workspace_id = ws.workspace_id.clone();
                let agent_kind = ws.agent.clone();
                let agent_name = ws.agent_name.clone();
                let slug = ws.slug.clone();
                let project_root = ws.project_root.clone();
                let workspaces_for_resume = workspaces.clone();
                let outcome = tokio::task::spawn_blocking(move || {
                    let phase = match crate::capsule_workspace::resume_if_absent(
                        &state_root,
                        &workspace_id,
                        &agent_kind,
                        &agent_name,
                        &slug,
                        &project_root,
                        workspaces_for_resume,
                    ) {
                        Ok(phase) => phase,
                        Err(e) => {
                            tracing::warn!(workspace_id = %workspace_id, error = %e, "pty.screen: resume_if_absent failed");
                            crate::capsule_workspace::UNREACHABLE_PHASE
                        }
                    };
                    let ready_phase =
                        crate::capsule_workspace::phase_str(sot_log::wire::SupervisorPhase::Ready);
                    if phase != ready_phase {
                        return CapsuleOpOutcome::NotReady(phase);
                    }
                    let deadline = std::time::Instant::now() + CAPSULE_OP_DEADLINE;
                    match crate::capsule_workspace::headless::screen_of(
                        &state_dir,
                        &controller_id,
                        deadline,
                    ) {
                        Ok(shot) => CapsuleOpOutcome::Ok(shot),
                        Err(e) => CapsuleOpOutcome::Headless(e),
                    }
                })
                .await
                .context("spawn_blocking pty.screen capsule")?;
                match outcome {
                    CapsuleOpOutcome::Ok(shot) => {
                        let res = PtyScreenRes {
                            runtime: "capsule".into(),
                            cols: shot.cols,
                            rows: shot.rows,
                            lines: shot.lines,
                            cursor: shot.cursor.map(|(row, col)| PtyCursor { row, col }),
                        };
                        Ok(vec![(
                            Frame::res(req_id, op::PTY_SCREEN, serde_json::to_value(res)?),
                            None,
                        )])
                    }
                    CapsuleOpOutcome::NotReady(phase) => {
                        let payload = json!({
                            "error": format!("capsule row not ready (phase: {phase})"),
                            "code": "capsule_not_ready",
                            "phase": phase,
                        });
                        Ok(vec![(Frame::res(req_id, op::PTY_SCREEN, payload), None)])
                    }
                    CapsuleOpOutcome::Headless(e) => {
                        let payload = headless_error_payload(e, "capsule_screen_failed");
                        Ok(vec![(Frame::res(req_id, op::PTY_SCREEN, payload), None)])
                    }
                }
            }
            #[cfg(not(any(windows, target_os = "linux")))]
            {
                let payload = json!({
                    "error": "capsule runtime not available on this host",
                    "code": "capsule_screen_failed",
                    "phase": "attach",
                });
                Ok(vec![(Frame::res(req_id, op::PTY_SCREEN, payload), None)])
            }
        }
        other => {
            let payload = json!({
                "error": format!("workspace runtime {other:?} has no pty.screen path"),
                "code": "runtime_not_available",
            });
            Ok(vec![(Frame::res(req_id, op::PTY_SCREEN, payload), None)])
        }
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

    // Duplicate-root gate (ADR 0036 Phase 1): one project root, one workspace
    // identity. A second registration for an already-registered root would
    // persist a TOML the daemon then faithfully respawns on every boot, and
    // hands two agent sessions one shared working tree (the collision class
    // worktrees exist to prevent). Compared by canonical path on BOTH sides so
    // symlinked spellings of one directory still collide; refused only for a
    // DIFFERENT slug (same-slug create = the long-standing id-preserving
    // refresh, still allowed). The `existing` block lets the caller offer
    // "switch to that workspace" instead of dead-ending. Canonicalization
    // failure on the candidate skips the gate rather than failing the create —
    // prevention must not make creation less reliable than it is today.
    let incoming_slug = crate::paths::slug(&req.label);
    match project_root.canonicalize() {
        Ok(canon) => {
            if let Some(existing) =
                find_other_workspace_with_root(&canon, &incoming_slug, workspaces)
            {
                let payload = json!({
                    "error": format!(
                        "project_root is already registered as workspace '{}' (slug '{}')",
                        existing.label, existing.slug
                    ),
                    "code": "duplicate_root",
                    "existing": {
                        "workspace_id": existing.workspace_id,
                        "slug": existing.slug,
                        "label": existing.label,
                    },
                });
                return Ok(vec![(
                    Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                    None,
                )]);
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, project_root = %req.project_root,
                "duplicate-root gate skipped — candidate did not canonicalize");
        }
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

    // ADR 0043 decision 22: the runtime is now an explicit VALUE, not a
    // platform cfg — `""` (absent on the wire) means this host's own
    // platform default, `"capsule"` asks for one explicitly on either
    // platform, `"tmux"` is refused on Windows (the no-knob rule: no
    // tmux runtime exists there at all). The Linux platform default
    // stays "tmux" (a capsule row's attach is same-machine only until
    // the bridge — see `ops.rs`'s own doc on this field).
    let runtime: String = match req.runtime.as_str() {
        "" => if cfg!(windows) { "capsule" } else { "tmux" }.to_string(),
        "capsule" => "capsule".to_string(),
        "tmux" if !cfg!(windows) => "tmux".to_string(),
        "tmux" => {
            let payload = json!({
                "error": "no tmux runtime on Windows".to_string(),
                "code": "runtime_not_available",
            });
            return Ok(vec![(
                Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                None,
            )]);
        }
        other => {
            let payload = json!({
                "error": format!("unknown runtime {other:?} (want \"capsule\", \"tmux\", or \"\" for this host's default)"),
                "code": "bad_runtime",
            });
            return Ok(vec![(
                Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                None,
            )]);
        }
    };
    // ADR 0042 slice L1a, Codex review finding 9: validated BEFORE any
    // state mutation, whenever the resolved runtime is "capsule" (every
    // NEW workspace on Windows, or an explicitly requested one anywhere
    // `capsule_workspace::runtime` compiles — ADR 0043 decision 22). The
    // tmux path below accepts every agent kind unchanged. `agent_argv` is
    // the same function the spawn itself uses, so this is the real
    // check, not a second guess at it — "codex" (no known launcher on
    // either platform) is refused here rather than silently launching a
    // bare shell nobody asked for.
    let capsule_argv: Vec<String> = if runtime == "capsule" {
        match crate::capsule_workspace::agent_argv(&agent_kind) {
            Ok(argv) => argv,
            Err(detail) => {
                let payload = json!({
                    "error": detail,
                    "code": "unsupported_agent_on_this_host",
                });
                return Ok(vec![(
                    Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                    None,
                )]);
            }
        }
    } else {
        Vec::new()
    };
    // ADR 0043 decision 23: refuse an unqualified state root at the SAME
    // "before any state mutation" moment `capsule_argv` above already
    // established — before `ws_seed`, before `workspaces.insert`, before
    // any toml. Gated identically to the capsule runtime's own
    // availability check further down (`#[cfg(any(windows, target_os =
    // "linux"))]`): a platform with no capsule runtime AT ALL (macOS)
    // keeps its existing "runtime not available" refusal below instead of
    // a state-root diagnosis that would be beside the point there.
    #[cfg(any(windows, target_os = "linux"))]
    let capsule_state_root: Option<std::path::PathBuf> = if runtime == "capsule" {
        match crate::capsule_workspace::qualified_state_root() {
            Ok(root) => Some(root),
            Err(detail) => {
                let payload = json!({
                    "error": detail,
                    "code": "state_root_unqualified",
                });
                return Ok(vec![(
                    Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                    None,
                )]);
            }
        }
    } else {
        None
    };
    #[cfg(not(any(windows, target_os = "linux")))]
    let capsule_state_root: Option<std::path::PathBuf> = None;
    let mut ws_seed = crate::workspaces::Workspace::from_label(
        &req.label,
        project_root.clone(),
        autostart,
        agent_kind.clone(),
        req.agent_name.clone(),
        req.task.clone(),
    );
    ws_seed.runtime = runtime;
    let ws_handle = workspaces.insert(ws_seed);
    if let Err(e) = crate::workspaces::save(&ws_handle) {
        tracing::warn!(error = %e, "workspace toml persist failed; workspace is in-memory only");
    }

    // ADR 0043 decision 22: branch on the resolved runtime VALUE, not a
    // platform cfg — `ws_handle.runtime` is exhaustive over the two
    // runtimes this daemon can ever create (see `Workspace::runtime`'s
    // own doc); both arms compile on every platform this daemon builds
    // for (on Windows the tmux arm is simply unreachable — "tmux" is
    // refused above before either arm is ever entered).
    if ws_handle.runtime == "capsule" {
        // ADR 0043 decision 22: the capsule runtime itself only compiles
        // on Windows and Linux (`capsule_workspace::runtime`'s own
        // gate) — on any other host (macOS stays experimental) an
        // explicit `"runtime":"capsule"` request is refused gracefully,
        // the same "not available" shape `destroy_capsule_workspace`
        // reports for a row that somehow already has one.
        #[cfg(any(windows, target_os = "linux"))]
        {
        // ADR 0042 slice L1a, Codex review finding 1: the capsule spawn —
        // and, unlike the tmux path below, a SYNCHRONOUS failure here
        // FAILS the whole op: "a capsule workspace with no supervisor is
        // not a workspace." Rule C (shrink round): this daemon no longer
        // creates the state directory itself — `sot-capsule supervise`
        // creates its OWN, as its first act after it actually runs — so
        // a synchronous failure below leaves nothing on disk at all, not
        // even an empty directory. The DETACHED spawn-and-watch is what
        // survives this daemon's own exit, with its own exit handled
        // going forward (finding 6). On ANY failure to reach a running
        // supervisor, roll back the registry row and its persisted toml
        // and refuse the op with the real error text.
        // ADR 0043 decision 23: `capsule_state_root` was already resolved
        // and qualified ABOVE, before this row (or its toml) ever existed
        // — reuse it rather than re-resolving a second time. Always
        // `Some` here in practice (this arm only runs when
        // `ws_handle.runtime == "capsule"`, which is exactly when the
        // earlier check ran and would have already returned on failure);
        // the `None` arm stays as a defensive fallback, never actually hit.
        // ADR 0043 decision 29: a process spawn never runs on a Tokio
        // worker.
        //
        // ADR 0043 decision 33 (Codex review, 2026-09-11): this row's own
        // guard, taken HERE — inside the capsule arm only, never for a
        // tmux row (nothing else ever contends a tmux id's guard) — and
        // held across the spawn attempt below, closing the exact race
        // `pty.open`'s own `ensure_started` could otherwise win against
        // this handler's still-in-flight spawn (the field latency map's
        // own ordering: `ensure_started` can reach this SAME
        // freshly-minted workspace_id within milliseconds of the row
        // becoming visible via `insert` above). Every other lifecycle
        // mutation of a capsule row takes the SAME guard (`ensure_started`,
        // `resume_if_absent`, the watchdog's own restart, `resume_all`) —
        // this is that discipline's create-time entry. `capsule_guard`
        // itself already refuses to mint a guard for an absent row; the
        // membership recheck right after (under the lock, not before it)
        // catches one that vanished WHILE this waited for it — deciding
        // under the guard rather than starting unconditionally, the same
        // discipline every other guarded mutation follows.
        let capsule_guard = workspaces.capsule_guard(&ws_handle.workspace_id);
        let _capsule_guard_held = match &capsule_guard {
            Some(g) => Some(g.lock().await),
            None => None,
        };
        let still_registered = capsule_guard.is_some()
            && workspaces.list().iter().any(|ws| ws.workspace_id == ws_handle.workspace_id);
        let spawn_result: std::result::Result<(), String> = if !still_registered {
            Err("workspace was removed before its capsule supervisor could be started".to_string())
        } else {
            match capsule_state_root {
            None => Err(format!(
                "could not resolve this machine's state root ({} unset)",
                crate::capsule_workspace::STATE_ROOT_HINT
            )),
            // `&req.agent_name` verbatim (Codex round finding 2: no
            // synthesized default — a synthesized `<slug>-<host>` handed
            // to SOT_COMM_NAME would become an explicit pin that
            // overwrites any existing registry row of that name,
            // violating PROTOCOL.md's "never reuse a handle"; an empty
            // `agent_name` is a real, supported case now — comm-join.sh's
            // own #148 auto-disambiguating derivation picks the handle,
            // via the SOT_COMM_SELF_FILE this spawn pins).
            Some(state_root) => {
                let workspace_id = ws_handle.workspace_id.clone();
                let capsule_argv = capsule_argv.clone();
                let project_root = project_root.clone();
                let agent_name = req.agent_name.clone();
                let slug = ws_handle.slug.clone();
                let workspaces_for_spawn = workspaces.clone();
                // BLOCKING (process spawn, superseded by ADR 0045: no
                // pre-spawn probe runs here anymore): the row guard is
                // held by the CALLING async fn's own frame for this whole
                // `.await`, not by this closure — a panic in here is
                // caught by `spawn_blocking` itself and never unwinds
                // past that guard, so there is nothing to release on the
                // error path below beyond reporting it.
                tokio::task::spawn_blocking(move || {
                    crate::capsule_workspace::start_supervisor(
                        &state_root,
                        &workspace_id,
                        crate::capsule_workspace::StartMode::Start,
                        &capsule_argv,
                        &project_root,
                        &agent_name,
                        &slug,
                        workspaces_for_spawn,
                    )
                })
                .await
                .unwrap_or_else(|join_err| Err(format!("capsule spawn task panicked: {join_err}")))
                .map(|_phase| ())
            }
            }
        };
        match spawn_result {
            Ok(()) => {
                tracing::info!(workspace_id = %ws_handle.workspace_id, "workspace.create: capsule supervisor spawned");
            }
            Err(detail) => {
                tracing::warn!(workspace_id = %ws_handle.workspace_id, error = %detail, "workspace.create: capsule spawn failed; rolling back");
                let _ = workspaces.remove_by_id(&ws_handle.workspace_id);
                for toml_path in [
                    crate::workspaces::toml_path_for(&ws_handle.slug),
                    crate::workspaces::legacy_toml_path_for(&ws_handle.slug),
                ] {
                    match std::fs::remove_file(&toml_path) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => tracing::warn!(error = %e, path = ?toml_path, "workspace.create rollback: toml remove failed"),
                    }
                }
                let payload = json!({
                    "error": format!("capsule workspace could not be started: {detail}"),
                    "code": "capsule_spawn_failed",
                });
                return Ok(vec![(
                    Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                    None,
                )]);
            }
        }
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = &capsule_argv;
            let _ = &capsule_state_root;
            tracing::warn!(workspace_id = %ws_handle.workspace_id, "workspace.create: capsule runtime requested but not available on this host; rolling back");
            let _ = workspaces.remove_by_id(&ws_handle.workspace_id);
            for toml_path in [
                crate::workspaces::toml_path_for(&ws_handle.slug),
                crate::workspaces::legacy_toml_path_for(&ws_handle.slug),
            ] {
                match std::fs::remove_file(&toml_path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => tracing::warn!(error = %e, path = ?toml_path, "workspace.create rollback: toml remove failed"),
                }
            }
            let payload = json!({
                "error": "the capsule runtime is not available on this host",
                "code": "runtime_not_available",
            });
            return Ok(vec![(
                Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                None,
            )]);
        }
    } else {
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

/// ADR 0042 slice L1a (Codex review finding 3): whether a capsule
/// workspace's row (and its persisted toml) may be safely removed by
/// `workspace.destroy`.
enum CapsuleDestroyOutcome {
    /// The run was CONFIRMED ended (`RecordVerified`/`RecordClosed`/
    /// `AlreadyEnded` — see `capsule_workspace::EndRunOutcome`) — the row
    /// may be removed; the state directory never is. Human-readable
    /// (never the raw, Windows-only `EndRunOutcome` type) so this enum
    /// stays portable and unit-testable.
    #[cfg_attr(not(windows), allow(dead_code))]
    Removable(String),
    /// Not confirmed (unreachable/starting/failed/refused/unknown) — the
    /// row and toml MUST be kept: never orphan a live run, never claim
    /// "ended" for one that wasn't.
    Kept { detail: String },
}

/// Maps a `capsule_workspace::EndRunOutcome` to whether `workspace.destroy`
/// may remove the row. Pure/portable so it's unit-testable without a real
/// Windows lane; `#[cfg(test)]` below is its only caller off Windows.
#[cfg_attr(not(windows), allow(dead_code))]
fn capsule_destroy_outcome_of(o: crate::capsule_workspace::EndRunOutcome) -> CapsuleDestroyOutcome {
    use crate::capsule_workspace::EndRunOutcome as O;
    match o {
        O::RecordVerified => CapsuleDestroyOutcome::Removable("run ended and verified".to_string()),
        O::RecordClosed => CapsuleDestroyOutcome::Removable(
            "run ended (record closed, not yet verified)".to_string(),
        ),
        O::AlreadyEnded => CapsuleDestroyOutcome::Removable("run had already ended".to_string()),
        // A `Terminal` authority has no leg left to orphan -- `end_run`
        // already sent it `stop` and waited for confirmed exit (see
        // `EndRunOutcome::Terminal`'s own doc). Without this arm a
        // capsule row whose agent argv can never launch was UNENDABLE:
        // `end_run` used to report this as `NotEnded` (kept) forever.
        O::Terminal => CapsuleDestroyOutcome::Removable(
            "the run was terminal; the supervisor was stopped".to_string(),
        ),
        // A `Starting` lane is NOT "not running" -- retryable.
        O::Starting => CapsuleDestroyOutcome::Kept {
            detail: "supervisor is starting; retry".to_string(),
        },
        O::NotEnded(detail) => CapsuleDestroyOutcome::Kept { detail },
        // The lane was unreachable but the supervisor lock itself was
        // free to take -- nobody holds this row (see `EndRunOutcome::
        // Unheld`'s own doc). A run with no holder is not running.
        O::Unheld => {
            CapsuleDestroyOutcome::Removable("no supervisor held the row".to_string())
        }
    }
}

/// After a default row's run is CONFIRMED ended (`confirmed_ended` from
/// `default_row_end_response`), reset the row's `agent`/`agent_name` back
/// to the inert-anchor shape and persist + broadcast the change — the
/// ADR 0042 amendment invariant ("an anchor with no run is inert, and
/// inert anchors are hidden") applied to the one path that used to leave
/// a carried-over `agent` stuck forever (field defect, v0.6.0-rc.12: the
/// owner once started an agent in this row before that rule existed, and
/// nothing ever reset `agent` back to "none" once its run ended, so
/// `Workspaces::is_inert_default_anchor` never went true again). A
/// `false` confirmed_ended is a no-op: `default_row_end_response` already
/// built the typed-error response for a `Kept` outcome, and neither the
/// row nor its toml may change under a refusal.
///
/// ADR 0043 decision 35: also prunes the row's sot-comm registry entries,
/// the same way `workspace.destroy`'s non-default path does below — a
/// killed default-row agent can't run its own `comm-leave`, so without
/// this its row lingered as a ghost `workspace.list` merges back in.
///
/// `held_guard` is `destroy_capsule_workspace`'s own row guard, carried
/// through unexamined so it stays locked across the reset below too
/// (ADR 0043 decision 33, Codex review round 2) — dropped only once this
/// function returns, whichever arm it takes.
async fn end_default_row_run(
    workspaces: &Workspaces,
    ws_events: &broadcast::Sender<WorkspaceChanged>,
    workspace_id: &str,
    slug: &str,
    agent_name: &str,
    tmux_session: &str,
    confirmed_ended: bool,
    _held_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
) {
    if !confirmed_ended {
        return;
    }
    let reg_agent = agent_name.to_string();
    let reg_session = tmux_session.to_string();
    let reg_host = crate::workspaces::state_host();
    let comm_removed = tokio::task::spawn_blocking(move || {
        remove_comm_agents_for_workspace(&reg_session, &reg_agent, &reg_host)
    })
    .await
    .unwrap_or_default();
    if !comm_removed.is_empty() {
        tracing::info!(
            removed = ?comm_removed,
            slug = %slug,
            "pruned sot-comm registry rows for the default row's ended run"
        );
    }
    if let Some(reset) = workspaces.reset_agent_to_none(workspace_id) {
        if let Err(e) = crate::workspaces::save(&reset) {
            tracing::warn!(error = %e, workspace_id = %workspace_id,
                "default row agent-reset toml persist failed; workspace is in-memory only");
        }
    }
    // Live-push so the Sessions strip re-lists — the row's phase is
    // derived fresh from the supervisor lane on every `workspace.list`
    // call. `action` is informational only: every `workspace.changed`
    // push just triggers an FE re-list.
    let _ = ws_events.send(WorkspaceChanged {
        action: "run_ended".into(),
        slug: slug.to_string(),
        workspace_id: workspace_id.to_string(),
    });
}

/// `reason` is the immutable end-run reason recorded on the wire —
/// parameterized so each caller (a real delete vs. the default row's
/// own kept-not-deleted branch) supplies its own honest text.
/// `agent_kind`/`agent_name`/`slug`/`project_root` are `ws`'s own fields,
/// passed through (rather than re-resolved) so this can call
/// `capsule_workspace::resume_locked` — the guard-free inner
/// `resume_if_absent` itself uses — under the SAME row guard `end_run`
/// then runs under (ADR 0043 decision 33's own resume-before-end
/// destroy caller): a row whose supervisor died leaves a live LEG behind
/// with no authority to end it; resuming re-establishes the authority so
/// `end_run` has a real lane to ask, rather than falling straight to its
/// own fence/leg proof. A resume failure is logged and never fails the
/// call — `end_run`'s own arms decide the outcome regardless.
///
/// Every mutation runs under the row's own guard, from the first probe
/// through the outcome this returns (ADR 0043 decision 33) — a row the
/// watchdog already marked `workspaces.is_capsule_terminal` takes the
/// SAME guarded path as every other row: `resume_locked`'s own internal
/// check still reports that phase without a live round trip (no wasted
/// probe against an authority that is almost always already gone — see
/// its own doc), but `end_run`'s fresh `query_status` then independently
/// proves the row's fence AND leg both absent before this reports
/// `Removable` (BLOCKER, Codex review, 2026-09-11: an earlier revision
/// short-circuited straight to `Removable` on `is_capsule_terminal`
/// alone, bypassing the guard and this proof entirely — `is_capsule_
/// terminal` records that the watchdog's OWN `child.wait()` confirmed
/// the AUTHORITY exited, never that a leg the watchdog's restart budget
/// left running, or a failed adoption, is also gone).
///
/// Returns the row's own guard alongside the outcome, still HELD
/// (`None` only when no real lane call was ever attempted) — Codex
/// review round 2 on the L1a PR: an owned watchdog can check membership,
/// enter its own backoff, and restart the very row a caller is mid-way
/// through removing, unless the SAME guard covers both the end/stop
/// call here AND whatever the caller does with a confirmed outcome
/// (row removal, or the default row's own reset) afterward. The caller
/// holds it through that follow-up, then drops it.
async fn destroy_capsule_workspace(
    workspace_id: &str,
    reason: &str,
    agent_kind: &str,
    agent_name: &str,
    slug: &str,
    project_root: &std::path::Path,
    workspaces: &Workspaces,
) -> (CapsuleDestroyOutcome, Option<tokio::sync::OwnedMutexGuard<()>>) {
    #[cfg(any(windows, target_os = "linux"))]
    {
        let Some(state_root) = sot_log::state_dir::sot_state_dir() else {
            return (
                CapsuleDestroyOutcome::Kept {
                    detail: format!(
                        "could not resolve this machine's state root ({} unset)",
                        crate::capsule_workspace::STATE_ROOT_HINT
                    ),
                },
                None,
            );
        };
        let state_dir = crate::capsule_workspace::state_dir_for(&state_root, workspace_id);
        let reason = reason.to_string();
        let workspace_id = workspace_id.to_string();
        let agent_kind = agent_kind.to_string();
        let agent_name = agent_name.to_string();
        let slug = slug.to_string();
        let project_root = project_root.to_path_buf();
        let workspaces_for_guard = workspaces.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            // ADR 0043 decision 33: this row's own guard, taken OWNED so
            // it survives this closure's return and stays held by the
            // caller through the row's actual removal/reset — see this
            // function's own doc. `None` (Codex review, 2026-09-11:
            // `capsule_guard` itself now refuses to mint one for a row
            // that is not currently registered) means a concurrent
            // remover already won this race — nothing left here to end.
            let Some(guard) = workspaces_for_guard.capsule_guard(&workspace_id) else {
                return (
                    Err(std::io::Error::new(std::io::ErrorKind::NotFound, "unknown workspace")),
                    None,
                );
            };
            let held = guard.blocking_lock_owned();
            match crate::capsule_workspace::resume_locked(
                &state_root,
                &workspace_id,
                &agent_kind,
                &agent_name,
                &slug,
                &project_root,
                workspaces_for_guard.clone(),
            ) {
                // BLOCKER (Codex review, 2026-09-11): a pending resume can
                // outlive deletion. `resume_locked` returns this exact
                // sentinel phase ONLY when it just spawned a fresh
                // authority (its own probe first read `UNREACHABLE_PHASE`)
                // and `start_supervisor`'s settle deadline elapsed with
                // the lane STILL unobserved — an unresolved spawn is still
                // in flight under THIS SAME guard. Falling through to
                // `end_run` regardless (the old behaviour) would race it:
                // the freshly spawned process has not yet taken the fence
                // or re-executed the leg, so `end_run`'s own absence proof
                // could read both as acquirable and report the row
                // Removable an instant before that supervisor starts.
                // There is no cheap way to cancel or reap it from here —
                // the spawned `Child` is already owned by its own
                // watchdog, installed inside `resume_locked`'s own call,
                // never handed back to this caller — so a timeout stays
                // non-removable: `Kept` with an honest code
                // (`supervisor_starting`), never a guess.
                Ok(phase) if phase == crate::capsule_workspace::UNREACHABLE_PHASE => {
                    return (
                        Err(std::io::Error::new(std::io::ErrorKind::WouldBlock, "supervisor_starting")),
                        Some(held),
                    );
                }
                Ok(_) => {}
                // SHOULD-FIX (Codex review, 2026-09-11): a destroy that
                // waited behind another remover's SAME guard must not
                // continue into `end_run` once THIS recheck (run only
                // after the guard was actually acquired) finds the row
                // already gone — the old state dir's fence and leg really
                // are free once nothing owns it any more, so `end_run`'s
                // own proof would still succeed and report `Removable`,
                // and the caller would then delete a SLUG-keyed toml that
                // may since belong to a REPLACEMENT registration under
                // the same slug. `held` is dropped (not carried) so this
                // lands on the SAME "row already gone" `NotFound` arm
                // below the top-of-function race already uses.
                Err(e) if e == "unknown workspace" => {
                    return (Err(std::io::Error::new(std::io::ErrorKind::NotFound, e)), None);
                }
                Err(e) => {
                    tracing::warn!(
                        workspace_id = %workspace_id, error = %e,
                        "workspace.destroy: resume before end_run failed; end_run's own arms decide"
                    );
                }
            }
            let result = crate::capsule_workspace::end_run(&state_dir, &reason);
            (result, Some(held))
        })
        .await;
        match outcome {
            Ok((Ok(o), held)) => (capsule_destroy_outcome_of(o), held),
            // `end_run`'s own `state_dir_missing` (ADR 0043 decision 33's
            // destroy proof: a missing state dir proves nothing and is
            // reported, never recreated) gets its own typed code rather
            // than folding into the generic "lane unreachable" detail —
            // `capsule_end_not_reached_payload` reads it back off this
            // exact sentinel string. A `None` guard here is the "row
            // already gone" race above, reusing the SAME NotFound kind —
            // never mistaken for a missing state dir.
            Ok((Err(e), held)) if held.is_none() && e.kind() == std::io::ErrorKind::NotFound => (
                CapsuleDestroyOutcome::Kept {
                    detail: "workspace was removed before its capsule run could be ended".to_string(),
                },
                None,
            ),
            Ok((Err(e), held)) if e.kind() == std::io::ErrorKind::NotFound => {
                (CapsuleDestroyOutcome::Kept { detail: "state_dir_missing".to_string() }, held)
            }
            // The pending-resume sentinel above — a timeout stays
            // non-removable with its own honest code, never folded into
            // the generic "supervisor lane unreachable" catch-all below.
            Ok((Err(e), held)) if e.kind() == std::io::ErrorKind::WouldBlock => (
                CapsuleDestroyOutcome::Kept { detail: "supervisor_starting".to_string() },
                held,
            ),
            Ok((Err(e), held)) => (
                CapsuleDestroyOutcome::Kept {
                    detail: format!("supervisor lane unreachable: {e}"),
                },
                held,
            ),
            Err(join_err) => (
                CapsuleDestroyOutcome::Kept {
                    detail: format!("end_run task panicked: {join_err}"),
                },
                None,
            ),
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (workspace_id, reason, agent_kind, agent_name, slug, project_root, workspaces);
        // Unreachable in practice: no workspace has `runtime == "capsule"`
        // off Windows/Linux (see `Workspace::runtime`'s own doc) — a host
        // this crate compiles for but the capsule runtime does not
        // (ADR 0043: macOS stays experimental).
        (
            CapsuleDestroyOutcome::Kept {
                detail: "the capsule runtime is not available on this host".to_string(),
            },
            None,
        )
    }
}

/// The typed error `workspace.destroy` returns for a `Kept` outcome —
/// shared by the non-default path and the default row's own branch.
/// `"state_dir_missing"` and `"supervisor_starting"` are
/// `destroy_capsule_workspace`'s own sentinel details (ADR 0043 decision
/// 33) — the two `Kept` reasons with a code more specific than the
/// generic catch-all, so a caller can tell "nothing durable was ever
/// established here" and "a resume is still in flight, retry" apart from
/// every other kept reason without parsing prose.
fn capsule_end_not_reached_payload(detail: &str) -> serde_json::Value {
    let code = match detail {
        "state_dir_missing" => "state_dir_missing",
        "supervisor_starting" => "supervisor_starting",
        _ => "capsule_end_not_reached",
    };
    json!({
        "error": format!("capsule workspace could not be safely deleted: {detail}"),
        "code": code,
    })
}

/// The default row's own `workspace.destroy` response, built from an
/// already-computed outcome (pure/portable, unit-testable without a real
/// lane). Returns the payload and whether to broadcast `run_ended` —
/// `true` only for a CONFIRMED end; `Kept` gets the typed error instead.
fn default_row_end_response(
    workspace_id: &str,
    slug: &str,
    label: &str,
    outcome: CapsuleDestroyOutcome,
) -> (serde_json::Value, bool) {
    match outcome {
        CapsuleDestroyOutcome::Removable(detail) => {
            let res = sot_protocol::WorkspaceDestroyRes {
                workspace_id: workspace_id.to_string(),
                slug: slug.to_string(),
                label: label.to_string(),
                tmux_killed: false,
                toml_removed: false,
                kept: Some(format!("ended run of '{label}' ({detail})")),
            };
            (
                serde_json::to_value(res).expect("WorkspaceDestroyRes always serializes"),
                true,
            )
        }
        CapsuleDestroyOutcome::Kept { detail } => (capsule_end_not_reached_payload(&detail), false),
    }
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

    // The default workspace's ROW is never destroyed here — it's the
    // daemon's anchor, with no fallback target to swap ops to. A default
    // TMUX row has no run to end, so it keeps the flat refusal. A
    // default CAPSULE row (ADR 0042: the default `local` row on a
    // Windows FE box, ADR 0043 decision 22: on Linux only ever reached
    // via a hand-edited toml, since the Linux platform default stays
    // "tmux" until the bridge) instead ends its run and keeps the row,
    // reusing the non-default delete's own path below — a `Kept`
    // (unconfirmed) outcome still returns the SAME typed error, never a
    // fabricated success.
    if workspaces.default_id().as_deref() == Some(ws.workspace_id.as_str()) {
        // Gate on the toml's own `runtime` string alone now (ADR 0043
        // decision 22): capsule support is no longer Windows-only, so a
        // Linux default row that genuinely carries `runtime = "capsule"`
        // gets the same real end-run path a Windows one does. Every
        // OTHER default row (the ordinary "tmux" case on every host)
        // keeps the same flat refusal it always had.
        if ws.runtime != "capsule" {
            // Otherwise invisible in the daemon log — a refused destroy on
            // a dead-end default row (e.g. one stuck with a runtime the
            // daemon also refuses to start) previously left no trace at
            // all to diagnose from.
            tracing::info!(
                workspace_id = %ws.workspace_id,
                slug = %ws.slug,
                code = "default_workspace_not_destroyable",
                "workspace.destroy refused: default workspace cannot be destroyed"
            );
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

        // Same end-run path the non-default delete uses below. The
        // reason is honest for THIS row (not "deleted" — it's kept).
        let (outcome, held_guard) = destroy_capsule_workspace(
            &ws.workspace_id,
            "run ended by the user",
            &ws.agent,
            &ws.agent_name,
            &ws.slug,
            &ws.project_root,
            workspaces,
        )
        .await;
        let (payload, confirmed_ended) =
            default_row_end_response(&ws.workspace_id, &ws.slug, &ws.label, outcome);
        tracing::info!(workspace_id = %ws.workspace_id, confirmed_ended, "workspace.destroy: default row's capsule run outcome; row kept");

        // The row guard (if any) rides along into the reset below and
        // drops only once that returns — see `end_default_row_run`'s own
        // doc.
        end_default_row_run(
            workspaces,
            ws_events,
            &ws.workspace_id,
            &ws.slug,
            &ws.agent_name,
            &ws.tmux_session,
            confirmed_ended,
            held_guard,
        )
        .await;

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

    // This row's guard, if `destroy_capsule_workspace` took one — HELD
    // (ADR 0043 decision 33, Codex review round 2) across the removal
    // below, past the `if`, so a watchdog can never restart the row
    // between a confirmed end and `remove_by_id`. Dropped explicitly
    // once removal is done; stays `None` for a tmux row (no capsule
    // guard applies) or a `Kept` outcome (nothing is removed).
    let mut destroy_guard: Option<tokio::sync::OwnedMutexGuard<()>> = None;

    // ADR 0042 slice L1a, Codex review finding 3: a capsule workspace has
    // no tmux session to kill at all — end its run over the supervisor
    // lane instead, and — unlike the tmux kill, which is a best-effort UX
    // nicety — a capsule whose run did NOT reach `record_closed`/
    // `record_verified` STOPS the whole delete here: the row and its
    // toml are kept, and the caller sees a typed error, so a live or
    // unreachable run is never orphaned by a delete that silently
    // "succeeded" out from under it.
    let tmux_killed = if ws.runtime == "capsule" {
        let reason = format!("workspace '{slug}' deleted");
        let (outcome, held) = destroy_capsule_workspace(
            &workspace_id,
            &reason,
            &ws.agent,
            &agent_name,
            &slug,
            &ws.project_root,
            workspaces,
        )
        .await;
        match outcome {
            CapsuleDestroyOutcome::Removable(outcome) => {
                tracing::info!(workspace_id = %workspace_id, %outcome, "workspace.destroy: capsule run ended; removing the row");
                destroy_guard = held;
                false // no tmux session ever existed to kill -- accurate, not a failure
            }
            CapsuleDestroyOutcome::Kept { detail } => {
                tracing::warn!(workspace_id = %workspace_id, detail = %detail, "workspace.destroy: capsule run not confirmed ended; keeping the row");
                return Ok(vec![(
                    Frame::res(
                        req_id,
                        op::WORKSPACE_DESTROY,
                        capsule_end_not_reached_payload(&detail),
                    ),
                    None,
                )]);
            }
        }
    } else {
        // Kill the tmux session. Failure is non-fatal — usually means the
        // session wasn't running anyway. We surface the bool so the
        // frontend can decide whether to surface the discrepancy.
        let tmux_target = tmux_session.clone();
        tokio::task::spawn_blocking(move || {
            crate::tmux::TmuxClient::new()
                .kill_session(&tmux_target)
                .is_ok()
        })
        .await
        .unwrap_or(false)
    };

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
    let reg_host = crate::workspaces::state_host();
    let comm_removed = tokio::task::spawn_blocking(move || {
        remove_comm_agents_for_workspace(&reg_session, &reg_agent, &reg_host)
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
    // Only now may this row's guard (if any) release — see its own doc
    // above: held from `destroy_capsule_workspace`'s end/stop call
    // through this exact removal, so a watchdog waiting on the same
    // guard can never restart a row that is already gone.
    drop(destroy_guard);

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
        kept: None,
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
/// ack. Structurally mirrors `handle_agent_send`.
///
/// 2026-09-08 review rework — an untargeted request (`target: None`) is
/// resolved HERE, before publish, from ONE `Clients::snapshot_with_active()`
/// call:
/// - An active frontend exists: deliver to it EXCLUSIVELY, by connection
///   serial (design point B — a bare handle string is not a reliable
///   identity; `evt.target_serial` carries the serial for `server.rs`'s
///   per-connection fan-out to filter on, while `evt.target` still carries
///   the handle for the FE's own — now redundant but harmless —
///   `route_fe_command` self-check).
/// - No active frontend, and `cmd == "relaunch"`: publish NOTHING (design
///   point E — an unresolved relaunch broadcast is a command nobody can
///   safely execute; every FE refuses an undirected one anyway, so the
///   daemon not sending it is strictly more honest, not less capable).
/// - No active frontend, any other `cmd`: fall through to the pre-existing
///   broadcast (`target` stays `None`, `target_serial` stays `None` — every
///   connection's `route_fe_command` self-filter sees the badge floor,
///   unchanged from before this design).
///
/// An explicit `target` from the caller is never touched, and delivery for
/// it stays a handle-matched broadcast exactly as before (`target_serial`
/// stays `None`, so every matching connection self-filters as today).
///
/// `resolved_target` on the ack (design point E) always mirrors the final
/// `target` — `None` when nothing was resolved (whether or not something
/// broadcast), so a caller like `sot-fe relaunch` can tell "no active
/// frontend" apart from "delivered/broadcast" without inspecting `cmd`
/// itself.
///
/// `delivered_to` (2026-09-09 field incident: a broadcast `open-url` acked
/// `ok:true` twice while landing on a machine other than the one the owner
/// was sitting at) is the count of ATTACHED FRONTENDS this command was
/// actually published to, read off the SAME `snapshot_with_active()` call
/// `resolved_target` was resolved from — never a second lock acquisition
/// (`handle_version_query` sets the precedent). It counts what will
/// ACTUALLY act, which is not always a handle count: a target resolved to
/// the active frontend is delivered by SERIAL, so it is exactly 1 even when
/// a second connection shares that handle; an explicit `--fe <handle>` stays
/// a handle-matched broadcast, so its audience IS the handle count (0 when
/// nothing carries it — the incident above); `target: None` counts every row
/// with a non-empty `fe_handle` (the badge-floor broadcast's audience); the
/// unresolved-relaunch early return below is `Some(0)` — it published
/// nothing. This never changes WHAT gets published, only what the ack
/// truthfully reports about it.
pub async fn handle_fe_command_send(
    req_id: u64,
    payload_json: serde_json::Value,
    fe_tx: &broadcast::Sender<FeCommandEvt>,
    clients: &crate::clients::Clients,
) -> Result<HandlerOutput> {
    let mut req: FeCommandSendReq =
        serde_json::from_value(payload_json).context("fe.command.send payload")?;
    let mut target_serial: Option<u64> = None;
    let snap = clients.snapshot_with_active();
    if req.target.is_none() {
        if let Some(active) = snap.active() {
            req.target = Some(active.handle.clone());
            target_serial = Some(active.serial);
        }
    }
    let resolved_target = req.target.clone();

    if req.cmd == "relaunch" && req.target.is_none() {
        tracing::info!(
            cmd = %req.cmd,
            delivered_to = 0,
            "fe.command.send relay: relaunch has no active frontend and no explicit target — not publishing"
        );
        return Ok(vec![(
            Frame::res(
                req_id,
                op::FE_COMMAND_SEND,
                serde_json::to_value(FeCommandSendRes {
                    ok: true,
                    resolved_target,
                    delivered_to: Some(0),
                })?,
            ),
            None,
        )]);
    }

    let delivered_to = match (target_serial, resolved_target.as_deref()) {
        // Resolved to the ACTIVE frontend: `server.rs` fans out on the
        // SERIAL, so exactly that one CONNECTION acts — however many
        // connections happen to share its handle (a relaunched frontend
        // whose predecessor's connection has not been reaped yet is the
        // real case). Counting handles here would over-report the audience
        // of an exclusive delivery: the same lie in miniature that this
        // field exists to end.
        (Some(_), _) => 1,
        // An explicit `--fe <handle>` stays a handle-matched broadcast —
        // every connection carrying that handle self-filters as a match,
        // so the handle count IS the audience.
        (None, Some(handle)) => snap
            .clients
            .iter()
            .filter(|c| c.fe_handle.as_deref() == Some(handle))
            .count(),
        // The badge floor: every attached, handle-bearing frontend acts.
        (None, None) => snap
            .clients
            .iter()
            .filter(|c| c.fe_handle.as_deref().is_some_and(|h| !h.is_empty()))
            .count(),
    };

    tracing::info!(cmd = %req.cmd, target = ?req.target, delivered_to, "fe.command.send relay");
    let evt = FeCommandEvt {
        v: 1,
        cmd: req.cmd,
        args: req.args,
        target: req.target,
        target_serial,
    };
    // Fire-and-forget broadcast; send error means no subscribers, harmless.
    let _ = fe_tx.send(evt);
    Ok(vec![(
        Frame::res(
            req_id,
            op::FE_COMMAND_SEND,
            serde_json::to_value(FeCommandSendRes {
                ok: true,
                resolved_target,
                delivered_to: Some(delivered_to),
            })?,
        ),
        None,
    )])
}

/// `fe.presence` (2026-09-08 review rework, design point A): a person
/// provided real input; ack only. Stamping happens in `server.rs`'s
/// dispatch (it needs this connection's registered serial, which isn't
/// visible from an op payload alone).
pub async fn handle_fe_presence(req_id: u64) -> Result<HandlerOutput> {
    Ok(vec![(
        Frame::res(
            req_id,
            op::FE_PRESENCE,
            serde_json::to_value(sot_protocol::FePresenceRes { ok: true })?,
        ),
        None,
    )])
}

#[cfg(test)]
mod fe_command_send_tests {
    use super::handle_fe_command_send;
    use crate::clients::Clients;
    use sot_protocol::FeCommandEvt;
    use tokio::sync::broadcast;

    fn req_json(cmd: &str, target: Option<&str>) -> serde_json::Value {
        serde_json::json!({
            "cmd": cmd,
            "args": {"text": "hi"},
            "target": target,
        })
    }

    fn resolved_target_of(out: &super::HandlerOutput) -> Option<String> {
        out[0]
            .0
            .payload
            .get("resolved_target")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }

    fn delivered_to_of(out: &super::HandlerOutput) -> Option<usize> {
        out[0]
            .0
            .payload
            .get("delivered_to")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
    }

    /// Two connections share one handle (a relaunched frontend whose
    /// predecessor has not been reaped yet) and one of them is active.
    /// Delivery is by SERIAL, so exactly one connection acts — and
    /// `delivered_to` must say 1, not the handle's population. Counting
    /// handles here would over-report an exclusive delivery.
    #[tokio::test]
    async fn untargeted_send_counts_the_exclusive_connection_not_the_shared_handle() {
        let clients = Clients::new();
        let stale = clients.register("c-stale", "local", None, "0.6.0", 1, Some("win-fe-a".into()));
        let active = clients.register("c-active", "local", None, "0.6.0", 1, Some("win-fe-a".into()));
        clients.touch_person_input(active.serial());

        let (tx, mut rx) = broadcast::channel::<FeCommandEvt>(8);
        let out = handle_fe_command_send(1, req_json("notify", None), &tx, &clients)
            .await
            .expect("handler ok");
        assert_eq!(resolved_target_of(&out).as_deref(), Some("win-fe-a"));
        assert_eq!(
            delivered_to_of(&out),
            Some(1),
            "delivery is by serial: one connection acts even though two share the handle"
        );

        let evt = rx.try_recv().expect("exactly one evt published");
        assert_eq!(
            evt.target_serial,
            Some(active.serial()),
            "the ACTIVE connection's serial, not the stale one sharing its handle"
        );
        assert_ne!(active.serial(), stale.serial(), "two distinct connections");
    }

    /// No `target` on the wire + an active client registered → the daemon
    /// resolves delivery to that client's CONNECTION EXCLUSIVELY
    /// (`target_serial`, design point B) — not merely its handle, which a
    /// second connection could share.
    #[tokio::test]
    async fn untargeted_send_with_an_active_client_delivers_to_it_only() {
        let clients = Clients::new();
        let active = clients.register("c-active", "local", None, "0.6.0", 1, Some("win-fe-a".into()));
        let _idle = clients.register("c-idle", "local", None, "0.6.0", 1, Some("win-fe-b".into()));
        clients.touch_person_input(active.serial());

        let (tx, mut rx) = broadcast::channel::<FeCommandEvt>(8);
        let out = handle_fe_command_send(1, req_json("notify", None), &tx, &clients)
            .await
            .expect("handler ok");
        assert_eq!(resolved_target_of(&out).as_deref(), Some("win-fe-a"));
        assert_eq!(
            delivered_to_of(&out),
            Some(1),
            "exclusive delivery to the resolved handle -> exactly one attached frontend"
        );

        let evt = rx.try_recv().expect("exactly one evt published");
        assert_eq!(
            evt.target.as_deref(),
            Some("win-fe-a"),
            "resolves to the active frontend, not a broadcast"
        );
        assert_eq!(
            evt.target_serial,
            Some(active.serial()),
            "exclusive delivery is by SERIAL, not merely the handle string"
        );
        assert!(rx.try_recv().is_err(), "only one evt published");
    }

    /// With no active client (none registered a handle, or none touched
    /// recently), an untargeted send falls through to today's behaviour
    /// unchanged: `target`/`target_serial` stay `None`, which every
    /// connection's `route_fe_command` self-filter reads as "broadcast, act".
    /// `delivered_to` now says how big that broadcast's real audience is —
    /// every attached, handle-bearing frontend (here, two), not just "some".
    #[tokio::test]
    async fn untargeted_send_with_no_active_client_broadcasts_as_before() {
        let clients = Clients::new();
        // Registered but never touched by a person -> no active frontend.
        let _idle_a = clients.register("c-idle-a", "local", None, "0.6.0", 1, Some("win-fe-a".into()));
        let _idle_b = clients.register("c-idle-b", "local", None, "0.6.0", 1, Some("win-fe-b".into()));

        let (tx, mut rx) = broadcast::channel::<FeCommandEvt>(8);
        let out = handle_fe_command_send(1, req_json("notify", None), &tx, &clients)
            .await
            .expect("handler ok");
        assert_eq!(resolved_target_of(&out), None);
        assert_eq!(
            delivered_to_of(&out),
            Some(2),
            "an undirected broadcast's delivered_to counts every attached frontend, not merely 0/1"
        );

        let evt = rx.try_recv().expect("exactly one evt published");
        assert!(evt.target.is_none(), "no active frontend -> today's broadcast behaviour");
        assert!(evt.target_serial.is_none());
    }

    /// An explicit `--fe <handle>` target is never overridden by the
    /// active-frontend resolution, even when a different client is active,
    /// and stays a handle-matched broadcast (`target_serial` unset).
    ///
    /// This is the exact shape of the 2026-09-09 field incident: no
    /// attached connection has the handle "win-fe-explicit" (only
    /// "win-fe-a" is registered), yet the ack was `ok:true` regardless —
    /// `delivered_to == Some(0)` is the fix, the ground truth the old ack
    /// could not report.
    #[tokio::test]
    async fn explicit_target_is_never_overridden_by_active_resolution() {
        let clients = Clients::new();
        let active = clients.register("c-active", "local", None, "0.6.0", 1, Some("win-fe-a".into()));
        clients.touch_person_input(active.serial());

        let (tx, mut rx) = broadcast::channel::<FeCommandEvt>(8);
        let out = handle_fe_command_send(1, req_json("notify", Some("win-fe-explicit")), &tx, &clients)
            .await
            .expect("handler ok");
        assert_eq!(resolved_target_of(&out).as_deref(), Some("win-fe-explicit"));
        assert_eq!(
            delivered_to_of(&out),
            Some(0),
            "ok:true but delivered to nobody -- the bug this field fixes"
        );

        let evt = rx.try_recv().expect("exactly one evt published");
        assert_eq!(evt.target.as_deref(), Some("win-fe-explicit"));
        assert!(evt.target_serial.is_none(), "explicit --fe stays a handle-matched broadcast");
    }

    /// The happy-path mirror of the case above: an explicit `--fe <handle>`
    /// that IS attached counts as delivered.
    #[tokio::test]
    async fn explicit_target_that_is_attached_delivers_to_it() {
        let clients = Clients::new();
        let _target = clients.register("c-target", "local", None, "0.6.0", 1, Some("win-fe-target".into()));

        let (tx, mut rx) = broadcast::channel::<FeCommandEvt>(8);
        let out = handle_fe_command_send(1, req_json("notify", Some("win-fe-target")), &tx, &clients)
            .await
            .expect("handler ok");
        assert_eq!(resolved_target_of(&out).as_deref(), Some("win-fe-target"));
        assert_eq!(delivered_to_of(&out), Some(1));

        let evt = rx.try_recv().expect("exactly one evt published");
        assert_eq!(evt.target.as_deref(), Some("win-fe-target"));
    }

    /// Design point E: an untargeted `relaunch` with NO active frontend
    /// publishes NOTHING — a command nobody could safely act on anyway
    /// (every FE refuses an undirected relaunch) — and the ack's
    /// `resolved_target` is `None` so `sot-fe` can fail visibly instead of
    /// reporting success for a no-op. `delivered_to` says the same thing
    /// numerically: `Some(0)`, since nothing was published.
    #[tokio::test]
    async fn untargeted_relaunch_with_no_active_frontend_publishes_nothing() {
        let clients = Clients::new();
        let _idle = clients.register("c-idle", "local", None, "0.6.0", 1, Some("win-fe-a".into()));

        let (tx, mut rx) = broadcast::channel::<FeCommandEvt>(8);
        let out = handle_fe_command_send(1, req_json("relaunch", None), &tx, &clients)
            .await
            .expect("handler ok");
        assert_eq!(resolved_target_of(&out), None);
        assert_eq!(delivered_to_of(&out), Some(0), "nothing was published, so nothing was delivered");
        assert!(
            rx.try_recv().is_err(),
            "an unresolved relaunch must not publish ANYTHING, not even an untargeted broadcast"
        );
    }

    /// A relaunch WITH an active frontend behaves like any other verb:
    /// exclusive delivery by serial, `resolved_target` names the winner.
    #[tokio::test]
    async fn untargeted_relaunch_with_an_active_frontend_delivers_to_it_only() {
        let clients = Clients::new();
        let active = clients.register("c-active", "local", None, "0.6.0", 1, Some("win-fe-a".into()));
        clients.touch_person_input(active.serial());

        let (tx, mut rx) = broadcast::channel::<FeCommandEvt>(8);
        let out = handle_fe_command_send(1, req_json("relaunch", None), &tx, &clients)
            .await
            .expect("handler ok");
        assert_eq!(resolved_target_of(&out).as_deref(), Some("win-fe-a"));
        assert_eq!(delivered_to_of(&out), Some(1));

        let evt = rx.try_recv().expect("exactly one evt published");
        assert_eq!(evt.target_serial, Some(active.serial()));
    }
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

/// Resolve the sot-comm registry path: `<sot-comm home>/registry.json`,
/// via the ONE shared resolver (`paths::sot_comm_home`, Codex round
/// finding 8 — the same one `capsule_workspace::capsule_supervisor_env`
/// injects as `SOT_COMM_HOME` into a spawned capsule's env, so the daemon
/// and the scripts can never disagree about where `~/.sot-comm` is).
/// Returns `None` only when the resolver itself found nothing (no
/// `SOT_COMM_HOME`, `HOME`, or `USERPROFILE`) — every other failure is
/// the caller's to treat as "absent" (empty strings).
pub(crate) fn comm_registry_path() -> Option<std::path::PathBuf> {
    let mut p = crate::paths::sot_comm_home()?;
    p.push("registry.json");
    Some(p)
}

/// A capsule row's comm handle, read back from the SAME self-file
/// `capsule_workspace::capsule_supervisor_env` pinned into its producer's
/// env (`SOT_COMM_SELF_FILE`) — Codex round finding 2/companion: since
/// the daemon no longer synthesizes/pins a name for an un-pinned capsule,
/// `comm-join.sh`'s own #148 auto-disambiguating derivation is what
/// actually decides the handle, and it writes that decision as the
/// self-file's first line. `resolve_handle`'s tmux-session match (below)
/// can never find a capsule row at all — a capsule has no tmux pane, so
/// its own comm-join.sh row's `tmux` field is always empty. `""` on any
/// failure (not yet joined, unreadable, empty file) — callers already
/// fall back to the stored `agent_name` exactly as the tmux path does.
fn capsule_comm_handle(workspace_id: &str) -> String {
    let Some(comm_home) = crate::paths::sot_comm_home() else {
        return String::new();
    };
    let host = crate::workspaces::state_host();
    let self_file = comm_home.join("self").join(format!("{host}__{workspace_id}.txt"));
    std::fs::read_to_string(&self_file)
        .ok()
        .and_then(|s| s.lines().next().map(str::to_string))
        .unwrap_or_default()
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

/// Does this sot-comm registry row's `host` field match `host` (case-
/// insensitive)? An absent or empty `host` is UNKNOWN ownership, never a
/// match — comm-join.sh has stamped `host` (the raw `hostname -s`, case
/// preserved) on every row since the registry existed, so a row without
/// one is not evidence it's ours (LU5d2, Codex text round finding 3: the
/// prior "no host = legacy, matches on session alone" clause let a
/// hand-edited or foreign-tool row bind here on name/session alone).
/// Factored out of `comm_row_owned_here` so the `by_name` term in
/// `remove_comm_agents_for_workspace` and the plain field reads in
/// `handle_workspace_list`'s `agent_str` apply the same strict rule as the
/// tmux-based match below.
fn host_matches(entry: &serde_json::Value, host: &str) -> bool {
    entry
        .get("host")
        .and_then(|v| v.as_str())
        .map(|h| !h.is_empty() && h.eq_ignore_ascii_case(host))
        .unwrap_or(false)
}

/// Does this sot-comm registry row belong to `host`'s occupant of
/// `tmux_session`? `~/.sot-comm/registry.json` is ONE file shared by every
/// host on an NFS-homed cluster, so two hosts can each run a session with
/// the same slug — matching on the tmux session part alone let one host's
/// `workspace.list` show, and one host's destroy delete, another host's
/// row. `host` is `state_host()`, resolved ONCE by the caller (not per
/// row).
fn comm_row_owned_here(entry: &serde_json::Value, tmux_session: &str, host: &str) -> bool {
    if tmux_session.is_empty() {
        return false;
    }
    let same_session = entry
        .get("tmux")
        .and_then(|v| v.as_str())
        .map(|t| t.split(':').next().unwrap_or("") == tmux_session)
        .unwrap_or(false);
    same_session && host_matches(entry, host)
}

/// Resolve the sot-comm handle bound to a workspace via its LIVE TMUX
/// OCCUPANT, so manually-joined / pre-state-nav agents (whose stored
/// `agent_name` was never set — only the spawn path writes it) still bind.
/// The registry `tmux` field is `"<session>:<win>.<pane>"`; match its
/// session part against `tmux_session`, filtered through
/// `comm_row_owned_here` so a same-slug session on another host never
/// binds here. Falls back to `stored_agent_name` when there's no live tmux
/// match (e.g. a `spawning` row whose `tmux` is still `""`).
///
/// This is the TMUX half of the row-binding rule; `comm_handle_for_workspace`
/// below is the whole rule (it also covers capsule rows, which have no tmux
/// pane at all) and is what callers should use. Kept as its own function
/// because it's independently useful — and independently tested — as "the
/// live occupant of this tmux session".
fn resolve_comm_handle(
    agents: Option<&serde_json::Value>,
    tmux_session: &str,
    stored_agent_name: &str,
    host: &str,
) -> String {
    if !tmux_session.is_empty() {
        if let Some(agents) = agents.and_then(|a| a.as_object()) {
            // Several rows can share a session (different panes, or a stale
            // duplicate handle); prefer the most-recently-seen so we bind the
            // live occupant, not a dead row. ISO `last_seen` compares lexically.
            let mut best: Option<(&str, &str)> = None;
            for (handle, entry) in agents {
                if !comm_row_owned_here(entry, tmux_session, host) {
                    continue;
                }
                let seen = entry
                    .get("last_seen")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if best.map_or(true, |(_, bseen)| seen > bseen) {
                    best = Some((handle.as_str(), seen));
                }
            }
            if let Some((h, _)) = best {
                return h.to_string();
            }
        }
    }
    stored_agent_name.to_string()
}

/// Which sot-comm registry row is workspace `ws`'s? THE ONE place that
/// answers this — `handle_workspace_list` (the FE's `state`/`summary`
/// merge) and `clear_comm_unread` (ADR 0044's read-clears-blue) both call
/// this rather than each encoding their own copy of the rule, so they can
/// never disagree about which row a workspace owns.
///
/// - `runtime == "capsule"`: no tmux pane exists at all, so the tmux-based
///   match below can never discover it (Codex round finding 2/companion)
///   — read its OWN pinned self-file back instead (`capsule_comm_handle`),
///   falling back to the stored `agent_name` only when that file is
///   empty/absent (an un-pinned capsule with no explicit name).
/// - every other runtime: the live tmux occupant (`resolve_comm_handle`
///   above), same fallback.
fn comm_handle_for_workspace(
    ws: &Workspace,
    agents: Option<&serde_json::Value>,
    host: &str,
) -> String {
    if ws.runtime == "capsule" {
        let h = capsule_comm_handle(&ws.workspace_id);
        if h.is_empty() {
            ws.agent_name.clone()
        } else {
            h
        }
    } else {
        resolve_comm_handle(agents, &ws.tmux_session, &ws.agent_name, host)
    }
}

/// Take the sot-comm registry's mkdir-spinlock (`<comm_home>/.registry.lock`,
/// matching `comm-lib.sh`'s own `with_lock`), run `f` with the registry and
/// tmp-file paths, and release the lock on every exit path. THE ONE lock
/// helper — `remove_comm_agents_for_workspace` and `clear_comm_unread` both
/// call this rather than each spinning its own mkdir loop, so there is one
/// lock protocol, not two with quietly different rules.
///
/// Bounded at `bound` (polled every 50ms) and FAILS CLOSED: when the lock
/// can't be taken within it, this gives up and returns `None` rather than
/// force-breaking it — `with_lock`'s own rule since PR #148 finding F2. A
/// caller that skips its write this once is cosmetic (the next writer, or
/// the next attempt, retries against a byte-identical row); a forced
/// takeover can corrupt a concurrent shell writer's in-flight
/// `registry.json.tmp`, which is not recoverable the same way.
///
/// `None` also on anything that keeps this from even starting: no
/// `comm_registry_path()` (no `SOT_COMM_HOME`/`HOME`/`USERPROFILE`), or a
/// path with no parent directory.
fn with_comm_registry_lock<T>(
    bound: std::time::Duration,
    f: impl FnOnce(&std::path::Path, &std::path::Path) -> T,
) -> Option<T> {
    const POLL: std::time::Duration = std::time::Duration::from_millis(50);
    let reg_path = comm_registry_path()?;
    let dir = reg_path.parent()?.to_path_buf();
    let lock_dir = dir.join(".registry.lock");
    let tmp_path = dir.join("registry.json.tmp");

    let tries = (bound.as_millis() / POLL.as_millis()).max(1) as u32;
    let mut acquired = false;
    for _ in 0..tries {
        match std::fs::create_dir(&lock_dir) {
            Ok(()) => {
                acquired = true;
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                std::thread::sleep(POLL);
            }
            Err(e) => {
                tracing::warn!(error = %e, lock = ?lock_dir, "comm registry lock error");
                return None;
            }
        }
    }
    if !acquired {
        tracing::warn!(
            lock = ?lock_dir, ?bound,
            "comm registry lock contended for the full bound — giving up \
             (fail-closed: never force-broken, matching comm-lib.sh's \
             with_lock since PR #148 F2)"
        );
        return None;
    }

    // RAII release: `f` runs under the caller's `spawn_blocking`, which
    // contains a panic (the awaiting task just sees a `JoinError`), but a
    // plain "release after the call" would only run on the NORMAL return
    // path — a panic mid-critical-section would leave `.registry.lock`
    // behind forever, and since nothing force-breaks it any more (the
    // fail-closed fix above), every subsequent writer — this daemon's own
    // callers and every `comm-status.sh` hook on the shared home — would
    // wedge closed permanently. The guard's `Drop` runs on unwind too, so
    // the lock is released either way.
    let _guard = CommRegistryLockGuard {
        lock_dir: &lock_dir,
    };
    Some(f(&reg_path, &tmp_path))
}

/// Releases `lock_dir` on drop — including during a panic unwind — so
/// `with_comm_registry_lock` above always releases the mkdir-spinlock it
/// took, whatever `f` does.
struct CommRegistryLockGuard<'a> {
    lock_dir: &'a std::path::Path,
}

impl Drop for CommRegistryLockGuard<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(self.lock_dir);
    }
}

/// `remove_comm_agents_for_workspace`'s lock bound: a `workspace.destroy`
/// is a one-shot, user-triggered action off the hot per-connection reply
/// path (it already awaits the tmux kill above), so it can afford to sit
/// out a much longer contention window than `clear_comm_unread` below
/// before giving up — matches the OLD force-break threshold (200×50ms), so
/// the fail-closed change doesn't also make destroys flaky on an
/// ordinarily-brief contention.
const COMM_PRUNE_LOCK_BOUND: std::time::Duration = std::time::Duration::from_secs(10);

/// `clear_comm_unread`'s lock bound: `server.rs`'s `handle_connection`
/// awaits every handler inline, so a long spin here would stall the whole
/// `workspace.activate` reply — bounded much tighter than the prune above.
const CLEAR_COMM_UNREAD_LOCK_BOUND: std::time::Duration = std::time::Duration::from_secs(1);

/// Remove the sot-comm registry rows owned by a workspace that is being
/// destroyed, returning the handles removed (for logging). A killed agent can't
/// run `comm-leave` for itself, so its row would otherwise persist and show as
/// a ghost in `workspace.list`. We mirror `handle_workspace_list`'s row-binding
/// rule — a row belongs to this workspace when `comm_row_owned_here` matches
/// (its `tmux` session-part equals `tmux_session` AND it's this `host`'s row),
/// or (fallback for not-yet-joined `spawning` rows) when its handle equals the
/// stored `agent_name` AND `host_matches` too (LU5d2: the stored name is
/// caller-supplied, not proof of ownership — a same-named row stamped by
/// another host must survive). ALL matching rows are dropped, including stale
/// duplicates on the same session.
///
/// Fully best-effort: a missing registry, malformed JSON, a lock that can't be
/// taken within `COMM_PRUNE_LOCK_BOUND` (fail-closed, via
/// `with_comm_registry_lock` — never force-broken), or any I/O failure yields
/// an empty result and never propagates — the destroy must not fail because
/// the registry couldn't be pruned. Writes via a temp file + atomic rename so
/// a concurrent bash mutator (comm-join / comm-status / …) can't see a torn
/// file.
fn remove_comm_agents_for_workspace(tmux_session: &str, agent_name: &str, host: &str) -> Vec<String> {
    remove_comm_agents_for_workspace_bounded(tmux_session, agent_name, host, COMM_PRUNE_LOCK_BOUND)
}

/// `remove_comm_agents_for_workspace` with an explicit lock bound — split out
/// so a test can exercise the real prune body under a SHORT contended-lock
/// bound (proving it fails closed, same as `clear_comm_unread`'s own test)
/// without waiting out the real `COMM_PRUNE_LOCK_BOUND`. Production code
/// only ever calls the wrapper above.
fn remove_comm_agents_for_workspace_bounded(
    tmux_session: &str,
    agent_name: &str,
    host: &str,
    bound: std::time::Duration,
) -> Vec<String> {
    with_comm_registry_lock(bound, |reg_path, tmp_path| -> Vec<String> {
        let bytes = match std::fs::read(reg_path) {
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
                let by_name =
                    !agent_name.is_empty() && handle == agent_name && host_matches(entry, host);
                let by_tmux = comm_row_owned_here(entry, tmux_session, host);
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
        if let Err(e) = std::fs::write(tmp_path, &serialized) {
            tracing::warn!(error = %e, "comm registry tmp write failed");
            return Vec::new();
        }
        if let Err(e) = std::fs::rename(tmp_path, reg_path) {
            tracing::warn!(error = %e, "comm registry rename failed");
            let _ = std::fs::remove_file(tmp_path);
            return Vec::new();
        }
        to_remove
    })
    .unwrap_or_default()
}

/// Clear a `done` row's blue after a PERSON switched the view onto it
/// (`workspace.activate { read: true }` — ADR 0044 "Viewing clears blue").
/// Flips `.agents[<handle>].state` from `"done"` to `"idle"` and writes
/// NOTHING else: the summary survives (the row reads `idle · last: …`) and
/// `status_at` is untouched, so reading a parked row doesn't make it look
/// recently active. `blocked`, `waiting`, `working`, and an already-`idle`
/// row are never touched — viewing is not answering, and it is not
/// finishing a job.
///
/// Two phases, both filtered through `comm_handle_for_workspace` — THE SAME
/// row-binding rule `handle_workspace_list` uses (capsule self-file first,
/// else the live tmux occupant / stored `agent_name`), so a capsule
/// workspace's blue clears exactly the same way a tmux one's does:
///
/// 1. **Unlocked pre-check** — read the registry once, resolve the handle,
///    require the row to pass `host_matches` and have `state == "done"`.
///    Anything else returns with no lock taken and no write — the common
///    activate (nothing to clear, or no registry at all) costs one file
///    read, same as the `workspace.list` call that follows every activate
///    on the wire.
/// 2. **Lock, then re-read and re-decide inside it.** ADR 0044 round 2:
///    read-decide-write is one critical section; the pre-check is only a
///    filter and can never itself cause a write.
///
/// Lock protocol is `with_comm_registry_lock` (same helper
/// `remove_comm_agents_for_workspace` uses), bounded at
/// `CLEAR_COMM_UNREAD_LOCK_BOUND` (much tighter — see its doc comment).
///
/// Best-effort throughout: a missing registry, malformed JSON, or any I/O
/// failure is a silent no-op — the activate's ack is unaffected either
/// way (the caller sends it regardless of what this does).
fn clear_comm_unread(ws: &Workspace, host: &str) {
    // --- Unlocked pre-check ---
    let pre_agents = read_comm_agents();
    let handle = comm_handle_for_workspace(ws, pre_agents.as_ref(), host);
    if handle.is_empty() {
        return;
    }
    let is_done = pre_agents
        .as_ref()
        .and_then(|a| a.get(&handle))
        .filter(|entry| host_matches(entry, host))
        .and_then(|entry| entry.get("state"))
        .and_then(|v| v.as_str())
        .map(|s| s == "done")
        .unwrap_or(false);
    if !is_done {
        return;
    }

    with_comm_registry_lock(CLEAR_COMM_UNREAD_LOCK_BOUND, |reg_path, tmp_path| {
        let bytes = match std::fs::read(reg_path) {
            Ok(b) => b,
            Err(_) => return,
        };
        let mut root: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => return,
        };
        // Re-resolve and re-decide against the freshly-read registry — it
        // may have changed since the pre-check above.
        let handle = comm_handle_for_workspace(ws, root.get("agents"), host);
        if handle.is_empty() {
            return;
        }
        let Some(agents) = root.get_mut("agents").and_then(|a| a.as_object_mut()) else {
            return;
        };
        let Some(entry) = agents.get_mut(&handle) else {
            return;
        };
        let still_done = host_matches(entry, host)
            && entry.get("state").and_then(|v| v.as_str()) == Some("done");
        if !still_done {
            return;
        }
        let Some(entry_obj) = entry.as_object_mut() else {
            return;
        };
        // ONLY `state`. Not `status_at`, `last_seen`, `summary`, or
        // `turn_origin` — reading is not activity and must not make a
        // parked row look recently touched.
        entry_obj.insert(
            "state".to_string(),
            serde_json::Value::String("idle".to_string()),
        );

        let mut serialized = match serde_json::to_vec_pretty(&root) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "comm registry serialize failed");
                return;
            }
        };
        serialized.push(b'\n');
        if let Err(e) = std::fs::write(tmp_path, &serialized) {
            tracing::warn!(error = %e, "comm registry tmp write failed");
            return;
        }
        if let Err(e) = std::fs::rename(tmp_path, reg_path) {
            tracing::warn!(error = %e, "comm registry rename failed");
            let _ = std::fs::remove_file(tmp_path);
        }
    });
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
    let host = crate::workspaces::state_host();
    // Pull `.agents[agent_name].<field>` as an owned String, "" if anything is
    // missing or not a string. LU5d2: `agent_name` here is a handle the caller
    // (below) already bound to THIS workspace — by live tmux match or by the
    // stored `agent_name` fallback — never proof it's this host's row, so
    // filter the entry through `host_matches` too: a same-named handle
    // stamped by another host on the shared registry must read as empty, not
    // leak its summary/status_at/state into this host's list.
    let agent_str = |agent_name: &str, field: &str| -> String {
        if agent_name.is_empty() {
            return String::new();
        }
        comm_agents
            .as_ref()
            .and_then(|a| a.get(agent_name))
            .filter(|entry| host_matches(entry, &host))
            .and_then(|entry| entry.get(field))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    // Resolve the comm handle ACTUALLY running in a workspace's tmux session, so
    // manually-joined / pre-state-nav agents (whose `ws.agent_name` was never set
    // — only the spawn path writes it) still bind. `resolve_comm_handle` does the
    // actual matching (shared with `clear_comm_unread` below — one rule, not a
    // copy).
    let ws_list = workspaces.list();
    // ADR 0042 slice L1a, Codex review finding 11: query every capsule
    // workspace's supervisor lane under FIXED-WIDTH concurrency (a
    // semaphore, the same `LANE_CONCURRENCY` bound the startup
    // resume-scan uses — finding 10) and ONE absolute deadline over the
    // WHOLE gather, never a fresh per-row budget (which let total call
    // time grow unboundedly with row count, and handed a wedged task a
    // brand-new allowance every time its own handle happened to be
    // reached). A workspace already marked `capsule_terminal` (finding 6
    // — its watchdog gave up) is never queried at all; its phase is
    // "terminal", not a fresh "unreachable" that would misleadingly
    // imply the next probe might succeed. `capsule_workspace::phase_of`
    // is gated to Windows and Linux only (ADR 0043 decision 22); on any
    // other host no workspace ever has `runtime == "capsule"`, so the
    // query set there is always empty.
    #[cfg(any(windows, target_os = "linux"))]
    let phases: std::collections::HashMap<String, String> = {
        let candidates: Vec<(String, std::path::PathBuf)> = match sot_log::state_dir::sot_state_dir() {
            Some(root) => ws_list
                .iter()
                .filter(|ws| ws.runtime == "capsule" && !workspaces.is_capsule_terminal(&ws.workspace_id))
                .map(|ws| (ws.workspace_id.clone(), crate::capsule_workspace::state_dir_for(&root, &ws.workspace_id)))
                .collect(),
            None => Vec::new(),
        };
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(crate::capsule_workspace::LANE_CONCURRENCY));
        let mut handles = Vec::with_capacity(candidates.len());
        for (id, dir) in candidates {
            let permit = semaphore.clone();
            handles.push(tokio::spawn(async move {
                let _permit = permit.acquire_owned().await;
                let phase = tokio::task::spawn_blocking(move || crate::capsule_workspace::phase_of(&dir))
                    .await
                    .unwrap_or(crate::capsule_workspace::UNREACHABLE_PHASE);
                (id, phase.to_string())
            }));
        }
        let mut out = std::collections::HashMap::new();
        let gather = async {
            for h in handles {
                if let Ok((id, phase)) = h.await {
                    out.insert(id, phase);
                }
            }
        };
        let _ = tokio::time::timeout(crate::capsule_workspace::LIST_LANE_DEADLINE, gather).await;
        out
    };
    #[cfg(not(any(windows, target_os = "linux")))]
    let phases: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut entries: Vec<WorkspaceListEntry> = ws_list
        .into_iter()
        .map(|ws| {
            // Which registry row is this workspace's — one rule, shared with
            // `clear_comm_unread` (`comm_handle_for_workspace`).
            let handle = comm_handle_for_workspace(&ws, comm_agents.as_ref(), &host);
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
            // ADR 0042 slice L1a: `state_dir` is a pure function of the
            // state root + workspace_id (no I/O, no query) so it's
            // available even when the phase query below failed or timed
            // out. `phase` — Codex review finding 6 — checks
            // `capsule_terminal` FIRST: a workspace whose watchdog gave
            // up is never re-queried, and reports "terminal" (loud and
            // final) rather than a fresh "unreachable" that misleadingly
            // implies the next probe might still succeed; otherwise it
            // falls back to "unreachable" for an unresolved/failed query
            // ("failure -> unreachable" — see `capsule_workspace`'s own
            // doc). Both stay `None` for a `"tmux"` row.
            let (state_dir, phase) = if ws.runtime == "capsule" {
                let state_dir = sot_log::state_dir::sot_state_dir().map(|root| {
                    crate::capsule_workspace::state_dir_for(&root, &ws.workspace_id)
                        .to_string_lossy()
                        .into_owned()
                });
                let phase = if workspaces.is_capsule_terminal(&ws.workspace_id) {
                    crate::capsule_workspace::phase_str(sot_log::wire::SupervisorPhase::Terminal).to_string()
                } else {
                    phases
                        .get(&ws.workspace_id)
                        .cloned()
                        .unwrap_or_else(|| crate::capsule_workspace::UNREACHABLE_PHASE.to_string())
                };
                (state_dir, Some(phase))
            } else {
                (None, None)
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
                repl_state: ws.repl_state().to_string(),
                runtime: ws.runtime.clone(),
                state_dir,
                phase,
            }
        })
        .collect();
    // Pin the default workspace (the daemon's home anchor) FIRST: the FE never
    // lists it, but its position is the strip's own active-index fallback.
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

/// `workspace.activate` — builds the ack. The connection-local state this
/// updates (`active_workspace`, `server.rs`'s `handle_connection`) is
/// mutated by the CALLER, not here — this function only resolves
/// `req.workspace_id` (again; the caller does the same resolve to learn
/// what to store, mirroring how the `HELLO` arm computes the auth flag
/// inline before calling `handle_hello`) and echoes back the canonical id,
/// or `None` when it didn't resolve. See `op::WORKSPACE_ACTIVATE` (ops.rs)
/// for the full design.
pub async fn handle_workspace_activate(
    req_id: u64,
    payload_json: serde_json::Value,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    use sot_protocol::{WorkspaceActivateReq, WorkspaceActivateRes};
    let req: WorkspaceActivateReq =
        serde_json::from_value(payload_json).context("workspace.activate payload")?;
    let resolved_ws = workspaces.resolve(req.workspace_id.as_deref());
    let resolved = resolved_ws.as_ref().map(|ws| ws.workspace_id.clone());
    // `read: true` = a PERSON switched the view here (Sessions-Enter,
    // Shift+Left/Right cycling) — clear this row's blue (ADR 0044). The ack
    // below is sent unconditionally, whatever this does or doesn't clear.
    if req.read {
        if let Some(ws) = resolved_ws.clone() {
            let host = crate::workspaces::state_host();
            let _ = tokio::task::spawn_blocking(move || clear_comm_unread(&ws, &host)).await;
        }
    }
    tracing::info!(
        requested = req.workspace_id.as_deref().unwrap_or("<default>"),
        resolved = resolved.as_deref().unwrap_or("<unresolved>"),
        read = req.read,
        "workspace.activate"
    );
    let res = WorkspaceActivateRes {
        workspace_id: resolved,
    };
    Ok(vec![(
        Frame::res(req_id, op::WORKSPACE_ACTIVATE, serde_json::to_value(res)?),
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
mod duplicate_root_tests {
    use super::find_other_workspace_with_root;
    use crate::workspaces::{Workspace, Workspaces};
    use std::path::{Path, PathBuf};

    /// Unique on-disk dir per test (no tempfile dev-dep; pid + a counter keep
    /// parallel tests from colliding). Never cleaned up — OS temp is fine.
    fn scratch_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "sot-duproot-{}-{}-{}",
            std::process::id(),
            tag,
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&d).expect("create scratch dir");
        d
    }

    fn ws(label: &str, root: &Path) -> Workspace {
        Workspace::from_label(label, root.to_path_buf(), false, "none".into(), String::new(), String::new())
    }

    fn reg(rows: Vec<Workspace>) -> Workspaces {
        let r = Workspaces::new();
        for w in rows {
            r.insert(w);
        }
        r
    }

    #[test]
    fn same_root_different_slug_is_found() {
        let root = scratch_dir("hit");
        let existing = reg(vec![ws("sot", &root)]);
        let canon = root.canonicalize().unwrap();
        let hit = find_other_workspace_with_root(&canon, "ship-of-tools", &existing)
            .expect("a second identity for one root must be caught");
        assert_eq!(hit.slug, "sot");
    }

    #[test]
    fn same_slug_is_invisible_so_refresh_stays_allowed() {
        // A same-slug create is Workspaces::insert's id-preserving metadata
        // refresh; the gate must not turn that idempotent path into an error.
        let root = scratch_dir("refresh");
        let existing = reg(vec![ws("sot", &root)]);
        let canon = root.canonicalize().unwrap();
        assert!(find_other_workspace_with_root(&canon, "sot", &existing).is_none());
    }

    #[test]
    fn different_roots_pass() {
        let a = scratch_dir("a");
        let b = scratch_dir("b");
        let existing = reg(vec![ws("sot", &a)]);
        let canon = b.canonicalize().unwrap();
        assert!(find_other_workspace_with_root(&canon, "other", &existing).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_spelling_of_a_registered_root_still_collides() {
        // The incident shape with a twist: the duplicate is registered via a
        // symlink to the same directory. Canonical comparison must see through
        // it — path-string comparison would not.
        let root = scratch_dir("real");
        let link = std::env::temp_dir().join(format!("sot-duproot-link-{}", std::process::id()));
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&root, &link).expect("create symlink");
        let existing = reg(vec![ws("sot", &link)]);
        let canon = root.canonicalize().unwrap();
        let hit = find_other_workspace_with_root(&canon, "ship-of-tools", &existing)
            .expect("symlinked duplicate must be caught");
        assert_eq!(hit.slug, "sot");
    }

    /// ADR 0042 amendment: the inert default anchor (the default row with no
    /// agent, root = the home dir, any runtime) is not a session, so a session
    /// created at that root passes the gate — while a default row that
    /// carries an agent is still refused.
    #[test]
    fn inert_default_anchor_does_not_block_a_session_at_its_root() {
        let root = scratch_dir("anchor");
        let canon = root.canonicalize().unwrap();
        let existing = Workspaces::new();
        let mut anchor = ws("local", &root);
        anchor.runtime = "capsule".to_string();
        let anchor = existing.insert(anchor);
        existing.set_default(&anchor.workspace_id);
        assert!(
            find_other_workspace_with_root(&canon, "home-session", &existing).is_none(),
            "the inert anchor must not claim its root against a real session"
        );
        // Control: the default row WITH an agent is a real session and is
        // still caught (same-slug insert keeps the id, so it stays default).
        let mut sot = ws("local", &root);
        sot.agent = "claude".to_string();
        existing.insert(sot);
        assert!(find_other_workspace_with_root(&canon, "home-session", &existing).is_some());
    }

    #[test]
    fn registered_root_that_no_longer_resolves_is_skipped_not_fatal() {
        // A workspace whose root was deleted is Phase 2's (reap) problem; the
        // gate must neither match it nor error on it.
        let gone = std::env::temp_dir().join(format!("sot-duproot-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&gone);
        let live = scratch_dir("live");
        let existing = reg(vec![ws("dead", &gone)]);
        let canon = live.canonicalize().unwrap();
        assert!(find_other_workspace_with_root(&canon, "other", &existing).is_none());
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
mod scalebar_sidecar_tests {
    use super::{merge_scale_sidecar, physical_scale_is_valid, write_scale_sidecar_atomic};
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sot-scale-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn write_scale_sidecar_atomic_roundtrips_verbatim() {
        let dir = tmp("write");
        let img = dir.join("render.png");
        std::fs::write(&img, b"\x89PNG").unwrap();
        let scale = serde_json::json!({
            "axes": [{"name":"x","nm_per_px":2.0},{"name":"y","nm_per_px":2.0}],
            "unit": "nm"
        });
        let sidecar = dir.join("render.png.scale.json");
        write_scale_sidecar_atomic(&sidecar, &scale).expect("atomic write");
        // Written verbatim + readable back through the same merge path.
        let extras = merge_scale_sidecar(&img, "image/png", None);
        let ps = extras
            .as_ref()
            .and_then(|e| e.get("physical_scale"))
            .expect("physical_scale from the written sidecar");
        assert_eq!(ps["unit"], "nm");
        assert_eq!(ps["axes"][0]["nm_per_px"], 2.0);
        assert_eq!(ps["axes"][1]["name"], "y");
        // No temp file left behind (atomic temp+rename cleaned up).
        let leftover: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftover.is_empty(), "temp file not cleaned: {leftover:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn physical_scale_is_valid_accepts_good_rejects_bad() {
        assert!(physical_scale_is_valid(&serde_json::json!({
            "axes": [{"name":"x","nm_per_px":5.0},{"name":"z","nm_per_px":20.0}],
            "unit": "nm"
        })));
        // empty axes
        assert!(!physical_scale_is_valid(
            &serde_json::json!({"axes":[],"unit":"nm"})
        ));
        // nm_per_px <= 0
        assert!(!physical_scale_is_valid(
            &serde_json::json!({"axes":[{"name":"x","nm_per_px":0.0}],"unit":"nm"})
        ));
        assert!(!physical_scale_is_valid(
            &serde_json::json!({"axes":[{"name":"x","nm_per_px":-1.0}],"unit":"nm"})
        ));
        // missing nm_per_px
        assert!(!physical_scale_is_valid(
            &serde_json::json!({"axes":[{"name":"x"}],"unit":"nm"})
        ));
        // missing axis name (would produce an unlabelable bar)
        assert!(!physical_scale_is_valid(
            &serde_json::json!({"axes":[{"nm_per_px":5.0}],"unit":"nm"})
        ));
        // missing unit
        assert!(!physical_scale_is_valid(
            &serde_json::json!({"axes":[{"name":"x","nm_per_px":2.0}]})
        ));
        // missing axes / not an object
        assert!(!physical_scale_is_valid(&serde_json::json!({"unit":"nm"})));
        assert!(!physical_scale_is_valid(&serde_json::json!("nope")));
    }

    #[test]
    fn sidecar_sets_physical_scale_for_raster() {
        let dir = tmp("set");
        let img = dir.join("render.png");
        std::fs::write(&img, b"").unwrap();
        std::fs::write(
            dir.join("render.png.scale.json"),
            br#"{"axes":[{"name":"x","nm_per_px":2.0},{"name":"y","nm_per_px":2.0}],"unit":"nm"}"#,
        )
        .unwrap();
        let extras = merge_scale_sidecar(&img, "image/png", None);
        let ps = extras
            .as_ref()
            .and_then(|e| e.get("physical_scale"))
            .expect("physical_scale set from sidecar");
        assert_eq!(ps["unit"], "nm");
        assert_eq!(ps["axes"][0]["nm_per_px"], 2.0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn no_sidecar_leaves_extras_untouched() {
        let dir = tmp("none");
        let img = dir.join("bare.png");
        std::fs::write(&img, b"").unwrap();
        assert!(merge_scale_sidecar(&img, "image/png", None).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn non_raster_mime_is_skipped() {
        let dir = tmp("skip");
        let f = dir.join("notes.txt");
        std::fs::write(&f, b"").unwrap();
        // A sidecar present but the mime isn't a raster → skipped, not read.
        std::fs::write(dir.join("notes.txt.scale.json"), b"{}").unwrap();
        assert!(merge_scale_sidecar(&f, "text/plain; charset=utf-8", None).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn merges_into_existing_extras_without_clobbering() {
        let dir = tmp("merge");
        let img = dir.join("m.png");
        std::fs::write(&img, b"").unwrap();
        std::fs::write(dir.join("m.png.scale.json"), br#"{"unit":"nm"}"#).unwrap();
        let extras = merge_scale_sidecar(&img, "image/png", Some(serde_json::json!({"page": 2})))
            .expect("some");
        assert_eq!(extras["page"], 2, "existing extras preserved");
        assert_eq!(extras["physical_scale"]["unit"], "nm");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invalid_json_sidecar_is_ignored() {
        let dir = tmp("bad");
        let img = dir.join("b.png");
        std::fs::write(&img, b"").unwrap();
        std::fs::write(dir.join("b.png.scale.json"), b"not json{").unwrap();
        assert!(merge_scale_sidecar(&img, "image/png", None).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod phys_scale_tests {
    use super::{merge_png_phys_scale, png_phys_nm_per_px};

    const SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

    /// A well-formed PNG chunk: len + type + data + the REAL CRC over
    /// type+data. The walk validates CRCs now, so a dummy value would make
    /// every fixture read as corrupt.
    fn chunk(ty: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut covered = ty.to_vec();
        covered.extend_from_slice(data);
        let mut v = (data.len() as u32).to_be_bytes().to_vec();
        v.extend_from_slice(&covered);
        v.extend_from_slice(&crc32fast::hash(&covered).to_be_bytes());
        v
    }

    /// Same, with a deliberately wrong CRC.
    fn chunk_bad_crc(ty: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut v = chunk(ty, data);
        let n = v.len();
        v[n - 1] ^= 0xFF;
        v
    }

    fn phys_data(ppm_x: u32, ppm_y: u32, unit: u8) -> Vec<u8> {
        let mut d = ppm_x.to_be_bytes().to_vec();
        d.extend_from_slice(&ppm_y.to_be_bytes());
        d.push(unit);
        d
    }

    fn png_with(chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut v = SIG.to_vec();
        for c in chunks {
            v.extend_from_slice(c);
        }
        v
    }

    /// 100_000_000 px/m == 10 nm/px exactly.
    fn isotropic_10nm() -> Vec<u8> {
        png_with(&[
            chunk(b"IHDR", &[0; 13]),
            chunk(b"pHYs", &phys_data(100_000_000, 100_000_000, 1)),
            chunk(b"IDAT", &[0; 4]),
            chunk(b"IEND", &[]),
        ])
    }

    #[test]
    fn metre_unit_phys_resolves_exactly() {
        assert_eq!(png_phys_nm_per_px(&isotropic_10nm()), Some((10.0, 10.0)));
        // Anisotropic still READS as two axes — suppression is the merge
        // layer's job, so the reader stays an honest report of the file.
        let aniso = png_with(&[
            chunk(b"pHYs", &phys_data(100_000_000, 200_000_000, 1)),
            chunk(b"IEND", &[]),
        ]);
        assert_eq!(png_phys_nm_per_px(&aniso), Some((10.0, 5.0)));
    }

    #[test]
    fn aspect_only_zero_density_and_missing_phys_resolve_none() {
        // unit 0 = aspect ratio only (dimensionless).
        let aspect = png_with(&[chunk(b"pHYs", &phys_data(2, 1, 0)), chunk(b"IEND", &[])]);
        assert_eq!(png_phys_nm_per_px(&aspect), None);
        // Unknown unit byte is not metres either.
        let unknown = png_with(&[chunk(b"pHYs", &phys_data(1000, 1000, 7)), chunk(b"IEND", &[])]);
        assert_eq!(png_phys_nm_per_px(&unknown), None);
        // Either axis zero is a division we must not do.
        for d in [phys_data(0, 5, 1), phys_data(5, 0, 1)] {
            let zero = png_with(&[chunk(b"pHYs", &d), chunk(b"IEND", &[])]);
            assert_eq!(png_phys_nm_per_px(&zero), None);
        }
        let none = png_with(&[chunk(b"IHDR", &[0; 13]), chunk(b"IEND", &[])]);
        assert_eq!(png_phys_nm_per_px(&none), None);
    }

    #[test]
    fn malformed_streams_resolve_none() {
        assert_eq!(png_phys_nm_per_px(b"not a png"), None);
        assert_eq!(png_phys_nm_per_px(b""), None);
        // pHYs after IDAT violates the spec — the walk stops at IDAT.
        let late = png_with(&[
            chunk(b"IDAT", &[0; 4]),
            chunk(b"pHYs", &phys_data(1_000_000, 1_000_000, 1)),
        ]);
        assert_eq!(png_phys_nm_per_px(&late), None);
        // A declared length that overruns the buffer, at every truncation
        // point of a valid file (never panics, never reads out of bounds).
        let full = isotropic_10nm();
        for cut in 1..full.len() {
            let _ = png_phys_nm_per_px(&full[..cut]);
        }
        // u32::MAX length: `checked_add` must catch it, not wrap.
        let mut huge = SIG.to_vec();
        huge.extend_from_slice(&u32::MAX.to_be_bytes());
        huge.extend_from_slice(b"pHYs");
        huge.extend_from_slice(&[0; 16]);
        assert_eq!(png_phys_nm_per_px(&huge), None);
    }

    #[test]
    fn spec_violations_are_rejected_not_skipped() {
        let good = chunk(b"pHYs", &phys_data(100_000_000, 100_000_000, 1));
        // Corrupt CRC: never trust a calibration we can't verify.
        let bad_crc = png_with(&[
            chunk_bad_crc(b"pHYs", &phys_data(100_000_000, 100_000_000, 1)),
            chunk(b"IEND", &[]),
        ]);
        assert_eq!(png_phys_nm_per_px(&bad_crc), None);
        // Wrong length is malformed — and must NOT fall through to a later,
        // well-formed pHYs, which is how a bad file could pick its own scale.
        let short = png_with(&[
            chunk(b"pHYs", &phys_data(1_000_000, 1_000_000, 1)[..8]),
            good.clone(),
            chunk(b"IEND", &[]),
        ]);
        assert_eq!(png_phys_nm_per_px(&short), None);
        // At most one pHYs per the spec; a duplicate is malformed even when
        // the first one was perfectly good.
        let dup = png_with(&[good.clone(), good.clone(), chunk(b"IEND", &[])]);
        assert_eq!(png_phys_nm_per_px(&dup), None);
        // ...including a duplicate after a legal aspect-only first chunk.
        let dup_after_aspect = png_with(&[
            chunk(b"pHYs", &phys_data(2, 1, 0)),
            good.clone(),
            chunk(b"IEND", &[]),
        ]);
        assert_eq!(png_phys_nm_per_px(&dup_after_aspect), None);
    }

    #[test]
    fn walk_is_bounded_and_skips_large_ancillary_chunks() {
        // A big iCCP/eXIf before pHYs is skipped by declared length, never
        // scanned byte-by-byte.
        let big = png_with(&[
            chunk(b"iCCP", &vec![0u8; 512 * 1024]),
            chunk(b"pHYs", &phys_data(100_000_000, 100_000_000, 1)),
            chunk(b"IEND", &[]),
        ]);
        assert_eq!(png_phys_nm_per_px(&big), Some((10.0, 10.0)));
        // Zero-length chunks still advance (12 bytes each), so this
        // terminates — on the budget, not on a scan of the whole file.
        let mut flood = SIG.to_vec();
        for _ in 0..20_000 {
            flood.extend_from_slice(&chunk(b"tEXt", &[]));
        }
        flood.extend_from_slice(&chunk(b"pHYs", &phys_data(100_000_000, 100_000_000, 1)));
        assert_eq!(png_phys_nm_per_px(&flood), None);
    }

    #[test]
    fn merge_fills_only_when_sidecar_did_not() {
        let png = isotropic_10nm();
        // No prior extras → pHYs fills, sidecar schema shape.
        let merged = merge_png_phys_scale(&png, "image/png", None, true).unwrap();
        let axes = &merged["physical_scale"]["axes"];
        assert_eq!(axes[0]["name"], "x");
        assert_eq!(axes[0]["nm_per_px"], 10.0);
        assert_eq!(axes[1]["name"], "y");
        assert_eq!(axes[1]["nm_per_px"], 10.0);
        assert_eq!(merged["physical_scale"]["unit"], "nm");
        // Sidecar already resolved → untouched (exact floats win).
        let sidecar = serde_json::json!({"physical_scale": {"axes": [], "unit": "nm"}});
        let kept = merge_png_phys_scale(&png, "image/png", Some(sidecar.clone()), true);
        assert_eq!(kept, Some(sidecar));
        // A present-but-null sidecar value still counts as resolved.
        let null_sidecar = serde_json::json!({ "physical_scale": null });
        let kept_null = merge_png_phys_scale(&png, "image/png", Some(null_sidecar.clone()), true);
        assert_eq!(kept_null, Some(null_sidecar));
        // Non-PNG mime → untouched.
        assert_eq!(merge_png_phys_scale(&png, "image/jpeg", None, true), None);
        // Unrelated extras are preserved alongside the new key.
        let other = serde_json::json!({ "page_count": 3 });
        let both = merge_png_phys_scale(&png, "image/png", Some(other), true).unwrap();
        assert_eq!(both["page_count"], 3);
        assert_eq!(both["physical_scale"]["axes"][0]["nm_per_px"], 10.0);
    }

    #[test]
    fn plugin_output_is_never_read_for_embedded_scale() {
        // Same bytes, same mime — only provenance differs. A plugin's blob
        // may be re-encoded, resized, or carry a rasterizer's own render DPI
        // (pdftoppm stamps 144), so its pHYs is not the subject's scale.
        let png = isotropic_10nm();
        assert!(merge_png_phys_scale(&png, "image/png", None, true).is_some());
        assert_eq!(merge_png_phys_scale(&png, "image/png", None, false), None);
    }

    #[test]
    fn anisotropic_phys_is_suppressed_not_half_rendered() {
        // The FE renders one bar from axes[0] by design, so unequal axes read
        // automatically off a file would silently label the image with its x
        // scale alone. Suppress rather than mislabel.
        let aniso = png_with(&[
            chunk(b"pHYs", &phys_data(100_000_000, 200_000_000, 1)),
            chunk(b"IEND", &[]),
        ]);
        assert_eq!(png_phys_nm_per_px(&aniso), Some((10.0, 5.0)));
        assert_eq!(merge_png_phys_scale(&aniso, "image/png", None, true), None);
        // ...but an explicitly anisotropic SIDECAR is a deliberate human act
        // and still passes through untouched.
        let sidecar = serde_json::json!({"physical_scale": {
            "axes": [{"name":"x","nm_per_px":10.0},{"name":"y","nm_per_px":5.0}], "unit":"nm"}});
        assert_eq!(
            merge_png_phys_scale(&aniso, "image/png", Some(sidecar.clone()), true),
            Some(sidecar)
        );
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

        let (out, scale) =
            downsample_oversize_raster("image/png", &png).expect("should downsample");
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
        // The returned scale is the linear factor that hit the cap, so it must
        // reproduce the observed shrink (used to rescale the scalebar, F1).
        assert!(scale > 0.0 && scale < 1.0, "downsample scale in (0,1): {scale}");
    }

    #[test]
    fn rescale_physical_scale_multiplies_every_axis() {
        // A 2×-downsample (scale=0.5 → factor=2.0) must double every axis's
        // per-original-px nm_per_px to per-served-px, preserving anisotropy.
        let extras = Some(serde_json::json!({
            "physical_scale": {
                "axes": [
                    {"name": "x", "nm_per_px": 2.0},
                    {"name": "z", "nm_per_px": 5.0}
                ],
                "unit": "nm"
            }
        }));
        let out = super::rescale_physical_scale(extras, 2.0).unwrap();
        let axes = out["physical_scale"]["axes"].as_array().unwrap();
        assert_eq!(axes[0]["nm_per_px"].as_f64().unwrap(), 4.0);
        assert_eq!(axes[1]["nm_per_px"].as_f64().unwrap(), 10.0);
        // unit + names untouched.
        assert_eq!(out["physical_scale"]["unit"], "nm");
        assert_eq!(axes[0]["name"], "x");
    }

    #[test]
    fn rescale_physical_scale_noop_without_scale() {
        // No physical_scale → returned unchanged (no panic, no spurious key).
        let extras = Some(serde_json::json!({"page": 3}));
        let out = super::rescale_physical_scale(extras, 2.0).unwrap();
        assert_eq!(out["page"], 3);
        assert!(out.get("physical_scale").is_none());
        // None in → None out.
        assert!(super::rescale_physical_scale(None, 2.0).is_none());
    }
}

#[cfg(test)]
mod capsule_comm_handle_tests {
    // Codex round finding 2/companion: `capsule_comm_handle` reads back
    // the handle `comm-join.sh`'s own derivation wrote into the
    // self-file the daemon pinned via `SOT_COMM_SELF_FILE` — the daemon
    // itself no longer synthesizes/persists a name, so this read-back is
    // the ONLY way a capsule row's handle is ever discovered (a capsule
    // has no tmux pane, so `resolve_handle`'s tmux-session match never
    // finds it).
    use super::capsule_comm_handle;

    struct EnvGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
        sot_comm_home: Option<std::ffi::OsString>,
        home: Option<std::ffi::OsString>,
        userprofile: Option<std::ffi::OsString>,
        sot_state_host: Option<std::ffi::OsString>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, val) in [
                ("SOT_COMM_HOME", &self.sot_comm_home),
                ("HOME", &self.home),
                ("USERPROFILE", &self.userprofile),
                ("SOT_STATE_HOST", &self.sot_state_host),
            ] {
                match val {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn guarded() -> EnvGuard {
        let serial = crate::paths::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        EnvGuard {
            sot_comm_home: std::env::var_os("SOT_COMM_HOME"),
            home: std::env::var_os("HOME"),
            userprofile: std::env::var_os("USERPROFILE"),
            sot_state_host: std::env::var_os("SOT_STATE_HOST"),
            _serial: serial,
        }
    }

    #[test]
    fn reads_first_line_of_the_pinned_self_file() {
        let _guard = guarded();
        let dir = std::env::temp_dir().join(format!(
            "sot-capsule-comm-handle-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let self_dir = dir.join("self");
        std::fs::create_dir_all(&self_dir).unwrap();
        std::env::set_var("SOT_COMM_HOME", &dir);
        std::env::set_var("SOT_STATE_HOST", "testhost");
        std::fs::write(
            self_dir.join("testhost__ws-myrepo-1a2b.txt"),
            "myrepo-testhost\nrepo=myrepo\nroot=/home/me/myrepo\n",
        )
        .unwrap();

        assert_eq!(capsule_comm_handle("ws-myrepo-1a2b"), "myrepo-testhost");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_when_self_file_does_not_exist() {
        let _guard = guarded();
        let dir = std::env::temp_dir().join(format!(
            "sot-capsule-comm-handle-missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("SOT_COMM_HOME", &dir);
        std::env::set_var("SOT_STATE_HOST", "testhost");

        assert_eq!(capsule_comm_handle("ws-never-joined-9f9f"), "");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod comm_row_owned_here_tests {
    // LU5d: `~/.sot-comm/registry.json` is ONE file shared by every host on
    // an NFS-homed cluster; two hosts can each run a session with the same
    // slug (e.g. `sot-be-x`). Without a host term in the match, one host's
    // `workspace.list` could bind to — and one host's destroy could delete —
    // another host's row on that session. `comm_row_owned_here` is the one
    // predicate both call sites (`resolve_handle` and
    // `remove_comm_agents_for_workspace`) now share.
    //
    // LU5d2 (Codex text round finding 3): a row with no `host` field is
    // UNKNOWN ownership, not a "legacy" free pass — comm-join.sh has
    // stamped `host` since the registry existed, so an absent field is not
    // evidence the row is ours.
    use super::{comm_row_owned_here, host_matches};
    use serde_json::json;

    fn row(tmux: &str, host: Option<&str>) -> serde_json::Value {
        match host {
            Some(h) => json!({ "tmux": tmux, "host": h }),
            None => json!({ "tmux": tmux }),
        }
    }

    #[test]
    fn matches_same_session_same_host() {
        let entry = row("sot-be-x:0.0", Some("kitt"));
        assert!(comm_row_owned_here(&entry, "sot-be-x", "kitt"));
    }

    #[test]
    fn rejects_same_session_other_host() {
        let entry = row("sot-be-x:0.0", Some("descent"));
        assert!(!comm_row_owned_here(&entry, "sot-be-x", "kitt"));
    }

    #[test]
    fn host_match_is_case_insensitive() {
        let entry = row("sot-be-x:0.0", Some("KITT"));
        assert!(comm_row_owned_here(&entry, "sot-be-x", "kitt"));
    }

    #[test]
    fn row_with_no_host_never_matches() {
        let entry = row("sot-be-x:0.0", None);
        assert!(!comm_row_owned_here(&entry, "sot-be-x", "kitt"));
        assert!(!comm_row_owned_here(&entry, "sot-be-x", "descent"));
    }

    #[test]
    fn empty_tmux_session_never_matches() {
        let entry = row("sot-be-x:0.0", Some("kitt"));
        assert!(!comm_row_owned_here(&entry, "", "kitt"));
    }

    #[test]
    fn different_session_never_matches() {
        let entry = row("sot-be-x:0.0", Some("kitt"));
        assert!(!comm_row_owned_here(&entry, "sot-be-y", "kitt"));
    }

    #[test]
    fn host_matches_rejects_absent_and_empty_host() {
        assert!(!host_matches(&json!({}), "kitt"));
        assert!(!host_matches(&json!({ "host": "" }), "kitt"));
    }

    #[test]
    fn host_matches_is_case_insensitive() {
        assert!(host_matches(&json!({ "host": "KITT" }), "kitt"));
        assert!(!host_matches(&json!({ "host": "descent" }), "kitt"));
    }
}

#[cfg(test)]
mod with_comm_registry_lock_panic_tests {
    // Coordinator hardening: `f` runs inside the caller's `spawn_blocking`,
    // which contains a panic (the awaiting task just sees a `JoinError`) —
    // but the OLD code released `.registry.lock` only on the normal return
    // path, so a panic mid-critical-section left it behind forever. Since
    // the fail-closed fix means nothing force-breaks it any more, every
    // subsequent writer — this daemon's own callers AND every
    // `comm-status.sh` hook on the shared home — would then wedge closed
    // permanently. `CommRegistryLockGuard`'s `Drop` must release on unwind
    // too.
    use super::with_comm_registry_lock;

    struct EnvGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
        sot_comm_home: Option<std::ffi::OsString>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.sot_comm_home {
                Some(v) => std::env::set_var("SOT_COMM_HOME", v),
                None => std::env::remove_var("SOT_COMM_HOME"),
            }
        }
    }

    fn guarded() -> EnvGuard {
        let serial = crate::paths::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        EnvGuard {
            sot_comm_home: std::env::var_os("SOT_COMM_HOME"),
            _serial: serial,
        }
    }

    #[test]
    fn a_panicking_closure_still_releases_the_lock() {
        let _guard = guarded();
        let dir = std::env::temp_dir().join(format!(
            "sot-comm-registry-lock-panic-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SOT_COMM_HOME", &dir);
        let lock_dir = dir.join(".registry.lock");

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_comm_registry_lock(std::time::Duration::from_secs(1), |_reg, _tmp| {
                panic!("boom — simulate a write that panics mid-critical-section");
            })
        }));

        assert!(result.is_err(), "the panic must propagate to the caller");
        assert!(
            !lock_dir.exists(),
            "the lock dir must be released even when `f` panics"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod remove_comm_agents_for_workspace_host_tests {
    // Same shared-registry scenario, exercised through the real prune path:
    // a destroy on host A must remove only host A's row on a session —
    // never host B's same-session row, and never a host-less row (LU5d2:
    // absent `host` is unknown ownership, not a free pass).
    use super::{remove_comm_agents_for_workspace, remove_comm_agents_for_workspace_bounded};

    struct EnvGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
        sot_comm_home: Option<std::ffi::OsString>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.sot_comm_home {
                Some(v) => std::env::set_var("SOT_COMM_HOME", v),
                None => std::env::remove_var("SOT_COMM_HOME"),
            }
        }
    }

    fn guarded() -> EnvGuard {
        let serial = crate::paths::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        EnvGuard {
            sot_comm_home: std::env::var_os("SOT_COMM_HOME"),
            _serial: serial,
        }
    }

    #[test]
    fn destroy_on_one_host_leaves_the_other_hosts_same_session_row() {
        let _guard = guarded();
        let dir = std::env::temp_dir().join(format!(
            "sot-comm-registry-host-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SOT_COMM_HOME", &dir);
        let registry_path = dir.join("registry.json");
        std::fs::write(
            &registry_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "agents": {
                    "kitt-be-x": {"tmux": "sot-be-x:0.0", "host": "kitt"},
                    "descent-be-x": {"tmux": "sot-be-x:0.0", "host": "descent"},
                    "hostless-be-x": {"tmux": "sot-be-x:0.0"},
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let removed = remove_comm_agents_for_workspace("sot-be-x", "", "kitt");
        assert_eq!(removed, vec!["kitt-be-x"]);

        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
        let agents = after.get("agents").unwrap().as_object().unwrap();
        assert!(!agents.contains_key("kitt-be-x"));
        assert!(
            agents.contains_key("descent-be-x"),
            "host B's row must survive host A's destroy"
        );
        assert!(
            agents.contains_key("hostless-be-x"),
            "a row with no host is unknown ownership, never ours — it must survive too"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn by_name_requires_host() {
        // The `by_name` fallback (for a not-yet-joined `spawning` row whose
        // `tmux` is still "") matches on the caller-supplied `agent_name`
        // alone before LU5d2 — a row on another host that happens to share
        // that handle string must survive.
        let _guard = guarded();
        let dir = std::env::temp_dir().join(format!(
            "sot-comm-registry-by-name-host-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SOT_COMM_HOME", &dir);
        let registry_path = dir.join("registry.json");
        std::fs::write(
            &registry_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "agents": {
                    "same-name": {"tmux": "", "host": "descent"},
                }
            }))
            .unwrap(),
        )
        .unwrap();

        // No live tmux row for this session, so only the `by_name` term is in
        // play; the stored handle matches, but the row's host does not.
        let removed = remove_comm_agents_for_workspace("sot-be-x", "same-name", "kitt");
        assert!(removed.is_empty());

        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
        assert!(after
            .get("agents")
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("same-name"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn contended_lock_gives_up_bounded_and_leaves_registry_unchanged() {
        // Mirrors `clear_comm_unread_tests`'s own contended-lock test: the
        // prune used to force-break a stale-looking lock after its bound
        // (200×50ms); since PR #148 F2 fail-closed is the rule the SHELL
        // side already lives by, and this proves the Rust prune now follows
        // it too — via a short bound (`remove_comm_agents_for_workspace_bounded`)
        // so the test doesn't have to wait out the real
        // `COMM_PRUNE_LOCK_BOUND`.
        let _guard = guarded();
        let dir = std::env::temp_dir().join(format!(
            "sot-comm-registry-prune-lock-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SOT_COMM_HOME", &dir);
        let registry_path = dir.join("registry.json");
        std::fs::write(
            &registry_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "agents": {
                    "kitt-be-x": {"tmux": "sot-be-x:0.0", "host": "kitt"},
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let before = std::fs::read(&registry_path).unwrap();
        // Pre-create the lock dir so the mkdir-spinlock can never acquire it.
        let lock_dir = dir.join(".registry.lock");
        std::fs::create_dir(&lock_dir).unwrap();

        let bound = std::time::Duration::from_millis(150);
        let start = std::time::Instant::now();
        let removed = remove_comm_agents_for_workspace_bounded("sot-be-x", "", "kitt", bound);
        let elapsed = start.elapsed();

        assert!(removed.is_empty(), "a contended lock must prune nothing");
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "bounded spin must not stall the caller: took {elapsed:?}"
        );
        let after = std::fs::read(&registry_path).unwrap();
        assert_eq!(before, after, "a contended lock must fail closed with no write");
        assert!(
            lock_dir.is_dir(),
            "fail-closed means the pre-existing lock dir is left exactly as found — never force-broken"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod clear_comm_unread_tests {
    // ADR 0044 "Viewing clears blue": `workspace.activate { read: true }`
    // flips a `done` row to `idle` and touches NOTHING else. Same
    // guarded()/SOT_COMM_HOME pattern as
    // `remove_comm_agents_for_workspace_host_tests` above —
    // `clear_comm_unread` shares `with_comm_registry_lock` (the lock
    // protocol) and `comm_handle_for_workspace` (the row-binding rule,
    // tmux and capsule alike) with that function and `handle_workspace_list`
    // respectively, so these tests also stand in for both: neither has any
    // other caller-facing behaviour beyond what binds/writes a row here.
    use super::{clear_comm_unread, Workspace};

    struct EnvGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
        sot_comm_home: Option<std::ffi::OsString>,
        sot_state_host: Option<std::ffi::OsString>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.sot_comm_home {
                Some(v) => std::env::set_var("SOT_COMM_HOME", v),
                None => std::env::remove_var("SOT_COMM_HOME"),
            }
            match &self.sot_state_host {
                Some(v) => std::env::set_var("SOT_STATE_HOST", v),
                None => std::env::remove_var("SOT_STATE_HOST"),
            }
        }
    }

    fn guarded() -> EnvGuard {
        let serial = crate::paths::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        EnvGuard {
            sot_comm_home: std::env::var_os("SOT_COMM_HOME"),
            sot_state_host: std::env::var_os("SOT_STATE_HOST"),
            _serial: serial,
        }
    }

    fn temp_home(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "sot-clear-comm-unread-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn write_registry(dir: &std::path::Path, agents: serde_json::Value) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        std::env::set_var("SOT_COMM_HOME", dir);
        let registry_path = dir.join("registry.json");
        std::fs::write(
            &registry_path,
            serde_json::to_vec_pretty(&serde_json::json!({ "agents": agents })).unwrap(),
        )
        .unwrap();
        registry_path
    }

    // A `"tmux"`-runtime workspace with `tmux_session = "sot-be-<label>"`
    // (matching `Workspace::from_label`'s own convention, so a fixture's
    // registry `"tmux": "sot-be-<label>:0.0"` binds via the live-occupant
    // match, same as production) and the given stored `agent_name`.
    fn mk_ws(label: &str, agent_name: &str) -> Workspace {
        let mut ws = Workspace::from_label(
            label,
            std::path::PathBuf::from("/p"),
            false,
            "none".into(),
            agent_name.to_string(),
            String::new(),
        );
        // These rows are tmux rows on every platform: a label-built
        // workspace defaults to "capsule" on Windows, which would route the
        // clear through the self-file branch instead of the seeded tmux row.
        ws.runtime = "tmux".to_string();
        ws
    }

    #[test]
    fn done_row_for_this_host_flips_to_idle_summary_and_status_at_untouched() {
        let _guard = guarded();
        let dir = temp_home("done");
        let registry_path = write_registry(
            &dir,
            serde_json::json!({
                "kitt-be-x": {
                    "tmux": "sot-be-x:0.0",
                    "host": "kitt",
                    "state": "done",
                    "summary": "probe summary",
                    "status_at": "2026-09-08T00:00:00Z",
                    "last_seen": "2026-09-08T00:00:01Z",
                },
            }),
        );

        clear_comm_unread(&mk_ws("x", ""), "kitt");

        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
        let row = &after["agents"]["kitt-be-x"];
        assert_eq!(row["state"], "idle");
        assert_eq!(row["summary"], "probe summary", "summary must survive the clear");
        assert_eq!(
            row["status_at"], "2026-09-08T00:00:00Z",
            "status_at must be untouched — reading is not activity"
        );
        assert_eq!(row["last_seen"], "2026-09-08T00:00:01Z");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_session_row_on_another_host_survives_untouched() {
        let _guard = guarded();
        let dir = temp_home("otherhost");
        let registry_path = write_registry(
            &dir,
            serde_json::json!({
                "descent-be-x": {
                    "tmux": "sot-be-x:0.0",
                    "host": "descent",
                    "state": "done",
                    "summary": "not yours",
                    "status_at": "2026-09-08T00:00:00Z",
                },
            }),
        );
        let before = std::fs::read(&registry_path).unwrap();

        clear_comm_unread(&mk_ws("x", ""), "kitt");

        let after = std::fs::read(&registry_path).unwrap();
        assert_eq!(before, after, "a foreign host's row must never be touched");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_done_states_are_never_touched() {
        let _guard = guarded();
        for state in ["blocked", "waiting", "working", "idle"] {
            let dir = temp_home(&format!("state-{state}"));
            let registry_path = write_registry(
                &dir,
                serde_json::json!({
                    "kitt-be-x": {
                        "tmux": "sot-be-x:0.0",
                        "host": "kitt",
                        "state": state,
                        "summary": "unchanged",
                        "status_at": "2026-09-08T00:00:00Z",
                    },
                }),
            );
            let before = std::fs::read(&registry_path).unwrap();

            clear_comm_unread(&mk_ws("x", ""), "kitt");

            let after = std::fs::read(&registry_path).unwrap();
            assert_eq!(before, after, "state {state} must never be rewritten");

            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn missing_registry_is_a_silent_no_op() {
        let _guard = guarded();
        let dir = temp_home("missing");
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SOT_COMM_HOME", &dir);
        // No registry.json written at all.

        clear_comm_unread(&mk_ws("x", "agent"), "kitt");
        assert!(!dir.join("registry.json").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_registry_is_a_silent_no_op() {
        let _guard = guarded();
        let dir = temp_home("malformed");
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SOT_COMM_HOME", &dir);
        let registry_path = dir.join("registry.json");
        std::fs::write(&registry_path, b"not json{{{").unwrap();
        let before = std::fs::read(&registry_path).unwrap();

        clear_comm_unread(&mk_ws("x", "agent"), "kitt");

        let after = std::fs::read(&registry_path).unwrap();
        assert_eq!(before, after);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn contended_lock_gives_up_bounded_and_leaves_registry_unchanged() {
        let _guard = guarded();
        let dir = temp_home("locked");
        let registry_path = write_registry(
            &dir,
            serde_json::json!({
                "kitt-be-x": {
                    "tmux": "sot-be-x:0.0",
                    "host": "kitt",
                    "state": "done",
                    "summary": "probe summary",
                    "status_at": "2026-09-08T00:00:00Z",
                },
            }),
        );
        let before = std::fs::read(&registry_path).unwrap();
        // Pre-create the lock dir so the mkdir-spinlock inside
        // `clear_comm_unread` can never acquire it.
        std::fs::create_dir(dir.join(".registry.lock")).unwrap();

        // No wall-clock assertion: the spin is bounded by a fixed iteration
        // count, and a loaded CI runner (the macOS leg took 2.6 s for the
        // ~1 s spin) turns any elapsed-time gate into a flake. The property
        // under test is fail-closed: the registry is untouched.
        clear_comm_unread(&mk_ws("x", ""), "kitt");
        let after = std::fs::read(&registry_path).unwrap();
        assert_eq!(before, after, "a contended lock must fail closed with no write");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn capsule_row_resolved_via_self_file_clears_to_idle() {
        // A capsule workspace has no tmux pane and (unless explicitly
        // requested) no stored `agent_name` either — the ONLY way to its
        // registry row is `capsule_comm_handle`'s self-file read-back.
        // `comm_handle_for_workspace` must try that path FIRST for a
        // capsule row, same as `handle_workspace_list` does, or these rows
        // — the ones that actually pile up blue for a capsule-only user —
        // would never clear.
        let _guard = guarded();
        let dir = temp_home("capsule");
        let registry_path = write_registry(
            &dir,
            serde_json::json!({
                "capsule-handle-x": {
                    "tmux": "",
                    "host": "kitt",
                    "state": "done",
                    "summary": "probe summary",
                    "status_at": "2026-09-08T00:00:00Z",
                },
            }),
        );
        std::env::set_var("SOT_STATE_HOST", "kitt");

        let mut ws = mk_ws("capsuleprobe", "");
        ws.runtime = "capsule".to_string();
        let self_dir = dir.join("self");
        std::fs::create_dir_all(&self_dir).unwrap();
        std::fs::write(
            self_dir.join(format!("kitt__{}.txt", ws.workspace_id)),
            "capsule-handle-x\n",
        )
        .unwrap();

        clear_comm_unread(&ws, "kitt");

        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
        let row = &after["agents"]["capsule-handle-x"];
        assert_eq!(row["state"], "idle");
        assert_eq!(row["summary"], "probe summary");
        assert_eq!(row["status_at"], "2026-09-08T00:00:00Z");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn capsule_row_with_empty_agent_name_and_no_self_file_is_a_no_op() {
        // The fallback half of `comm_handle_for_workspace`'s capsule arm:
        // no self-file AND an empty stored `agent_name` resolves to an
        // empty handle, same as the tmux path's "nothing to bind to".
        let _guard = guarded();
        let dir = temp_home("capsule-unbound");
        let registry_path = write_registry(
            &dir,
            serde_json::json!({
                "someone-else": {
                    "tmux": "",
                    "host": "kitt",
                    "state": "done",
                    "summary": "not yours",
                    "status_at": "2026-09-08T00:00:00Z",
                },
            }),
        );
        std::env::set_var("SOT_STATE_HOST", "kitt");
        let before = std::fs::read(&registry_path).unwrap();

        let mut ws = mk_ws("capsuleprobe2", "");
        ws.runtime = "capsule".to_string();
        // No self-file written at all.

        clear_comm_unread(&ws, "kitt");

        let after = std::fs::read(&registry_path).unwrap();
        assert_eq!(before, after, "an unbound capsule row must never fall through to an unrelated handle");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod workspace_activate_read_tests {
    // End-to-end through the real async handler: `read: true` clears a
    // `done` row via the SAME workspace binding `workspace.list` uses;
    // `read: false` (an old frontend, or any programmatic switch) leaves
    // the registry untouched. The ack echoes the canonical workspace_id
    // regardless of what the clear did.
    use super::*;

    struct EnvGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
        sot_comm_home: Option<std::ffi::OsString>,
        sot_state_host: Option<std::ffi::OsString>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.sot_comm_home {
                Some(v) => std::env::set_var("SOT_COMM_HOME", v),
                None => std::env::remove_var("SOT_COMM_HOME"),
            }
            match &self.sot_state_host {
                Some(v) => std::env::set_var("SOT_STATE_HOST", v),
                None => std::env::remove_var("SOT_STATE_HOST"),
            }
        }
    }

    fn guarded() -> EnvGuard {
        let serial = crate::paths::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        EnvGuard {
            sot_comm_home: std::env::var_os("SOT_COMM_HOME"),
            sot_state_host: std::env::var_os("SOT_STATE_HOST"),
            _serial: serial,
        }
    }

    fn seed_workspace(label: &str) -> (Workspaces, String, String) {
        let reg = Workspaces::new();
        let mut ws = Workspace::from_label(
            label,
            std::path::PathBuf::from("/p/x"),
            false,
            "none".into(),
            String::new(),
            String::new(),
        );
        // A tmux row on every platform (Windows defaults a label-built
        // workspace to "capsule").
        ws.runtime = "tmux".to_string();
        let id = ws.workspace_id.clone();
        let tmux_session = ws.tmux_session.clone();
        reg.insert(ws);
        (reg, id, tmux_session)
    }

    // A `runtime = "capsule"` row: no tmux pane, no stored `agent_name`
    // (the owner's actual capsule sessions — the ones piling up blue).
    fn seed_capsule_workspace(label: &str) -> (Workspaces, String) {
        let reg = Workspaces::new();
        let mut ws = Workspace::from_label(
            label,
            std::path::PathBuf::from("/p/x"),
            false,
            "none".into(),
            String::new(),
            String::new(),
        );
        ws.runtime = "capsule".to_string();
        let id = ws.workspace_id.clone();
        reg.insert(ws);
        (reg, id)
    }

    async fn activate(
        workspaces: &Workspaces,
        workspace_id: &str,
        read: bool,
    ) -> serde_json::Value {
        let payload = serde_json::json!({ "workspace_id": workspace_id, "read": read });
        let out = handle_workspace_activate(1, payload, workspaces)
            .await
            .expect("handler must not error");
        assert_eq!(
            out.len(),
            1,
            "workspace.activate always answers with exactly one frame"
        );
        out[0].0.payload.clone()
    }

    #[tokio::test]
    async fn read_true_clears_the_row_read_false_does_not() {
        let _guard = guarded();
        let dir = std::env::temp_dir().join(format!(
            "sot-workspace-activate-read-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SOT_COMM_HOME", &dir);
        std::env::set_var("SOT_STATE_HOST", "kitt");
        let registry_path = dir.join("registry.json");

        let (reg, id, tmux_session) = seed_workspace("activate-read-x");
        std::fs::write(
            &registry_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "agents": {
                    "kitt-activate-read-x": {
                        "tmux": format!("{tmux_session}:0.0"),
                        "host": "kitt",
                        "state": "done",
                        "summary": "probe summary",
                        "status_at": "2026-09-08T00:00:00Z",
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        // read: false — untouched, ack still names the resolved workspace.
        let ack = activate(&reg, &id, false).await;
        assert_eq!(
            ack.get("workspace_id").and_then(|v| v.as_str()),
            Some(id.as_str())
        );
        let after_false: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
        assert_eq!(after_false["agents"]["kitt-activate-read-x"]["state"], "done");

        // read: true — clears it; summary and status_at survive.
        let ack = activate(&reg, &id, true).await;
        assert_eq!(
            ack.get("workspace_id").and_then(|v| v.as_str()),
            Some(id.as_str())
        );
        let after_true: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
        let row = &after_true["agents"]["kitt-activate-read-x"];
        assert_eq!(row["state"], "idle");
        assert_eq!(row["summary"], "probe summary");
        assert_eq!(row["status_at"], "2026-09-08T00:00:00Z");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn capsule_row_read_true_clears_via_self_file() {
        // The manager-review case: a capsule workspace's row is found ONLY
        // through its pinned self-file (`capsule_comm_handle`), never
        // through a tmux match or a stored `agent_name` (both empty/absent
        // here) — this is what the owner's actual local capsule sessions
        // look like, so this path clearing is the whole point of the fix.
        let _guard = guarded();
        let dir = std::env::temp_dir().join(format!(
            "sot-workspace-activate-capsule-read-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SOT_COMM_HOME", &dir);
        std::env::set_var("SOT_STATE_HOST", "kitt");
        let registry_path = dir.join("registry.json");

        let (reg, id) = seed_capsule_workspace("activate-capsule-x");

        // The self-file `capsule_comm_handle` reads back — pinned under the
        // SAME scratch SOT_COMM_HOME, naming a handle that has no tmux row
        // and no relation to the workspace's (empty) stored `agent_name`.
        let self_dir = dir.join("self");
        std::fs::create_dir_all(&self_dir).unwrap();
        std::fs::write(
            self_dir.join(format!("kitt__{id}.txt")),
            "kitt-activate-capsule-x\n",
        )
        .unwrap();

        std::fs::write(
            &registry_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "agents": {
                    "kitt-activate-capsule-x": {
                        "tmux": "",
                        "host": "kitt",
                        "state": "done",
                        "summary": "capsule probe summary",
                        "status_at": "2026-09-08T00:00:00Z",
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let ack = activate(&reg, &id, true).await;
        assert_eq!(
            ack.get("workspace_id").and_then(|v| v.as_str()),
            Some(id.as_str())
        );
        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
        let row = &after["agents"]["kitt-activate-capsule-x"];
        assert_eq!(row["state"], "idle");
        assert_eq!(row["summary"], "capsule probe summary");
        assert_eq!(row["status_at"], "2026-09-08T00:00:00Z");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod agent_str_host_filter_tests {
    // LU5d2: `handle_workspace_list`'s `agent_str` closure read
    // `.agents[handle].<field>` by handle name alone. The handle it's
    // called with here is bound via the stored `agent_name` fallback (no
    // live tmux row for the session) — a caller-supplied name, not proof
    // of ownership — so a same-named row stamped by ANOTHER host on the
    // shared registry must read as empty, never leak its
    // summary/status_at into this host's `workspace.list`.
    use super::*;

    struct EnvGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
        sot_comm_home: Option<std::ffi::OsString>,
        sot_state_host: Option<std::ffi::OsString>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, val) in [
                ("SOT_COMM_HOME", &self.sot_comm_home),
                ("SOT_STATE_HOST", &self.sot_state_host),
            ] {
                match val {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn guarded() -> EnvGuard {
        let serial = crate::paths::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        EnvGuard {
            sot_comm_home: std::env::var_os("SOT_COMM_HOME"),
            sot_state_host: std::env::var_os("SOT_STATE_HOST"),
            _serial: serial,
        }
    }

    #[tokio::test]
    async fn agent_str_never_reads_another_hosts_same_named_row() {
        let _guard = guarded();
        let dir = std::env::temp_dir().join(format!(
            "sot-agent-str-host-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SOT_COMM_HOME", &dir);
        std::env::set_var("SOT_STATE_HOST", "kitt");
        std::fs::write(
            dir.join("registry.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "agents": {
                    "same-name": {
                        "tmux": "",
                        "host": "descent",
                        "state": "working",
                        "summary": "leaked",
                        "status_at": "2026-01-01T00:00:00Z"
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let workspaces = Workspaces::new();
        let ws = Workspace::from_label(
            "myws",
            std::path::PathBuf::from("/p/myws"),
            false,
            "none".into(),
            "same-name".into(),
            String::new(),
        );
        workspaces.insert(ws);

        let out = handle_workspace_list(1, json!({}), &workspaces)
            .await
            .expect("handler must not error");
        let payload = out[0].0.payload.clone();
        let entries = payload.get("workspaces").unwrap().as_array().unwrap();
        let entry = entries
            .iter()
            .find(|e| e.get("slug").and_then(|v| v.as_str()) == Some("myws"))
            .expect("the workspace we inserted must be in the list");
        assert_eq!(
            entry.get("agent_summary").and_then(|v| v.as_str()),
            Some("")
        );
        assert_eq!(
            entry.get("agent_status_at").and_then(|v| v.as_str()),
            Some("")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod workspace_destroy_default_row_tests {
    // Default-row end-run: a default TMUX row keeps the flat refusal
    // (no run to end). A default CAPSULE row (ADR 0043 decision 22: on
    // any host the capsule runtime compiles for, not just Windows) ends
    // its run and keeps the row, reporting the outcome in
    // `WorkspaceDestroyRes::kept` — but only when CONFIRMED
    // (`Removable`); `Kept` still returns the typed
    // `capsule_end_not_reached` error, never the flat tmux-style refusal.
    use super::*;

    // Isolates `crate::workspaces::save`'s config dir for the one test
    // below that (unlike every other test in this module) runs the
    // reset+persist path for real -- same technique as `workspaces.rs`'s
    // own `env_guarded`, serialized under the crate-wide lock so this
    // never races another module's env-mutating test. Every caller is now
    // `#[cfg(any(windows, target_os = "linux"))]` (the absence proof
    // `seed_provably_unheld_state_dir` builds only means anything there),
    // so this whole cluster is unused dead code elsewhere -- allowed
    // rather than gating the struct/fns themselves and losing the single
    // definition every platform's `cargo check` still type-checks.
    #[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
    struct EnvGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
        xdg_config_home: Option<std::ffi::OsString>,
        // Added alongside `seed_provably_unheld_state_dir` below (ADR
        // 0043 decision 33, Codex review, 2026-09-11): the tests that used
        // to lean on `mark_capsule_terminal`'s now-deleted unguarded fast
        // path instead point `sot_log::state_dir::sot_state_dir()` at a
        // scratch root so `destroy_capsule_workspace`'s real guarded path
        // finds a hermetic, provably-absent state dir there. Both vars are
        // saved/restored on every platform even though `sot_state_dir()`
        // only ever reads ONE of them per platform (`XDG_STATE_HOME` on
        // Unix, `LOCALAPPDATA` on Windows — see `pin_local_state_root`
        // below): a fixture that pinned only `XDG_STATE_HOME` used to be
        // silently ignored by the resolver on Windows CI, which is exactly
        // how the terminal/confirmed-end tests below used to fail there —
        // the fixture built a state dir nobody ever looked at.
        xdg_state_home: Option<std::ffi::OsString>,
        localappdata: Option<std::ffi::OsString>,
        sot_state_host: Option<std::ffi::OsString>,
        sot_comm_home: Option<std::ffi::OsString>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, val) in [
                ("XDG_CONFIG_HOME", &self.xdg_config_home),
                ("XDG_STATE_HOME", &self.xdg_state_home),
                ("LOCALAPPDATA", &self.localappdata),
                ("SOT_STATE_HOST", &self.sot_state_host),
                ("SOT_COMM_HOME", &self.sot_comm_home),
            ] {
                match val {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    #[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
    fn env_guarded() -> EnvGuard {
        let serial = crate::paths::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        EnvGuard {
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
            xdg_state_home: std::env::var_os("XDG_STATE_HOME"),
            localappdata: std::env::var_os("LOCALAPPDATA"),
            sot_state_host: std::env::var_os("SOT_STATE_HOST"),
            sot_comm_home: std::env::var_os("SOT_COMM_HOME"),
            _serial: serial,
        }
    }

    /// Points wherever `sot_log::state_dir::sot_state_dir()` ACTUALLY reads
    /// on this platform (`LOCALAPPDATA` on Windows, `XDG_STATE_HOME`
    /// elsewhere — that function's own doc has the precedence) at `dir`,
    /// then returns the root by calling that SAME resolver rather than
    /// hand-building `dir.join("sot")` here — the one seam every fixture
    /// below must agree with `destroy_capsule_workspace` about. Caller
    /// holds an `EnvGuard` (`env_guarded()`) first so both vars this may
    /// touch are restored on drop, and `dir` need not exist yet — nothing
    /// here creates it; `state_dir_missing` fixtures rely on exactly that.
    #[cfg(any(windows, target_os = "linux"))]
    fn pin_local_state_root(dir: &std::path::Path) -> std::path::PathBuf {
        #[cfg(windows)]
        std::env::set_var("LOCALAPPDATA", dir);
        #[cfg(not(windows))]
        std::env::set_var("XDG_STATE_HOME", dir);
        sot_log::state_dir::sot_state_dir()
            .expect("state root must resolve once pinned to a scratch dir")
    }

    /// Builds a hermetic on-disk state dir that `destroy_capsule_
    /// workspace`'s real guarded path (ADR 0043 decision 33) will
    /// independently prove BOTH halves of the destroy proof absent for —
    /// nothing ever holds `supervisor.lock`, and a published pointer
    /// names a voyage whose own `writer.lock` exists and is free — so
    /// `end_run`'s `Unheld` arm reports `Removable` with no live process
    /// anywhere. Caller must first call `pin_local_state_root` (under
    /// `env_guarded`) and pass ITS return value as `state_root` — the
    /// resolved root `sot_log::state_dir::sot_state_dir()` itself reports,
    /// never a hand-built path, so this fixture lands exactly where
    /// `destroy_capsule_workspace` (via `state_dir_for`) actually looks.
    /// Replaces this module's old reliance on `mark_capsule_terminal`'s
    /// deleted unguarded fast path (Codex review, 2026-09-11: that path
    /// returned `Removable` on the daemon's own say-so alone, with no
    /// proof at all) — same technique `capsule_workspace`'s own
    /// absence-proof unit tests use. Really `#[cfg]`-gated, not merely
    /// `allow(dead_code)`: the body reaches `sot_log::supervisor`, a
    /// module gated `#![cfg(any(windows, target_os = "linux"))]` at its
    /// own root (`log/src/supervisor.rs`) — nonexistent on every other
    /// host, not merely unused.
    #[cfg(any(windows, target_os = "linux"))]
    fn seed_provably_unheld_state_dir(state_root: &std::path::Path, workspace_id: &str) {
        let state_dir = crate::capsule_workspace::state_dir_for(state_root, workspace_id);
        std::fs::create_dir_all(&state_dir).expect("create the fake state dir");
        let voyage_id = "a1b2c3d4-e5f6-4890-9abc-def012345678";
        sot_log::pointer::publish(&state_dir, voyage_id).expect("publish the pointer");
        let voyage_root = sot_log::supervisor::voyage_root_path(&state_dir, voyage_id);
        std::fs::create_dir_all(&voyage_root).expect("voyage root");
        std::fs::write(voyage_root.join("writer.lock"), b"").expect("writer.lock file");
    }

    fn seed_default(runtime: &str) -> (Workspaces, String) {
        let reg = Workspaces::new();
        let mut ws = Workspace::from_label(
            "local",
            std::path::PathBuf::from("/p/local"),
            false,
            "none".into(),
            String::new(),
            String::new(),
        );
        ws.runtime = runtime.to_string();
        let id = ws.workspace_id.clone();
        reg.insert(ws);
        reg.set_default(&id);
        (reg, id)
    }

    /// Same as `seed_default("capsule")` but with a carried-over agent —
    /// the field shape (owner once started an agent in this row before
    /// the "nothing runs in the anchor" rule existed) that the reset in
    /// `end_default_row_run` exists to unstick.
    fn seed_default_with_agent(agent: &str, agent_name: &str) -> (Workspaces, String, String) {
        let reg = Workspaces::new();
        let mut ws = Workspace::from_label(
            "local",
            std::path::PathBuf::from("/p/local"),
            true,
            agent.to_string(),
            agent_name.to_string(),
            String::new(),
        );
        ws.runtime = "capsule".to_string();
        let id = ws.workspace_id.clone();
        let slug = ws.slug.clone();
        reg.insert(ws);
        reg.set_default(&id);
        (reg, id, slug)
    }

    async fn destroy(workspaces: &Workspaces, workspace_id: &str) -> serde_json::Value {
        let session = Session::new();
        let (tx, _rx) = broadcast::channel(16);
        let payload = json!({ "workspace_id": workspace_id });
        let out = handle_workspace_destroy(1, payload, &session, workspaces, &tx)
            .await
            .expect("handler must not error");
        assert_eq!(
            out.len(),
            1,
            "workspace.destroy always answers with exactly one frame"
        );
        out[0].0.payload.clone()
    }

    fn assert_refused(payload: &serde_json::Value) {
        assert_eq!(
            payload.get("code").and_then(|v| v.as_str()),
            Some("default_workspace_not_destroyable")
        );
        assert!(payload.get("error").is_some());
        assert!(payload.get("kept").is_none());
    }

    #[tokio::test]
    async fn default_tmux_workspace_is_still_refused() {
        let (reg, id) = seed_default("tmux");
        let payload = destroy(&reg, &id).await;
        assert_refused(&payload);
        // Untouched either way — the refusal never removes anything.
        assert!(reg.resolve(Some(&id)).is_some());
    }

    // ADR 0043 decision 22: capsule support is no longer Windows-only, so
    // a default row explicitly marked "capsule" (a hand-edited toml, or
    // later the bridge) now takes the SAME real end-run path a Windows
    // one always did — never the flat tmux-style refusal
    // (`default_workspace_not_destroyable`). Nothing is actually running
    // behind this row in-process, so the real attempt cannot reach a
    // live lane and the row is KEPT (unconfirmed) either way. The exact
    // reason is platform-dependent (ADR 0043 decision 33): on Windows and
    // Linux, `destroy_capsule_workspace`'s real path finds no state dir
    // at all on disk for this synthetic, never-spawned row and reports
    // the SPECIFIC `state_dir_missing` proof rather than the generic
    // "lane unreachable" catch-all; where the capsule runtime doesn't
    // compile at all (e.g. macOS), the portable fallback arm reports the
    // generic code instead — the outward `Kept` shape is the same either
    // way, only the code differs.
    // Pinned hermetic (Codex review, 2026-09-11): this test used to read
    // `sot_log::state_dir::sot_state_dir()`'s REAL, unpinned environment —
    // fine on a dev box whose shell always exports a stable, qualified
    // `XDG_STATE_HOME`, but on CI (nothing exported) it read whatever the
    // ambient state root happened to resolve to, unguarded against every
    // OTHER test in this module that mutates the SAME process-global vars
    // under `env_guarded()`'s lock. Pinning to a fresh, never-created
    // scratch root — same resolver, same lock — makes "no state dir on
    // disk for this workspace" true by construction, not by luck.
    #[tokio::test]
    async fn default_capsule_workspace_takes_the_real_end_run_path_not_the_flat_refusal() {
        let _guard = env_guarded();
        #[cfg(any(windows, target_os = "linux"))]
        let scratch = std::env::temp_dir().join(format!(
            "sot-ws-destroy-missing-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // Nothing is created under `scratch` — the point of this test is
        // that the resolved state dir does not exist on disk at all.
        #[cfg(any(windows, target_os = "linux"))]
        pin_local_state_root(&scratch);

        let (reg, id) = seed_default("capsule");
        let payload = destroy(&reg, &id).await;
        assert_ne!(
            payload.get("code").and_then(|v| v.as_str()),
            Some("default_workspace_not_destroyable"),
            "a capsule default row must not get the flat tmux-style refusal: {payload:?}"
        );
        #[cfg(any(windows, target_os = "linux"))]
        let expected_code = "state_dir_missing";
        #[cfg(not(any(windows, target_os = "linux")))]
        let expected_code = "capsule_end_not_reached";
        assert_eq!(
            payload.get("code").and_then(|v| v.as_str()),
            Some(expected_code),
            "payload: {payload:?}"
        );
        assert!(reg.resolve(Some(&id)).is_some(), "the default row is never removed either way");

        #[cfg(any(windows, target_os = "linux"))]
        let _ = std::fs::remove_dir_all(&scratch);
    }

    // ADR 0043 decision 33 (BLOCKER, Codex review, 2026-09-11): a row the
    // watchdog already marked `capsule_terminal` no longer takes an
    // unguarded shortcut straight to `Removable` -- that deleted fast
    // path returned "removable" on the daemon's own say-so alone,
    // bypassing the guard AND the fence/leg absence proof, so a leg the
    // watchdog's own exhausted restart budget (or a failed adoption) left
    // running behind a `Terminal` authority could have been orphaned. A
    // terminal row now goes through the SAME guarded resume/end_run path
    // as every other row: `resume_locked`'s own internal `is_capsule_
    // terminal` check still reports that phase without a live round trip
    // (no wasted probe against an authority that is almost always
    // already gone), but `end_run`'s fresh `query_status` -- naturally
    // unreachable here, nothing is listening -- then reaches the SAME
    // independent absence proof every other row does, hermetically
    // reproduced via `seed_provably_unheld_state_dir`. Called directly
    // (not through `handle_workspace_destroy`) to stay hermetic -- the
    // full wire path also removes on-disk tomls under the real config
    // dir, which is not safe to exercise from an in-process unit test.
    //
    // Gated (unlike the deleted portable shortcut this replaces): the
    // absence proof this now exercises lives entirely inside
    // `destroy_capsule_workspace`'s `#[cfg(any(windows, target_os =
    // "linux"))]` arm -- every other host takes the unconditional `Kept`
    // fallback regardless of any on-disk fixture.
    #[tokio::test]
    #[cfg(any(windows, target_os = "linux"))]
    async fn a_capsule_workspace_marked_terminal_still_needs_the_absence_proof() {
        let _guard = env_guarded();
        let scratch = std::env::temp_dir().join(format!(
            "sot-ws-destroy-terminal-proof-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let state_root = pin_local_state_root(&scratch);

        let (reg, id) = seed_default("capsule");
        reg.mark_capsule_terminal(&id);
        seed_provably_unheld_state_dir(&state_root, &id);

        // `/p/local`/`"local"` are placeholders (`resume_locked`'s own
        // `is_capsule_terminal` check returns before any agent argv is
        // resolved) -- only `state_root`'s on-disk fixture is real.
        let (outcome, held) = destroy_capsule_workspace(
            &id,
            "test reason",
            "none",
            "",
            "local",
            std::path::Path::new("/p/local"),
            &reg,
        )
        .await;
        // The guard IS taken now (Codex review: the deleted fast path's
        // `None` bypassed it) -- dropped once this proof has run.
        assert!(held.is_some(), "a terminal row must take the same row guard every other row does");
        match outcome {
            CapsuleDestroyOutcome::Removable(_) => {}
            CapsuleDestroyOutcome::Kept { detail } => {
                panic!(
                    "a terminal row with both halves of the absence proof independently absent \
                     must be Removable: {detail}"
                );
            }
        }

        let _ = std::fs::remove_dir_all(&scratch);
    }

    // The full field defect this lane fixes: a default row carrying an
    // agent from before the anchor rule, whose run is CONFIRMED ended,
    // must have its `agent`/`agent_name` reset to the inert-anchor
    // shape, that reset persisted to its toml, and the existing
    // `run_ended` broadcast still fired -- all through the real
    // `handle_workspace_destroy` wire path. Hermetic despite going
    // through the full handler: `seed_provably_unheld_state_dir` (ADR
    // 0043 decision 33's own absence proof, reproduced on disk -- the
    // technique the test above also uses) makes the outcome
    // deterministic with no live supervisor at all, and
    // `XDG_CONFIG_HOME`/`XDG_STATE_HOME`/`SOT_STATE_HOST` are pinned to a
    // scratch dir so neither the toml write nor the state dir ever
    // touches a real `~/.config/sot` or `~/.local/state/sot`.
    //
    // Gated: the absence proof `seed_provably_unheld_state_dir` targets
    // only exists inside `destroy_capsule_workspace`'s `#[cfg(any(windows,
    // target_os = "linux"))]` arm.
    #[tokio::test]
    #[cfg(any(windows, target_os = "linux"))]
    async fn default_row_confirmed_ended_resets_agent_persists_toml_and_broadcasts() {
        let _guard = env_guarded();
        let dir = std::env::temp_dir().join(format!(
            "sot-ws-destroy-default-reset-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        std::env::set_var("SOT_STATE_HOST", "reset-test-host");
        let state_root = pin_local_state_root(&dir.join("state"));

        let (reg, id, slug) = seed_default_with_agent("claude", "kal-local");
        assert!(
            !reg.is_inert_default_anchor(&reg.resolve(Some(&id)).unwrap()),
            "a default row carrying an agent is a real session, not the anchor, before the fix runs"
        );
        seed_provably_unheld_state_dir(&state_root, &id);

        let session = Session::new();
        let (tx, mut rx) = broadcast::channel(16);
        let payload = json!({ "workspace_id": id });
        let out = handle_workspace_destroy(1, payload, &session, &reg, &tx)
            .await
            .expect("handler must not error");
        let response = out[0].0.payload.clone();
        assert!(
            response.get("error").is_none(),
            "a confirmed end must not error: {response:?}"
        );
        assert!(
            response.get("kept").is_some(),
            "a confirmed end reports the success shape: {response:?}"
        );

        // The row: agent reset, inert again, id unchanged.
        let after = reg
            .resolve(Some(&id))
            .expect("the default row is never removed");
        assert_eq!(after.workspace_id, id);
        assert_eq!(after.agent, "none");
        assert_eq!(after.agent_name, "");
        assert!(
            reg.is_inert_default_anchor(&after),
            "with agent reset to none, the default row must be inert again"
        );

        // The broadcast: the existing `run_ended` WorkspaceChanged, unchanged.
        let evt = rx
            .try_recv()
            .expect("run_ended must still be broadcast on a confirmed end");
        assert_eq!(evt.action, "run_ended");
        assert_eq!(evt.workspace_id, id);
        assert_eq!(evt.slug, slug);

        // The toml: the reset was persisted, not just held in memory.
        let toml_path = crate::workspaces::toml_path_for(&slug);
        let contents = std::fs::read_to_string(&toml_path)
            .unwrap_or_else(|e| panic!("toml must be persisted at {toml_path:?}: {e}"));
        assert!(
            contents.contains("agent         = \"none\""),
            "agent must persist as none:\n{contents}"
        );
        assert!(
            contents.contains("agent_name    = \"\""),
            "agent_name must persist as empty:\n{contents}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ADR 0043 decision 35: the default row's end prunes its sot-comm
    // registry row the same way `workspace.destroy`'s non-default path
    // already does (`remove_comm_agents_for_workspace_host_tests` proves
    // that path in isolation) -- before this lane, only the destroy path
    // pruned, so a Windows default-row end left a ghost row that
    // `workspace.list` merged back in. Two rows share the agent's handle
    // string on the shared registry, one on this test's host and one on
    // another, to prove the prune is host-scoped exactly like the
    // destroy-path prune it mirrors.
    //
    // Gated: same reason as the reset test above -- the confirmed-end
    // outcome `seed_provably_unheld_state_dir` produces only reaches
    // `Removable` through `destroy_capsule_workspace`'s `#[cfg(any(windows,
    // target_os = "linux"))]` arm.
    #[tokio::test]
    #[cfg(any(windows, target_os = "linux"))]
    async fn default_row_end_prunes_the_rows_registry_row() {
        let _guard = env_guarded();
        let stamp = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let config_dir =
            std::env::temp_dir().join(format!("sot-ws-destroy-default-leave-cfg-{stamp}"));
        let comm_dir =
            std::env::temp_dir().join(format!("sot-ws-destroy-default-leave-comm-{stamp}"));
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&comm_dir).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &config_dir);
        std::env::set_var("SOT_COMM_HOME", &comm_dir);
        std::env::set_var("SOT_STATE_HOST", "leave-test-host");
        let scratch_state =
            std::env::temp_dir().join(format!("sot-ws-destroy-default-leave-state-{stamp}"));
        let state_root = pin_local_state_root(&scratch_state);

        let handle = "default-row-leave-handle";
        std::fs::write(
            comm_dir.join("registry.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "agents": {
                    handle: {"tmux": "", "host": "leave-test-host"},
                    "other-host-handle": {"tmux": "", "host": "another-host"},
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let (reg, id, _slug) = seed_default_with_agent("claude", handle);
        seed_provably_unheld_state_dir(&state_root, &id);
        let payload = destroy(&reg, &id).await;
        assert!(
            payload.get("error").is_none(),
            "a confirmed end must not error: {payload:?}"
        );

        let after: serde_json::Value = serde_json::from_slice(
            &std::fs::read(comm_dir.join("registry.json")).unwrap(),
        )
        .unwrap();
        let agents = after.get("agents").unwrap().as_object().unwrap();
        assert!(
            !agents.contains_key(handle),
            "the default row's own handle must be pruned on a confirmed end: {agents:?}"
        );
        assert!(
            agents.contains_key("other-host-handle"),
            "another host's same-named-session row must survive: {agents:?}"
        );

        let _ = std::fs::remove_dir_all(&config_dir);
        let _ = std::fs::remove_dir_all(&comm_dir);
        let _ = std::fs::remove_dir_all(&scratch_state);
    }

    // A lane still `Starting` is never "not running" -- retryable
    // `Kept`, never a fabricated "was not running" success.
    #[test]
    fn starting_outcome_maps_to_a_retryable_kept_not_not_running() {
        let outcome = capsule_destroy_outcome_of(crate::capsule_workspace::EndRunOutcome::Starting);
        match outcome {
            CapsuleDestroyOutcome::Kept { detail } => {
                assert_eq!(detail, "supervisor is starting; retry");
            }
            CapsuleDestroyOutcome::Removable(detail) => {
                panic!("Starting must never be reported Removable (\"ended\"): {detail}");
            }
        }
    }

    // An authority found ALREADY resting in `EndedNoRespawn` is
    // `AlreadyEnded`, not a fabricated `RecordVerified` -- still
    // `Removable` (safe to report "ended").
    #[test]
    fn already_ended_outcome_is_removable_and_distinct_from_record_verified() {
        let outcome =
            capsule_destroy_outcome_of(crate::capsule_workspace::EndRunOutcome::AlreadyEnded);
        match outcome {
            CapsuleDestroyOutcome::Removable(detail) => {
                assert!(
                    !detail.contains("verified"),
                    "must not claim verification it never observed: {detail}"
                );
            }
            CapsuleDestroyOutcome::Kept { detail } => {
                panic!("AlreadyEnded is a confirmed end — must not be Kept: {detail}");
            }
        }
    }

    // Rounds out coverage of `capsule_destroy_outcome_of`'s remaining
    // variants: a real end_run's own two confirmed outcomes both map to
    // `Removable`, and `NotEnded` (failed/refused/outcome-unknown) maps
    // to `Kept`.
    #[test]
    fn record_verified_and_closed_are_removable_not_ended_is_kept() {
        use crate::capsule_workspace::EndRunOutcome as O;
        for outcome in [O::RecordVerified, O::RecordClosed] {
            assert!(
                matches!(
                    capsule_destroy_outcome_of(outcome.clone()),
                    CapsuleDestroyOutcome::Removable(_)
                ),
                "{outcome:?} must map to Removable"
            );
        }
        match capsule_destroy_outcome_of(O::NotEnded("end_run failed: boom".to_string())) {
            CapsuleDestroyOutcome::Kept { detail } => assert_eq!(detail, "end_run failed: boom"),
            CapsuleDestroyOutcome::Removable(detail) => {
                panic!("NotEnded must never map to Removable: {detail}");
            }
        }
    }

    // A leg that went `Terminal` (e.g. an unlaunchable agent argv) has no
    // live run to orphan — `end_run` already sent it `stop` and waited
    // for confirmed exit before ever reporting this outcome, so the row
    // must be `Removable`, never stuck `Kept` forever (the gap this
    // whole variant closes: an unendable capsule row).
    #[test]
    fn terminal_outcome_is_removable_not_kept() {
        use crate::capsule_workspace::EndRunOutcome as O;
        match capsule_destroy_outcome_of(O::Terminal) {
            CapsuleDestroyOutcome::Removable(detail) => {
                assert!(
                    detail.contains("terminal"),
                    "detail should explain the row was terminal: {detail}"
                );
            }
            CapsuleDestroyOutcome::Kept { detail } => {
                panic!("Terminal is a confirmed end (stop was sent and awaited) — must not be Kept: {detail}");
            }
        }
    }

    // `Unheld` (no supervisor holds the row — see its own doc) is a
    // confirmed end, same family as `Terminal`/`AlreadyEnded`: `Removable`,
    // never `Kept`.
    #[test]
    fn unheld_outcome_is_removable_not_kept() {
        use crate::capsule_workspace::EndRunOutcome as O;
        match capsule_destroy_outcome_of(O::Unheld) {
            CapsuleDestroyOutcome::Removable(detail) => {
                assert_eq!(detail, "no supervisor held the row");
            }
            CapsuleDestroyOutcome::Kept { detail } => {
                panic!("Unheld means nobody holds the row — must not be Kept: {detail}");
            }
        }
    }

    // A `Kept` outcome must build the SAME typed error the non-default
    // path returns, and must NEVER signal a `run_ended` broadcast.
    #[test]
    fn kept_outcome_builds_the_typed_error_and_never_broadcasts_run_ended() {
        let (payload, confirmed_ended) = default_row_end_response(
            "ws-local-1",
            "local",
            "local",
            CapsuleDestroyOutcome::Kept {
                detail: "supervisor is starting; retry".to_string(),
            },
        );
        assert_eq!(
            payload.get("code").and_then(|v| v.as_str()),
            Some("capsule_end_not_reached")
        );
        assert!(payload.get("error").is_some());
        assert!(
            payload.get("workspace_id").is_none(),
            "must not carry the success shape's own fields: {payload:?}"
        );
        assert!(payload.get("kept").is_none());
        assert!(
            !confirmed_ended,
            "a Kept outcome must never signal a run_ended broadcast"
        );
    }

    // The mirror case: a `Removable` (confirmed) outcome DOES build the
    // success shape and DOES signal the broadcast.
    #[test]
    fn removable_outcome_builds_success_and_signals_run_ended() {
        let (payload, confirmed_ended) = default_row_end_response(
            "ws-local-1",
            "local",
            "local",
            CapsuleDestroyOutcome::Removable("run ended and verified".to_string()),
        );
        assert!(
            payload.get("error").is_none(),
            "must not error: {payload:?}"
        );
        assert_eq!(
            payload.get("workspace_id").and_then(|v| v.as_str()),
            Some("ws-local-1")
        );
        assert!(payload.get("kept").and_then(|v| v.as_str()).is_some());
        assert!(confirmed_ended);
    }
}
