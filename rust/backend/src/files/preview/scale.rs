//! Physical scale (ADR 0034): preview.set_scale, the
//! scale sidecar beside an image, and a PNG's pHYs density.

use super::*;
use sot_protocol::PreviewSetScaleReq;

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
    if matches!(&extras, Some(serde_json::Value::Object(m)) if m.contains_key("physical_scale")) {
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
