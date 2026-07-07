// preview/svg.rs — SVG bytes → RGBA8 bitmap via resvg → quad.
//
// This is the path inline-math takes: the backend's MathJax sidecar produces
// SVG for each math snippet (per ADR 0012); we rasterise here at the size the
// preview-layer assigns and upload as a quad. resvg supports MathJax-shape
// SVG natively; complex CSS-styled SVGs may need tweaks later.
//
// Render scale is keyed off the target rect's pixel size so math stays crisp
// at HiDPI without us tracking DPR explicitly — the caller passes the target
// pixel dimensions, we rasterise to those.

use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg::{fontdb, Options, Tree};

use super::quad::{Quad, QuadPipeline};

/// System-font database, populated once on first SVG. On Linux fontconfig
/// makes "serif" / "sans-serif" resolve transparently; on Windows usvg's
/// default fontdb is empty and `font-family="serif"` falls back to nothing,
/// dropping text glyphs entirely — `load_system_fonts()` is what closes that
/// gap.
fn system_fontdb() -> Arc<fontdb::Database> {
    static DB: OnceLock<Arc<fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        // Pin sensible generic-family defaults so `font-family="serif"` (the
        // shape MathJax SVG emits) resolves on every platform, not just where
        // fontconfig happens to map it for us.
        db.set_serif_family("Times New Roman");
        db.set_sans_serif_family("Arial");
        db.set_monospace_family("Consolas");
        tracing::info!(count = db.len(), "usvg system fontdb loaded");
        Arc::new(db)
    })
    .clone()
}

/// Rasterise SVG bytes into an RGBA8 bitmap of exactly `(target_w,
/// target_h)` and upload as a quad. Non-uniform scaling: the caller is
/// expected to have computed `target_*` from the SVG's intrinsic ex-unit
/// dimensions (see `gpu.rs::parse_math_svg_dims` /
/// `MATHJAX_EX_FACTOR`), so the aspect ratio is already correct. If the
/// caller passes an aspect that disagrees with the SVG, the output will
/// stretch — that's a caller bug, not a fit-recovery concern. Doing the
/// scale-to-fit inside this function used to silently turn short
/// equations into letterbox-tall slabs.
pub fn quad_from_svg_bytes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &QuadPipeline,
    bytes: &[u8],
    target_w: u32,
    target_h: u32,
) -> Result<Quad> {
    let opts = Options {
        fontdb: system_fontdb(),
        ..Options::default()
    };
    // MathJax SVG emits `fill="currentColor" stroke="currentColor"` on
    // its root glyph group, expecting a CSS cascade to resolve the
    // colour. resvg has no cascade, so `currentColor` falls back to
    // black — invisible against our dark chrome bg. Rewrite to white
    // before parsing. `white` is 5 chars vs `currentColor` 12, but
    // SVG is XML-textual and resvg doesn't care about byte offsets.
    let bytes_owned;
    let bytes: &[u8] = if memchr_currentcolor(bytes) {
        bytes_owned = replace_currentcolor_with_white(bytes);
        &bytes_owned
    } else {
        bytes
    };
    let tree = Tree::from_data(bytes, &opts).context("svg parse failed")?;

    let svg_size = tree.size();
    let sx = target_w as f32 / svg_size.width().max(0.001);
    let sy = target_h as f32 / svg_size.height().max(0.001);

    let pw = target_w.max(1);
    let ph = target_h.max(1);

    let mut pixmap = Pixmap::new(pw, ph)
        .ok_or_else(|| anyhow::anyhow!("pixmap alloc failed for {}x{}", pw, ph))?;

    resvg::render(&tree, Transform::from_scale(sx, sy), &mut pixmap.as_mut());

    Quad::from_rgba8(device, queue, pipeline, pixmap.data(), pw, ph)
}

fn memchr_currentcolor(bytes: &[u8]) -> bool {
    bytes.windows(12).any(|w| w == b"currentColor")
}

fn replace_currentcolor_with_white(bytes: &[u8]) -> Vec<u8> {
    let s = std::str::from_utf8(bytes).unwrap_or_default();
    s.replace("currentColor", "white").into_bytes()
}
