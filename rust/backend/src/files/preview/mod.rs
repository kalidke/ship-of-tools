//! preview.get: a node's preview from a kernel plugin or the file's bytes, capped, and downsampled
//! when an oversize raster would hold the connection.

use anyhow::Context;
use anyhow::Result;
use serde_json::json;
use sot_protocol::op;
use sot_protocol::BlobDescriptor;
use sot_protocol::Frame;
use sot_protocol::PreviewGetReq;
use sot_protocol::PreviewGetRes;

use crate::files::tree::mime_for_path;
use crate::server::reply::HandlerOutput;
use crate::sidecars::kernel::Kernel;
use crate::session::Session;
use crate::rows::Workspace;
use crate::rows::Workspaces;

pub(crate) mod crop;
pub(crate) mod scale;

use scale::{merge_png_phys_scale, merge_scale_sidecar};

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
            if let Some(unavailable) = e.downcast_ref::<crate::sidecars::kernel::KernelUnavailable>() {
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

#[cfg(test)]
mod preview_gate_tests {
    use super::{is_bounded_output_plugin, read_bytes_preview};
    use std::path::Path;

    #[test]
    fn every_video_extension_in_any_case_is_bounded() {
        for p in ["a.mp4", "A.MP4", "b.WebM", "c.mov", "d.MKV", "e.m4v"] {
            assert!(is_bounded_output_plugin(Path::new(p)), "{p} should be exempt");
        }
        for p in ["clip.avi", "song.mp3", "x.gif", "noext", "mp4"] {
            assert!(!is_bounded_output_plugin(Path::new(p)), "{p} should be gated");
        }
    }

    #[test]
    fn the_bytes_reader_answers_a_video_with_the_unavailable_note() {
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("clip.MOV");
        std::fs::write(&video, b"\x00\x01\x02").unwrap();
        let (mime, body) = read_bytes_preview(&video, "n1").unwrap();
        assert_eq!(mime, "text/markdown");
        assert!(body.starts_with(b"# video preview unavailable"));
        let other = dir.path().join("clip.avi");
        std::fs::write(&other, b"raw-bytes").unwrap();
        let (_, body) = read_bytes_preview(&other, "n2").unwrap();
        assert_eq!(body, b"raw-bytes");
    }

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
