//! The window's quads: the pipeline every quad draws with, the solid-colour brushes the chrome and the preview paint
//! with, the logos and the help peek's backing.

use std::collections::HashMap;

use anyhow::{Context, Result};

use super::cells::{CellQuad, Rgb};
use super::gpu::Gpu;
use super::quad::{Quad, QuadPipeline, SamplerKind, ScreenRect};

/// At most this many cell background colours paint in one frame: the palette texture's width, inside every
/// adapter's smallest allowed texture. A run in a colour past it paints no background (a full-screen truecolor
/// gradient would reach it; no program in use does).
const MAX_BACK_COLORS: usize = 2048;

/// The frame's cell backgrounds, each rect with the palette point it samples: made by `prepare_backs` for the palette
/// it built, painted by `paint_backs`.
pub(in crate::ui) struct CellBacks(Vec<Sampled>);

/// A rect with the palette point it samples.
type Sampled = (ScreenRect, [f32; 2]);

/// The palette for `rects` (each colour once, in order of first appearance) and each rect with the centre of its
/// colour's texel; a colour past `MAX_BACK_COLORS` is left out with its rects.
fn back_palette(rects: &[CellQuad]) -> (Vec<Rgb>, Vec<Sampled>) {
    let mut palette: Vec<Rgb> = Vec::new();
    let mut index: HashMap<Rgb, usize> = HashMap::new();
    let mut texel: Vec<(ScreenRect, usize)> = Vec::with_capacity(rects.len());
    for q in rects {
        let i = match index.get(&q.color) {
            Some(&i) => i,
            None if palette.len() < MAX_BACK_COLORS => {
                index.insert(q.color, palette.len());
                palette.push(q.color);
                palette.len() - 1
            }
            None => continue,
        };
        texel.push((ScreenRect { x: q.x, y: q.y, w: q.w, h: q.h }, i));
    }
    let n = palette.len() as f32;
    let sampled = texel.into_iter().map(|(r, i)| (r, [(i as f32 + 0.5) / n, 0.5])).collect();
    (palette, sampled)
}

/// The solid brushes each drawing stage paints rects with.
#[derive(Clone, Copy)]
pub(in crate::ui) enum Solid {
    SelectionBg,
    OverlayBack,
    CodeBg,
    CodeBorder,
    StrikeLine,
    ScalebarBar,
    ScalebarBack,
    CaptionBack,
}

/// The render resources' quad stage. A brush's vertex buffer is rewritten by each paint, so every colour drawn in
/// one pass has its own quad; the border colours are built once each, before the pass that paints them.
pub(in crate::ui) struct Brushes {
    /// The textured-quad pipeline every quad in the window draws with.
    pipeline: QuadPipeline,
    /// A 1×1 pastel-yellow RGBA texture used as the LLM-pane selection
    /// highlight. Rendered as a per-row stretched quad before text.render
    /// so glyphs sit on top — gives a "highlighter" look that doesn't
    /// invert the text colour (REVERSED inverts both fg and bg, which
    /// hurt readability per user feedback). Solid colour with full alpha
    /// — the chrome's near-black bg shows around glyph antialias edges.
    selection_bg: Quad,
    /// A 1×1 near-opaque surface-navy RGBA texture: the backing strip
    /// under nav-spill overlay rows. Rendered AFTER the main text pass
    /// (so it covers preview glyphs and images alike), then the overlay
    /// text layer draws the row text on top. Alpha slightly under full
    /// so an image preview barely ghosts through — reads as "floating
    /// above", not "hole cut into the preview".
    overlay_back: Quad,
    /// A 1×1 dark slate RGBA texture used as the markdown-preview code
    /// bg — both inline `<code>` and fenced code blocks render against
    /// it so `Vec<u8>` doesn't look like prose. Paint pass walks
    /// `MarkdownPreview::code_glyph_rects` and stretches this quad
    /// behind each contiguous code-run, slightly lifted from the
    /// chrome bg toward VS-Code Dark+'s `#1e1e1e` so the panel reads
    /// clearly against the deep-navy surface.
    code_bg: Quad,
    /// 1×1 lighter-slate RGBA tile for the 1-px border around fenced
    /// code blocks. Four thin quads per block (top/bottom/left/right)
    /// give the panel an outline so it reads as a discrete block
    /// instead of "a stripe of slate behind the text" — matches the
    /// GitHub / VS Code code-block treatment.
    code_border: Quad,
    /// 1×1 light-gray RGBA tile painted as a thin horizontal quad
    /// through every `STRIKE_GLYPH_FLAG` run in the markdown preview.
    /// Replaces the U+0335 / U+0336 combining-mark fallback that
    /// rasterised inconsistently across fontdb's font picks (Segoe UI
    /// rendered it as an underline, Consolas barely at all).
    strike_line: Quad,
    /// Lazily-built solid-colour quads for pane-border box-drawing chars,
    /// keyed by RGB. `chrome::project_border_quads` emits arm-from-centre
    /// rects sized to the exact cell so stacked `│` borders tile without the
    /// sub-cell font-leading gaps cosmic-text leaves (the resolved monospace
    /// font doesn't fill the 18px cell). Cached rather than built per frame
    /// because `Quad::from_rgba8` allocates a GPU texture + bind group; pane
    /// borders use only 1–2 colours so the map stays tiny.
    borders: HashMap<(u8, u8, u8), Quad>,
    /// Solid quads for the ADR-0034 scalebar overlay: a white `bar` over a
    /// black `back`ing box. Dedicated quads (not `borders`), one per colour, since
    /// each `render_many` rewrites its quad's vertex buffer.
    scalebar_bar: Quad,
    scalebar_back: Quad,
    /// Backing box for the figure-caption overlay. A dedicated quad, not a
    /// reuse of `scalebar_back`: two draws in one pass need two quads, since
    /// `render_many` rewrites its quad's vertex buffer.
    caption_back: Quad,
    /// Miniature dark logo (from `LOGO_DARK_PNG`) flanking each session badge
    /// in the bottom strip, plus its native `(w, h)` for aspect-ratio sizing.
    /// `None` when the embedded PNG fails to decode — purely cosmetic, the
    /// strip renders unchanged without it.
    logo: Option<(Quad, u32, u32)>,
    /// Small wordmark logo (from `LOGO_WORDMARK_PNG`) drawn at the top of the
    /// nav pane, plus its native `(w, h)` for aspect-ratio sizing. `None` when
    /// the embedded PNG fails to decode.
    wordmark: Option<(Quad, u32, u32)>,
    help_back: Option<(u8, Quad)>,
    /// The cell backgrounds' palette, one texel per colour, and the colours it holds: rebuilt before a pass only
    /// when the frame's colours differ from the last frame's (`prepare_backs`).
    backs: Option<(Vec<Rgb>, Quad)>,
}

