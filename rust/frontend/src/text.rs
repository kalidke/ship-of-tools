// text.rs — glyph-atlas text rendering on wgpu via glyphon.
//
// Per ADR 0011: cosmic-text is the single source of truth for text shaping
// and metrics. glyphon hosts cosmic-text-shaped runs in a wgpu glyph atlas
// and renders them inside a render pass.
//
// This module is the lowest layer of the text stack. The custom ratatui
// Backend in chrome.rs will sit on top and translate cells into TextAreas;
// the markdown previewer in preview.rs will use the same FontSystem to lay
// out longer flows.
//
// Public surface for now is intentionally tiny — `prepare_lines` + `render` —
// and grows as chrome and preview demand more (multiple buffers, scrolling
// regions, attribute spans, mixed monospace/proportional, etc.).

use anyhow::{Context, Result};
use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, Style,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight,
};

/// One run of text to render at a baseline position (top-left of the line
/// box, in physical pixels relative to the surface). `color` overrides the
/// TextLayer's default rgb when `Some`; `None` means "fall through to the
/// (220,220,220) default". `bold` / `italic` toggle the cosmic-text Attrs
/// weight + style. `dim` attenuates the resolved colour to ~50% — terminal
/// DIM convention — by adjusting `default_color`, since cosmic-text Attrs
/// doesn't expose a DIM channel. Chrome.rs splits each grid row into
/// multiple `Line` runs along colour / modifier boundaries so each gets
/// its own TextArea.
pub struct Line {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub color: Option<(u8, u8, u8)>,
    pub bold: bool,
    pub italic: bool,
    pub dim: bool,
}

/// Pixels of headroom inserted at the top of every ExtraArea — see the
/// `top:` field comment in `TextLayer::prepare` for why. Re-exported so the
/// scroll-clamp math in the chrome can compensate the bottom by the same
/// amount and the user can still reach the end of the document.
pub const EXTRA_TOP_PAD_PX: f32 = 4.0;

/// A pre-built cosmic-text Buffer (e.g. a flowed markdown buffer from the
/// preview layer) the chrome should host alongside its monospace lines so
/// they share one glyphon prepare/render pass.
pub struct ExtraArea<'a> {
    pub buffer: &'a Buffer,
    /// Glyph-origin top-left in physical pixels. The buffer's
    /// (buffer_x=0, buffer_y=0) glyph renders at this screen position
    /// before the per-area `scroll_y_px` shift and the universal
    /// `EXTRA_TOP_PAD_PX` headroom are applied (see `prepare`).
    pub x: f32,
    pub y: f32,
    /// Clip bounds (right/bottom edges) in physical pixels, used to keep
    /// flowed text from spilling outside the rect ratatui allocated.
    pub right: f32,
    pub bottom: f32,
    /// Optional override for `TextBounds.left`. When `None`, the bounds
    /// use `x.floor()`, which is correct for panes where the glyph
    /// origin sits at the pane edge. For wide-table extras where `x`
    /// is shifted *left of* the pane edge for horizontal scroll, set
    /// this to the pane's left edge so the off-pane glyphs get clipped
    /// instead of bleeding outside the preview rect.
    pub clip_left: Option<f32>,
    /// Optional override for `TextBounds.top`. Mirrors `clip_left` for
    /// the vertical axis — only used when the glyph origin sits above
    /// the pane top (none of the current callers do this, but the
    /// symmetry keeps the bounds story honest).
    pub clip_top: Option<f32>,
    pub color: (u8, u8, u8),
    /// Vertical scroll offset in physical pixels. Glyph origin is moved
    /// up by this amount while the clip bounds stay fixed to the rect,
    /// so content beyond the top is clipped away and content below the
    /// bottom waits in queue. 0 = no scroll (default position).
    pub scroll_y_px: f32,
}

pub struct TextLayer {
    font_system: FontSystem,
    swash_cache: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    text_renderer: TextRenderer,

    /// Buffers we own one-per-line. Kept persistent across frames so
    /// cosmic-text's shape-run-cache amortises across redraws — without
    /// reuse, `Buffer::new + set_text + shape_until_scroll` pays 100µs
    /// to 1.3ms per Line per frame (per cosmic-text issue #245), which
    /// for an LLM-pane redraw of 30-80 Lines blew out the paint budget.
    buffers: Vec<Buffer>,

