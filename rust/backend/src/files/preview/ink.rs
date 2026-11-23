//! The picture the agent reads for a drawing: the base (a figure or a kernel-drawn page scaled to the wire's raster,
//! or a white canvas) with the ink over it, written whole to `<root>/.sot/captures/ink-<row id>.png` through `FilesMode::write`, which
//! follows no link.

use std::path::PathBuf;

use image::ImageEncoder;
use sot_protocol::ink::{self, CANVAS_DIMS, INK_RGB};
use sot_protocol::ops::selection_code::{BAD_NODE_ID, NOT_AN_IMAGE, PUBLISH_FAILED, TOO_LARGE};
use sot_protocol::ops::{Drawing, InkBase};

use crate::files::tree::{mime_for_path, FilesMode};
use crate::rows::Workspace;

/// A source over this many pixels is refused before any decode.
const MAX_SOURCE_PIXELS: u64 = 40_000_000;
/// Decode-alloc ceiling, the image crate's default stated here so a change of that default leaves it; the header
/// check above bounds the decode (RGBA at [`MAX_SOURCE_PIXELS`] is 160 MB).
const MAX_DECODE_ALLOC: u64 = 512 * 1024 * 1024;

/// A refusal: a `selection_code` and its message.
pub(crate) type Refusal = (&'static str, String);

/// Page `page` of `node_id` as the row's kernel draws it for `preview.get` (its plugin is the one reader of a paged
/// file), fitted to the wire's raster: a picture's bytes for `compose`. The kernel is asked only for a page from 1 of a
/// regular file the PDF plugin claims (a path ending in `.pdf`, in any case), by a lookup that opens nothing: any other node (a text file, which its own plugin would
/// read; a FIFO, which the kernel's tool would open and wait on) and page 0 are `not_an_image` unasked. So are a note
/// in place of a picture, a kernel that does not answer within its bound, and a page other than the one asked (the
/// plugin clamps the page and says which it drew).
pub(crate) async fn page_png(ws: &Workspace, node_id: &str, page: u32) -> Result<Vec<u8>, Refusal> {
    let files = ws.files_mode().map_err(|e| (BAD_NODE_ID, format!("{e:#}")))?;
    let path = files.node_id_to_path(node_id).map_err(|e| (BAD_NODE_ID, format!("{e:#}")))?;
    // The plugin's own rule for a PDF (`matches` in the PDF plugin): the path as the kernel is handed it, lowercased,
    // ends in `.pdf`. A file whose whole name is `.pdf` is one, which `Path::extension` would not call one.
    let named = path.to_string_lossy().to_lowercase().ends_with(".pdf");
    let pdf = named && std::fs::metadata(&path).is_ok_and(|m| m.is_file());
    if page == 0 || !pdf {
        return Err((NOT_AN_IMAGE, format!("{path:?} page {page} is no page of a PDF file")));
    }
    let fit = Some(ink::INK_RASTER_MAX);
    match super::try_plugin_preview(&ws.kernel(), &path, node_id, Some(page), fit, fit).await {
        Ok(Some((mime, png, Some(extras)))) if mime.starts_with("image/") && extras["page"] == page => Ok(png),
        other => Err((NOT_AN_IMAGE, format!("{path:?} page {page} was not drawn: {:?}", other.map(|o| o.map(|p| (p.0, p.2)))))),
    }
}

/// Writes the picture of `drawing` for the row `row_id` and returns its path. `page` is the base's page as `page_png`
/// answered it, for a base that names one; such a base with no page bytes is `not_an_image`, whatever its node. CPU-
/// and IO-bound: call it on the blocking pool.
pub(crate) fn compose(files: &FilesMode, row_id: &str, drawing: &Drawing, page: Option<&[u8]>) -> Result<PathBuf, Refusal> {
    let base = match (&drawing.base, page) {
        (InkBase::Canvas, _) => image::RgbaImage::from_pixel(CANVAS_DIMS.0, CANVAS_DIMS.1, image::Rgba([255; 4])),
        (InkBase::Figure { page: Some(_), .. }, Some(png)) => {
            scaled(image::ImageReader::new(std::io::Cursor::new(png)).with_guessed_format(), "the page")?
        }
        (InkBase::Figure { page: Some(page), .. }, None) => return Err((NOT_AN_IMAGE, format!("page {page} was not drawn"))),
        (InkBase::Figure { node_id, .. }, _) => figure_raster(files, node_id)?,
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

/// The figure scaled to the wire's raster dimensions.
fn figure_raster(files: &FilesMode, node_id: &str) -> Result<image::RgbaImage, Refusal> {
    let path = files.node_id_to_path(node_id).map_err(|e| (BAD_NODE_ID, format!("{e:#}")))?;
    let mime = mime_for_path(&path);
    if !mime.starts_with("image/") {
        return Err((NOT_AN_IMAGE, format!("{path:?} is not an image (mime {mime})")));
    }
    // One open: the header is read from the decoder that then decodes, so a figure replaced between the check and
    // the decode cannot skip the size check.
    let reader = sot_platform::host::open_regular(&path)
        .and_then(|f| image::ImageReader::new(std::io::BufReader::new(f)).with_guessed_format());
    scaled(reader, &format!("{path:?}"))
}

/// The picture `reader` holds (`what`, for a refusal's message), decoded under the limits and scaled to the wire's
/// raster dimensions.
fn scaled<R: std::io::BufRead + std::io::Seek>(
    reader: std::io::Result<image::ImageReader<R>>, what: &str,
) -> Result<image::RgbaImage, Refusal> {
    let mut reader = reader.map_err(|e| (NOT_AN_IMAGE, format!("open {what}: {e}")))?;
    let mut limits = image::Limits::no_limits();
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let decoder = reader.into_decoder().map_err(|e| (NOT_AN_IMAGE, format!("{what}: {e}")))?;
    let (w, h) = image::ImageDecoder::dimensions(&decoder);
    if u64::from(w) * u64::from(h) > MAX_SOURCE_PIXELS {
        return Err((TOO_LARGE, format!("{what} is {w}x{h}, over 40 megapixels")));
    }
    let src = image::DynamicImage::from_decoder(decoder)
        .map_err(|e| (NOT_AN_IMAGE, format!("decode {what}: {e}")))?
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
