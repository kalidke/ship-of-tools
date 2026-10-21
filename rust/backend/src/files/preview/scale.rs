//! Physical scale (ADR 0034): preview.set_scale, the
//! scale sidecar beside an image, and a PNG's pHYs density.

use super::*;
use sot_protocol::PreviewSetScaleReq;

use sot_protocol::physical_scale::parse_physical_scale;

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

    let ws = match row_or_reply(workspaces, req.workspace_id.as_deref(), req_id, op::PREVIEW_SET_SCALE) {
        Ok(ws) => ws,
        Err(reply) => return Ok(reply),
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
    if parse_physical_scale(&req.physical_scale).is_none() {
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

/// ADR 0034: for a raster image preview, look for a `<path>.scale.json` sidecar
/// and surface its JSON as `extras.physical_scale` (the FE renders a dynamic
/// scalebar from it). Read backend-side because raster previews are served here
/// (`read_bytes_preview`), not via the kernel — and a JSON sidecar is not image
/// metadata, so it doesn't cross the "Rust never parses image metadata" line.
///
/// Best-effort: a missing, unparseable or invalid sidecar leaves `extras` untouched, so a lower tier can still
/// answer. A valid sidecar's JSON (`{axes:[{name,nm_per_px}],unit}`) is passed through as written. Merges into
/// an existing `extras` object (e.g. a plugin's) rather than clobbering it.
pub(super) fn merge_scale_sidecar(
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
    // Validate sidecar metadata through the shared scale parser before overriding a lower tier.
    if parse_physical_scale(&scale).is_none() {
        tracing::warn!(sidecar = %sidecar.display(), "scale sidecar is not a valid physical scale — ignoring");
        return extras;
    }
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
pub(super) fn merge_png_phys_scale(
    bytes: &[u8],
    mime: &str,
    extras: Option<serde_json::Value>,
    raw_file_bytes: bool,
) -> Option<serde_json::Value> {
    if !raw_file_bytes || mime != "image/png" {
        return extras;
    }
    // Only a valid existing scale suppresses PNG pHYs fallback.
    if extras
        .as_ref()
        .and_then(|e| e.get("physical_scale"))
        .and_then(parse_physical_scale)
        .is_some()
    {
        return extras;
    }
    let Some((x_nm, y_nm)) = png_phys_nm_per_px(bytes) else {
        return extras;
    };
    // The FE renders ONE bar from `axes[0]` by design (ui/preview/image/overlay.rs: "Isotropic
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

#[cfg(test)]
#[path = "scale_scalebar_sidecar_tests.rs"]
mod scalebar_sidecar_tests;

#[cfg(test)]
#[path = "scale_phys_scale_tests.rs"]
mod phys_scale_tests;