    /// Per-buffer signature `(text, width, bold, italic)` of the last
    /// content shaped into that slot. When the next frame produces an
    /// identical signature for the same slot we skip set_size/set_text/
    /// shape entirely — typical chrome rows (status line, idle nav,
    /// untouched LLM scrollback) are stable across frames so the
    /// shape cost falls off a cliff.
    sigs: Vec<BufferSig>,

    metrics: Metrics,
    last_width: u32,
    last_height: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct BufferSig {
    text: String,
    bold: bool,
    italic: bool,
    width: u32,
    height: u32,
}

impl TextLayer {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        surface_format: wgpu::TextureFormat,
        scale: f32,
    ) -> Self {
        let font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(device);
        let viewport = Viewport::new(device, &cache);
        let mut atlas = TextAtlas::new(device, queue, &cache, surface_format);
        let text_renderer =
            TextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None);

        Self {
            font_system,
            swash_cache,
            viewport,
            atlas,
            text_renderer,
            buffers: Vec::new(),
            sigs: Vec::new(),
            metrics: Metrics::new(14.0 * scale, 18.0 * scale),
            last_width: 0,
            last_height: 0,
        }
    }

    pub fn resize(&mut self, queue: &wgpu::Queue, width: u32, height: u32) {
        self.viewport.update(
            queue,
            Resolution {
                width,
                height,
            },
        );
    }

    /// Borrow the FontSystem so preview-layer modules (markdown, etc.) can
    /// build their own Buffers against the same font cache the chrome uses.
    pub fn font_system_mut(&mut self) -> &mut FontSystem {
        &mut self.font_system
    }

    /// Replace the chrome's per-line metrics (font size + line height).
    ///
    /// Updating `self.metrics` alone only affects buffers created *after*
    /// the change — the already-cached per-line `Buffer`s keep their old
    /// metrics, and `prepare`'s sig cache (which keys on text/bold/italic/
    /// size, not font size) skips re-shaping unchanged lines. The net effect
    /// of that bug was that Ctrl+=/- changed cell spacing (`cell_h` in the
    /// chrome) but not glyph size. So push the new metrics into every live
    /// buffer here; cosmic-text re-shapes each in place at its current size.
    /// The sigs stay valid because content is unchanged — the buffers are
    /// already correct after this, so the next `prepare` renders them
    /// without redundant re-shaping.
    pub fn set_metrics(&mut self, metrics: Metrics) {
        self.metrics = metrics;
        let fs = &mut self.font_system;
        for buf in self.buffers.iter_mut() {
            buf.set_metrics(fs, metrics);
        }
    }

    /// Measure the actual monospace glyph advance (physical px) at the
    /// current metrics. Used by the chrome to size its cell grid so the
    /// cursor block lines up flush with the typed text — without this,
    /// `BASE_CELL_W = 9.0` doesn't match cosmic-text's actual advance
    /// (Consolas at 14pt is ~7.7px) and the gap grows linearly with
    /// column, putting the cursor visibly right of the last typed char.
    ///
    /// Returns `None` on shape failure (no monospace font installed
    /// — caller should fall back to the static BASE_CELL_W * scale).
    pub fn monospace_advance(&mut self) -> Option<f32> {
        let mut buf = Buffer::new(&mut self.font_system, self.metrics);
        buf.set_size(&mut self.font_system, Some(2000.0), None);
        buf.set_text(
            &mut self.font_system,
            "xxxxxxxxxx",
            Attrs::new().family(Family::Monospace),
            Shaping::Basic,
        );
        buf.shape_until_scroll(&mut self.font_system, false);
        for run in buf.layout_runs() {
            if let Some(last) = run.glyphs.last() {
                let total = last.x + last.w;
                if total > 0.0 {
                    return Some(total / 10.0);
                }
            }
        }
        None
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        lines: &[Line],
        extras: &[ExtraArea],
    ) -> Result<()> {
        // Latency instrumentation: how many Buffers we actually re-shape
        // this frame vs how many we skip because their content is
        // unchanged. The keystroke-echo path tends to dirty 1-2 cells in
        // the LLM pane while leaving the other ~30+ Lines untouched, so
        // most frames should report `reshape ≪ total`.
        let prepare_t0 = std::time::Instant::now();
        let mut reshape_count = 0usize;
        let mut skip_count = 0usize;
        // Grow the persistent Buffer / sig vecs to fit current line count.
        // Shrinking just truncates — we hold the trailing slots until
        // they become needed again, paying a single Buffer::new only
        // when the chrome grows.
        while self.buffers.len() < lines.len() {
            self.buffers
                .push(Buffer::new(&mut self.font_system, self.metrics));
            self.sigs.push(BufferSig::default());
        }
        self.buffers.truncate(lines.len());
        self.sigs.truncate(lines.len());

        let size_changed = width != self.last_width || height != self.last_height;
        self.last_width = width;
        self.last_height = height;

        for ((buf, sig), line) in self
            .buffers
            .iter_mut()
            .zip(self.sigs.iter_mut())
            .zip(lines.iter())
        {
            let new_sig = BufferSig {
                text: line.text.clone(),
                bold: line.bold,
                italic: line.italic,
                width,
                height,
            };
            if !size_changed && *sig == new_sig {
                skip_count += 1;
                continue;
            }
            buf.set_size(
                &mut self.font_system,
                Some(width as f32),
                Some(height as f32),
            );
            let mut attrs = Attrs::new().family(Family::Monospace);
            if line.bold {
                attrs = attrs.weight(Weight::BOLD);
            }
            if line.italic {
                attrs = attrs.style(Style::Italic);
            }
            buf.set_text(
                &mut self.font_system,
                &line.text,
                attrs,
                Shaping::Advanced,
            );
            buf.shape_until_scroll(&mut self.font_system, false);
            *sig = new_sig;
            reshape_count += 1;
        }

        let chrome_areas = self.buffers.iter().zip(lines.iter()).map(|(buf, line)| {
            // Default fg sits at (204, 204, 204) — the VS Code Dark+ /
            // GitHub Dark default-foreground tone. Full-white (238+)
            // looks bright-white over a near-black background and is
            // fatiguing in long sessions; reserve that for explicit ANSI
            // 15 / Color::White from the chrome. DIM ≈ 65% gets you
            // ~133-grey, still above the contrast cliff at 14px mono.
            let (mut r, mut g, mut b) = line.color.unwrap_or((204, 204, 204));
            if line.dim {
                r = ((r as u16 * 65) / 100) as u8;
                g = ((g as u16 * 65) / 100) as u8;
                b = ((b as u16 * 65) / 100) as u8;
            }
            TextArea {
                buffer: buf,
                left: line.x,
                top: line.y,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: 0,
                    right: width as i32,
                    bottom: height as i32,
                },
                default_color: Color::rgb(r, g, b),
                custom_glyphs: &[],
            }
        });

        let extra_areas = extras.iter().map(|e| TextArea {
            buffer: e.buffer,
            left: e.x,
            // Shift the glyph origin DOWN by EXTRA_TOP_PAD_PX so the
            // first line's cap-height has a couple pixels of headroom
            // inside the pane. Without this, an H1 first line (24px
            // font in a 30px line-height) lands with its glyph tops at
            // buffer y=0 — and AA bleed clips against `bounds.top`,
            // making the heading look chopped. Bounds stay anchored to
            // the rect so anything past e.y still gets clipped instead
            // of leaking into the pane above; `preview_scroll_px`
            // continues to subtract as before.
            top: e.y + EXTRA_TOP_PAD_PX - e.scroll_y_px,
            scale: 1.0,
            bounds: TextBounds {
                left: e.clip_left.unwrap_or(e.x).floor() as i32,
                top: e.clip_top.unwrap_or(e.y).floor() as i32,
                right: e.right.ceil() as i32,
                bottom: e.bottom.ceil() as i32,
            },
            default_color: Color::rgb(e.color.0, e.color.1, e.color.2),
            custom_glyphs: &[],
        });

        let areas: Vec<TextArea> = chrome_areas.chain(extra_areas).collect();

        self.text_renderer
            .prepare(
                device,
                queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                areas,
                &mut self.swash_cache,
            )
            .context("glyphon prepare failed")?;

        let prepare_us = prepare_t0.elapsed().as_micros();
        // DEBUG, not INFO: this fires on every rendered frame; at INFO it
        // floods stdout (~6800 lines/min during active terminal streaming).
        tracing::debug!(
            prepare_us,
            reshape = reshape_count,
            skip = skip_count,
            "text_layer.prepare"
        );
        Ok(())
    }

    pub fn render<'pass>(&'pass self, render_pass: &mut wgpu::RenderPass<'pass>) -> Result<()> {
        self.text_renderer
            .render(&self.atlas, &self.viewport, render_pass)
            .context("glyphon render failed")?;
        Ok(())
    }

    pub fn trim(&mut self) {
        self.atlas.trim();
    }
}