impl Brushes {
    #[allow(clippy::too_many_arguments)]
    pub(in crate::ui) fn new(
        pipeline: QuadPipeline, selection_bg: Quad, overlay_back: Quad, code_bg: Quad, code_border: Quad,
        strike_line: Quad, scalebar_bar: Quad, scalebar_back: Quad, caption_back: Quad,
        logo: Option<(Quad, u32, u32)>, wordmark: Option<(Quad, u32, u32)>,
    ) -> Self {
        Self {
            pipeline, selection_bg, overlay_back, code_bg, code_border, strike_line, borders: HashMap::new(),
            scalebar_bar, scalebar_back, caption_back, logo, wordmark, help_back: None, backs: None,
        }
    }

    /// The pipeline, lent to whoever builds or draws a quad of its own (rasters, figures, math, the monitor chart).
    pub(in crate::ui) fn pipeline(&self) -> &QuadPipeline { &self.pipeline }

    /// Paint `rects` with one solid brush.
    pub(in crate::ui) fn paint(
        &mut self, brush: Solid, gpu: &Gpu, rpass: &mut wgpu::RenderPass<'_>, rects: &[ScreenRect],
    ) -> Result<()> {
        let quad = match brush {
            Solid::SelectionBg => &mut self.selection_bg,
            Solid::OverlayBack => &mut self.overlay_back,
            Solid::CodeBg => &mut self.code_bg,
            Solid::CodeBorder => &mut self.code_border,
            Solid::StrikeLine => &mut self.strike_line,
            Solid::ScalebarBar => &mut self.scalebar_bar,
            Solid::ScalebarBack => &mut self.scalebar_back,
            Solid::CaptionBack => &mut self.caption_back,
        };
        quad.render_many(gpu.device(), gpu.queue(), &self.pipeline, rpass, rects,
            (gpu.config().width, gpu.config().height))
    }

    /// Build the border brush for `color` once (a GPU texture and bind group), before the pass that paints it;
    /// `what` names the brush in the error.
    pub(in crate::ui) fn ensure_border(&mut self, color: (u8, u8, u8), gpu: &Gpu, what: &'static str) -> Result<()> {
        if !self.borders.contains_key(&color) {
            let (r, g, b) = color;
            let quad = Quad::from_rgba8(gpu.device(), gpu.queue(), &self.pipeline, &[r, g, b, 255], 1, 1)
                .context(what)?;
            self.borders.insert(color, quad);
        }
        Ok(())
    }

