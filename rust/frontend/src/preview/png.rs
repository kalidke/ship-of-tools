// preview/png.rs — decode PNG bytes to RGBA8, hand the buffer to the quad
// pipeline. Future image/jpeg / image/webp use the same path.

use anyhow::{Context, Result};

use super::quad::{Quad, QuadPipeline, SamplerKind};

/// Decode-time allocation ceiling for a single raster. The `image` crate's
/// default limit is 512 MiB, which rejects large scientific rasters outright
/// — a 273 MP DNA-PAINT render needs ~1.1 GB decoded to RGBA8. We lift the
/// ceiling so the full image decodes (then `decode_and_fit` downsamples it to
/// the GPU's max texture dimension), preserving full-resolution pan/zoom.
/// Bounded rather than unlimited so a pathological multi-gigapixel file errors
/// cleanly ("Memory limit exceeded" → blank pane + logged warning) instead of
/// OOM-ing the frontend. ~1 gigapixel of RGBA fits under this; beyond that the
/// real answer is tiled decoding, not a bigger number.
const MAX_DECODE_ALLOC: u64 = 4 * 1024 * 1024 * 1024;

/// A decoded raster, carrying BOTH its texture dimensions and the dimensions
/// it decoded at before any GPU-fit downsample.
///
/// The two differ only for images whose longest side exceeds the GPU's
/// `max_texture_dimension_2d`. Keeping both is load-bearing for the ADR-0034
/// scalebar: the wire's `nm_per_px` describes the pixels of the image as
/// SERVED, so a bar computed against the shrunken texture width mis-scales by
/// `src/texture` (a 20000 px raster capped to 16384 reads ~1.22x short).
pub struct DecodedImage {
    pub rgba: image::RgbaImage,
    /// Texture dimensions — post GPU-fit downsample, what the Quad is built at.
    pub w: u32,
    pub h: u32,
    /// Dimensions as decoded, BEFORE the GPU-fit downsample. `nm_per_px` from
    /// the wire describes THESE pixels.
    pub src_w: u32,
    pub src_h: u32,
    /// True when the GPU-fit downsample actually shrank the image, so callers
    /// can note that the user is on a reduced-resolution view.
    pub downsampled: bool,
}

/// Decode bytes into an RGBA8 image and downsample if either dimension
/// exceeds the GPU's `max_texture_dimension_2d`.
fn decode_and_fit(device: &wgpu::Device, bytes: &[u8]) -> Result<DecodedImage> {
    // Decode with a lifted allocation ceiling (see MAX_DECODE_ALLOC) — the
    // default 512 MiB limit rejects large scientific rasters before we ever
    // get a chance to downsample them.
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .context("guess image format")?;
    let mut limits = image::Limits::no_limits();
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let img = reader.decode().context("image decode failed")?;
    let mut rgba = img.to_rgba8();
    let (mut w, mut h) = rgba.dimensions();
    // Remember the as-decoded size: the scalebar's nm/px is per THESE pixels.
    let (src_w, src_h) = (w, h);
    let cap = device.limits().max_texture_dimension_2d;
    let mut downsampled = false;
    if w > cap || h > cap {
        // Scale so the longest side lands at `cap`, preserving aspect.
        let scale = (cap as f32 / w.max(h) as f32).min(1.0);
        let new_w = ((w as f32 * scale).floor() as u32).max(1);
        let new_h = ((h as f32 * scale).floor() as u32).max(1);
        tracing::info!(
            orig_w = w,
            orig_h = h,
            cap,
            new_w,
            new_h,
            "downsampling oversized image to fit GPU max texture dimension"
        );
        // `thumbnail` (area-averaging) over `resize(Triangle)`: ~4x faster on
        // huge sources (273 MP -> 16384: ~2s vs ~8.5s release) with comparable
        // downscale quality, which keeps the one-shot first-render cost down.
        rgba = image::imageops::thumbnail(&rgba, new_w, new_h);
        w = new_w;
        h = new_h;
        downsampled = true;
    }
    Ok(DecodedImage {
        rgba,
        w,
        h,
        src_w,
        src_h,
        downsampled,
    })
}

pub fn quad_from_png_bytes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &QuadPipeline,
    bytes: &[u8],
    sampler: SamplerKind,
) -> Result<Quad> {
    Ok(quad_and_source_dims_from_png_bytes(device, queue, pipeline, bytes, sampler)?.0)
}

/// Like [`quad_from_png_bytes`], but also reports the image's dimensions
/// BEFORE any GPU-fit downsample — returning `(quad, src_w, src_h)`.
///
/// The preview pane uses this because the ADR-0034 scalebar must key off the
/// as-served pixel count: `quad.size_px` is the (possibly shrunken) TEXTURE
/// size, while the wire's `nm_per_px` describes the served image's pixels.
/// Deriving the source dims per decode — rather than rescaling `preview_scale`
/// in place — keeps it idempotent: `render_preview_source` re-runs on every
/// workspace-snapshot restore, and an in-place rescale would compound the
/// ratio on each swap-back.
pub fn quad_and_source_dims_from_png_bytes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &QuadPipeline,
    bytes: &[u8],
    sampler: SamplerKind,
) -> Result<(Quad, u32, u32)> {
    let d = decode_and_fit(device, bytes)?;
    // Sampler is the caller's call: Nearest for standalone scientific
    // PNGs (user 2026-05-22: individual source pixels visible on zoom,
    // no bilinear smear), Linear for rasterized document pages (ADR
    // 0021 — text edges want filtering, not pixel blocks). Markdown
    // figures stay Linear via `quad_and_dims_from_bytes`.
    let quad = Quad::from_rgba8_with_sampler(
        device,
        queue,
        pipeline,
        d.rgba.as_raw(),
        d.w,
        d.h,
        sampler,
    )?;
    Ok((quad, d.src_w, d.src_h))
}

/// Decode raster image bytes (PNG / JPEG / WebP / GIF first-frame /
/// BMP / TIFF — anything `image` recognises) into an RGBA8 bitmap at
/// its native size, returning the quad alongside its `(w, h)` so the
/// caller can size placeholder reservations. Errors propagate up — the
/// chrome already swallows + logs them. SVG goes through
/// `quad_from_svg_bytes`; this helper rejects it via mime check at the
/// call site.
pub fn quad_and_dims_from_bytes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &QuadPipeline,
    bytes: &[u8],
) -> Result<(Quad, u32, u32)> {
    let d = decode_and_fit(device, bytes)?;
    let quad = Quad::from_rgba8(device, queue, pipeline, d.rgba.as_raw(), d.w, d.h)?;
    Ok((quad, d.w, d.h))
}
