//! image.crop: a region of an image, cut from the
//! source file and written as a PNG under the row's .sot/captures/ (ADR 0022).

use super::*;
use sot_protocol::ImageCropReq;
use sot_protocol::ImageCropRes;

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

    let ws = match row_or_reply(workspaces, req.workspace_id.as_deref(), req_id, op::IMAGE_CROP) {
        Ok(ws) => ws,
        Err(reply) => return Ok(reply),
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

    let (req_x, req_y, req_w, req_h) = (req.x, req.y, req.w, req.h);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("image")
        .to_string();
    let path_for_blk = path.clone();
    let files_for_blk = files_mode.clone();
    let crop_result = tokio::task::spawn_blocking(move || {
        crop_to_captures(&files_for_blk, &path_for_blk, &stem, (req_x, req_y, req_w, req_h))
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

type CropOk = (std::path::PathBuf, u32, u32, u32, u32, u32, u32);

/// Decode + clamp + crop + write, on a blocking thread: the decode and the PNG encode are synchronous CPU+IO and a large
/// decode would otherwise stall the tokio executor. Returns the written path, the clamped rect and the source
/// dimensions, or a (code, message) pair the caller turns into an error frame. The write lands in
/// `<project_root>/.sot/captures/` (watcher-exempt) through `FilesMode::write`, which follows no link; the filename carries the
/// source stem + a microsecond stamp so repeated captures don't clobber.
pub(super) fn crop_to_captures(
    files: &crate::files::tree::FilesMode,
    src: &std::path::Path,
    stem: &str,
    (req_x, req_y, req_w, req_h): (u32, u32, u32, u32),
) -> std::result::Result<CropOk, (String, String)> {
    let file = sot_platform::host::open_regular(src).map_err(|e| ("decode_failed".into(), format!("open {src:?}: {e}")))?;
    let img = image::ImageReader::new(std::io::BufReader::new(file))
        .with_guessed_format()
        .and_then(|r| r.decode().map_err(std::io::Error::other))
        .map_err(|e| ("decode_failed".into(), format!("decode {src:?}: {e}")))?;
    let (src_w, src_h) = (img.width(), img.height());
    if src_w == 0 || src_h == 0 {
        return Err(("empty_image".into(), format!("{src:?} has zero dimension")));
    }
    let x = req_x.min(src_w - 1);
    let y = req_y.min(src_h - 1);
    let w = req_w.clamp(1, src_w - x);
    let h = req_h.clamp(1, src_h - y);
    let cropped = img.crop_imm(x, y, w, h);
    let micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0);
    let mut png = std::io::Cursor::new(Vec::new());
    cropped
        .write_to(&mut png, image::ImageFormat::Png)
        .map_err(|e| ("io_error".into(), format!("encode the crop: {e}")))?;
    let out_path = files
        .write(&format!("files:.sot/captures/{stem}-roi-{micros}.png"), png.get_ref())
        .map_err(|e| ("io_error".into(), format!("{e:#}")))?;
    Ok((out_path, x, y, w, h, src_w, src_h))
}