    /// One batched paint per border colour that has rects.
    pub(in crate::ui) fn paint_borders(
        &mut self, gpu: &Gpu, rpass: &mut wgpu::RenderPass<'_>, rects_by_color: &HashMap<(u8, u8, u8), Vec<ScreenRect>>,
    ) -> Result<()> {
        for (color, quad) in self.borders.iter_mut() {
            if let Some(rects) = rects_by_color.get(color) {
                quad.render_many(gpu.device(), gpu.queue(), &self.pipeline, rpass, rects,
                    (gpu.config().width, gpu.config().height))?;
            }
        }
        Ok(())
    }

    /// Build the palette for this frame's cell backgrounds, before the pass that paints them, and return the rects
    /// with the points they sample. The palette is rebuilt only when the colours changed.
    pub(in crate::ui) fn prepare_backs(&mut self, rects: &[CellQuad], gpu: &Gpu) -> Result<CellBacks> {
        let (palette, sampled) = back_palette(rects);
        if !palette.is_empty() && self.backs.as_ref().map(|(p, _)| p) != Some(&palette) {
            let rgba: Vec<u8> = palette.iter().flat_map(|&(r, g, b)| [r, g, b, 255]).collect();
            let quad = Quad::from_rgba8_with_sampler(gpu.device(), gpu.queue(), &self.pipeline, &rgba,
                palette.len() as u32, 1, SamplerKind::Nearest).context("failed to build the cell background palette")?;
            self.backs = Some((palette, quad));
        }
        Ok(CellBacks(sampled))
    }

    /// Paint the cell backgrounds `prepare_backs` returned this frame, in one draw.
    pub(in crate::ui) fn paint_backs(&mut self, gpu: &Gpu, rpass: &mut wgpu::RenderPass<'_>, backs: &CellBacks) -> Result<()> {
        match self.backs.as_mut() {
            Some((_, quad)) => quad.render_many_texels(gpu.device(), gpu.queue(), &self.pipeline, rpass, &backs.0,
                (gpu.config().width, gpu.config().height)),
            None => Ok(()),
        }
    }

    /// The strip logo's native size, when it decoded.
    pub(in crate::ui) fn logo_size(&self) -> Option<(u32, u32)> { self.logo.as_ref().map(|(_, w, h)| (*w, *h)) }

    /// The bow wheels: one batched, rotated paint of the logo (nothing when it failed to decode).
    pub(in crate::ui) fn paint_logos(
        &mut self, gpu: &Gpu, rpass: &mut wgpu::RenderPass<'_>, rects: &[ScreenRect], angle: f32,
    ) -> Result<()> {
        match self.logo.as_mut() {
            Some((quad, _, _)) => quad.render_many_rotated(gpu.device(), gpu.queue(), &self.pipeline, rpass, rects,
                angle, (gpu.config().width, gpu.config().height)),
            None => Ok(()),
        }
    }

    /// The nav wordmark's native size, when it decoded.
    pub(in crate::ui) fn wordmark_size(&self) -> Option<(u32, u32)> { self.wordmark.as_ref().map(|(_, w, h)| (*w, *h)) }

    pub(in crate::ui) fn paint_wordmark(&self, gpu: &Gpu, rpass: &mut wgpu::RenderPass<'_>, rect: ScreenRect) -> Result<()> {
        match self.wordmark.as_ref() {
            Some((quad, _, _)) => quad.render(gpu.queue(), &self.pipeline, rpass, rect, (gpu.config().width, gpu.config().height)),
            None => Ok(()),
        }
    }

    /// The help peek's backing at `alpha`, rebuilt only when the alpha changes.
    pub(in crate::ui) fn ensure_help_back(&mut self, alpha: u8, gpu: &Gpu) -> Result<()> {
        if self.help_back.as_ref().map(|(a, _)| *a) != Some(alpha) {
            self.help_back = Some((alpha, Quad::from_rgba8(gpu.device(), gpu.queue(), &self.pipeline,
                &[14, 30, 46, alpha], 1, 1)?));
        }
        Ok(())
    }

    pub(in crate::ui) fn has_help_back(&self) -> bool { self.help_back.is_some() }

    pub(in crate::ui) fn paint_help_back(&self, gpu: &Gpu, rpass: &mut wgpu::RenderPass<'_>, rect: ScreenRect) -> Result<()> {
        match self.help_back.as_ref() {
            Some((_, quad)) => quad.render(gpu.queue(), &self.pipeline, rpass, rect, (gpu.config().width, gpu.config().height)),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
#[path = "brushes_tests.rs"]
mod tests;
