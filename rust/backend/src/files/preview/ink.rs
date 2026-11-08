//! The picture the agent reads for a drawing: the base (a figure scaled to the wire's raster, or a white canvas) with
//! the ink over it, written whole to `<root>/.sot/captures/ink-<row id>.png` through `FilesMode::write`, which
//! follows no link.

use std::path::PathBuf;

use image::ImageEncoder;
use sot_protocol::ink::{self, CANVAS_DIMS, INK_RGB};
use sot_protocol::ops::selection_code::{BAD_NODE_ID, NOT_AN_IMAGE, PUBLISH_FAILED, TOO_LARGE};
use sot_protocol::ops::{Drawing, InkBase};

use crate::files::tree::{mime_for_path, FilesMode};

/// A source over this many pixels is refused before any decode.
const MAX_SOURCE_PIXELS: u64 = 40_000_000;
/// Decode-alloc ceiling, the image crate's default stated here so a change of that default leaves it; the header
/// check above bounds the decode (RGBA at [`MAX_SOURCE_PIXELS`] is 160 MB).
const MAX_DECODE_ALLOC: u64 = 512 * 1024 * 1024;

/// A refusal: a `selection_code` and its message.
pub(crate) type Refusal = (&'static str, String);

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
    files
        .write(&format!("files:.sot/captures/ink-{row_id}.png"), &png)
        .map_err(|e| (PUBLISH_FAILED, format!("{e:#}")))
}

/// Opens `path` for reading without blocking and refuses, on the open handle, anything but a regular file: a FIFO
/// named like an image answers at once.
pub(crate) fn open_regular(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NONBLOCK | libc::O_NOCTTY);
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file"));
    }
    Ok(file)
}

/// The figure scaled to the wire's raster dimensions.
fn figure_raster(files: &FilesMode, node_id: &str) -> Result<image::RgbaImage, Refusal> {
    let path = files.node_id_to_path(node_id).map_err(|e| (BAD_NODE_ID, format!("{e:#}")))?;
    let mime = mime_for_path(&path);
    if !mime.starts_with("image/") {
        return Err((NOT_AN_IMAGE, format!("{path:?} is not an image (mime {mime})")));
    }
    // One open: the header is read from the decoder that then decodes, so a figure replaced between the check and
    // the decode cannot skip the size check.
    let mut reader = open_regular(&path)
        .and_then(|f| image::ImageReader::new(std::io::BufReader::new(f)).with_guessed_format())
        .map_err(|e| (NOT_AN_IMAGE, format!("open {path:?}: {e}")))?;
    let mut limits = image::Limits::no_limits();
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let decoder = reader.into_decoder().map_err(|e| (NOT_AN_IMAGE, format!("{path:?}: {e}")))?;
    let (w, h) = image::ImageDecoder::dimensions(&decoder);
    if u64::from(w) * u64::from(h) > MAX_SOURCE_PIXELS {
        return Err((TOO_LARGE, format!("{path:?} is {w}x{h}, over 40 megapixels")));
    }
    let src = image::DynamicImage::from_decoder(decoder)
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
