//! The picture the agent reads for a drawing: the base (a figure scaled to the wire's raster, or a white canvas) with
//! the ink over it, written whole to `<root>/.sot/captures/ink-<row id>.png`. Every write under `.sot/captures/`
//! (this one and the crop's) names its file through [`captures_file`].

use std::path::PathBuf;

use image::ImageEncoder;
use sot_protocol::ink::{self, CANVAS_DIMS, INK_RGB};
use sot_protocol::ops::selection_code::{BAD_NODE_ID, NOT_AN_IMAGE, PUBLISH_FAILED, TOO_LARGE};
use sot_protocol::ops::{Drawing, InkBase};

use super::decode_limited;
use crate::files::tree::{mime_for_path, FilesMode};

/// A source over this many pixels is refused before any decode.
const MAX_SOURCE_PIXELS: u64 = 40_000_000;
/// Decode-alloc ceiling for a source under [`MAX_SOURCE_PIXELS`] (RGBA at the bound is 160 MB).
const MAX_DECODE_ALLOC: u64 = 512 * 1024 * 1024;

/// A refusal: a `selection_code` and its message.
pub(crate) type Refusal = (&'static str, String);

/// The path of `name` in `<root>/.sot/captures/`, resolved by files' write resolver so a `.sot` or `captures` link
/// that leads out of the root is refused; the folder is created only after that check.
pub(crate) fn captures_file(files: &FilesMode, name: &str) -> anyhow::Result<PathBuf> {
    let path = files.node_id_to_path_confined(&format!("files:.sot/captures/{name}"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    Ok(path)
}

/// Writes the picture of `drawing` for the row `row_id` and returns its path. CPU- and IO-bound: call it on the
/// blocking pool.
pub(crate) fn compose(files: &FilesMode, row_id: &str, drawing: &Drawing) -> Result<PathBuf, Refusal> {
    let base = match &drawing.base {
        InkBase::Canvas => image::RgbaImage::from_pixel(CANVAS_DIMS.0, CANVAS_DIMS.1, image::Rgba([255; 4])),
        InkBase::Figure { node_id } => figure_raster(files, node_id)?,
    };
    let (rw, rh) = base.dimensions();
    let mask = ink::mask(&drawing.strokes, (rw, rh));
    let mut out = base;
    for (px, &m) in out.pixels_mut().zip(&mask) {
        if m > 0 {
            *px = over(*px, m);
        }
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(out.as_raw(), rw, rh, image::ExtendedColorType::Rgba8)
        .map_err(|e| (PUBLISH_FAILED, format!("encode the picture: {e}")))?;
    let path = captures_file(files, &format!("ink-{row_id}.png"))
        .map_err(|e| (PUBLISH_FAILED, format!("{e:#}")))?;
    sot_platform::host::write_durable(&path, &png)
        .map_err(|e| (PUBLISH_FAILED, format!("write {path:?}: {e}")))?;
    Ok(path)
}

/// The figure scaled to the wire's raster dimensions.
fn figure_raster(files: &FilesMode, node_id: &str) -> Result<image::RgbaImage, Refusal> {
    let path = files.node_id_to_path(node_id).map_err(|e| (BAD_NODE_ID, format!("{e:#}")))?;
    let mime = mime_for_path(&path);
    if !mime.starts_with("image/") {
        return Err((NOT_AN_IMAGE, format!("{path:?} is not an image (mime {mime})")));
    }
    let open = || -> Result<_, Refusal> {
        image::ImageReader::open(&path)
            .and_then(|r| r.with_guessed_format())
            .map_err(|e| (NOT_AN_IMAGE, format!("open {path:?}: {e}")))
    };
    let (w, h) = open()?.into_dimensions().map_err(|e| (NOT_AN_IMAGE, format!("{path:?}: {e}")))?;
    if u64::from(w) * u64::from(h) > MAX_SOURCE_PIXELS {
        return Err((TOO_LARGE, format!("{path:?} is {w}x{h}, over 40 megapixels")));
    }
    let src = decode_limited(open()?, MAX_DECODE_ALLOC)
        .map_err(|e| (NOT_AN_IMAGE, format!("decode {path:?}: {e}")))?
        .to_rgba8();
    let (rw, rh) = ink::raster_dims(w, h);
    Ok(image::imageops::thumbnail(&src, rw, rh))
}

/// `INK_RGB` at coverage `m / 255` source-over `px`, straight alpha.
fn over(px: image::Rgba<u8>, m: u8) -> image::Rgba<u8> {
    let a = f32::from(m) / 255.0;
    let ba = f32::from(px[3]) / 255.0;
    let oa = a + ba * (1.0 - a);
    let ch = |i: usize| ((f32::from(INK_RGB[i]) * a + f32::from(px[i]) * ba * (1.0 - a)) / oa).round() as u8;
    image::Rgba([ch(0), ch(1), ch(2), (oa * 255.0).round() as u8])
}
