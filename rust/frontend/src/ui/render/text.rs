// text.rs — glyph-atlas text rendering on wgpu via glyphon.
//
// Per ADR 0011: cosmic-text is the single source of truth for text shaping
// and metrics. glyphon hosts cosmic-text-shaped runs in a wgpu glyph atlas
// and renders them inside a render pass.
//
// This module is the lowest layer of the text stack.

use anyhow::{Context, Result};
use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, Style,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight,
};
use std::borrow::Cow;
use std::collections::HashMap;

/// The bidi paragraph separators cosmic-text does not end a buffer line at. Unicode ends a bidi paragraph at `\n`,
/// `\r`, U+001C, U+001D, U+001E, U+0085 and U+2029 (bidi class B); cosmic-text 0.12's `Buffer::set_text` ends a line
/// only at `\r` and `\n`, and shaping a line that holds two paragraphs of different directions panics
/// (`ShapeLine::new_in_buffer`).
pub(crate) const PARAGRAPH_SEPARATORS: [char; 5] = ['\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2029}'];

/// `text` with each of `PARAGRAPH_SEPARATORS` a space, so every line cosmic-text shapes is one bidi paragraph. A
/// caller that wants one to end a line makes it `\n` first (`normalize_newlines`, preview/markdown/buffer.rs).
pub(crate) fn one_paragraph_per_line(text: &str) -> Cow<'_, str> {
    if text.contains(PARAGRAPH_SEPARATORS) {
        Cow::Owned(text.replace(PARAGRAPH_SEPARATORS, " "))
    } else {
        Cow::Borrowed(text)
    }
}

/// `Buffer::set_text` through `one_paragraph_per_line`: with `set_buffer_rich_text`, the one way text enters a
/// cosmic-text buffer (rust/clippy.toml's text-into-the-shaper group refuses every other).
pub(crate) fn set_buffer_text(
    buffer: &mut Buffer,
    font_system: &mut FontSystem,
    text: &str,
    attrs: Attrs,
    shaping: Shaping,
) {
    #[allow(clippy::disallowed_methods, reason = "text into the shaper: the one way in, through one_paragraph_per_line")]
    buffer.set_text(font_system, &one_paragraph_per_line(text), attrs, shaping);
}

/// The monospace face's advance (physical px) at `metrics`, measured over ten characters; `None` when nothing shapes
/// (no monospace font installed).
pub(crate) fn monospace_advance(font_system: &mut FontSystem, metrics: Metrics) -> Option<f32> {
    let mut buf = Buffer::new(font_system, metrics);
    buf.set_size(font_system, Some(2000.0), None);
    set_buffer_text(&mut buf, font_system, "xxxxxxxxxx", Attrs::new().family(Family::Monospace), Shaping::Basic);
    buf.shape_until_scroll(font_system, false);
    let total = buf.layout_runs().find_map(|run| run.glyphs.last().map(|last| last.x + last.w).filter(|total| *total > 0.0))?;
    Some(total / 10.0)
}

/// Replace lines `range` of `buffer` with one line for each of `lines`, each a line's text without its end, in `attrs`:
/// the line `set_buffer_rich_text` makes of a span in its default attributes, through `one_paragraph_per_line`, with any
/// CR or LF made a space as well, so each line is one bidi paragraph. The other lines keep their shaping and layout;
/// the caller shapes the new ones (`Buffer::shape_until_scroll`).
pub(crate) fn replace_buffer_lines<'s>(
    buffer: &mut Buffer,
    range: std::ops::Range<usize>,
    lines: impl IntoIterator<Item = &'s str>,
    attrs: Attrs,
) {
    let line = |text: &'s str| {
        let text = one_paragraph_per_line(text).replace(['\r', '\n'], " ");
        #[allow(clippy::disallowed_methods, reason = "text into the shaper: one line through one_paragraph_per_line, no line end in it")]
        glyphon::cosmic_text::BufferLine::new(text, Default::default(), glyphon::cosmic_text::AttrsList::new(attrs), Shaping::Advanced)
    };
    buffer.lines.splice(range, lines.into_iter().map(line));
}

/// `Buffer::set_rich_text` with each span through `one_paragraph_per_line`.
pub(crate) fn set_buffer_rich_text<'r, 's, I>(
    buffer: &mut Buffer,
    font_system: &mut FontSystem,
    spans: I,
    default_attrs: Attrs,
    shaping: Shaping,
) where
    I: IntoIterator<Item = (&'s str, Attrs<'r>)>,
{
    let spans: Vec<(Cow<'s, str>, Attrs<'r>)> =
        spans.into_iter().map(|(text, attrs)| (one_paragraph_per_line(text), attrs)).collect();
    let spans = spans.iter().map(|(text, attrs)| (text.as_ref(), *attrs));
    #[allow(clippy::disallowed_methods, reason = "text into the shaper: the one way in, through one_paragraph_per_line")]
    buffer.set_rich_text(font_system, spans, default_attrs, shaping);
}

/// The runs of glyphs whose `Attrs::metadata` `flag` accepts, as `(x, y, w, h)` in the buffer's own coordinates: one per
/// layout line and unbroken stretch of flagged glyphs, `y` and `h` the line's top and height. A run is the extent of its
/// glyphs that are not a space, so it holds whichever way the line runs (a right-to-left stretch descends in x) and carries
/// nothing for the space that ends a wrapped line, which can hang past the width.
pub(crate) fn flagged_runs(buffer: &Buffer, flag: impl Fn(usize) -> bool) -> Vec<(f32, f32, f32, f32)> {
    let mut rects = Vec::new();
    for run in buffer.layout_runs() {
        let mut current: Option<(f32, f32)> = None;
        let mut end = |current: &mut Option<(f32, f32)>| rects.extend(current.take().map(|(s, e)| (s, run.line_top, e - s, run.line_height)));
        for g in run.glyphs.iter() {
            if !flag(g.metadata) {
                end(&mut current);
            } else if &run.text[g.start..g.end] != " " {
                let (s, e) = (g.x, g.x + g.w.max(0.0));
                current = Some(current.map_or((s, e), |(a, b)| (a.min(s), b.max(e))));
            }
        }
        end(&mut current);
    }
    rects
}

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

    /// The main layer's shaped runs: one buffer per run of the frame, reused by content (`ShapedRuns`).
    runs: ShapedRuns,

    /// Overlay layer: a SECOND glyphon TextRenderer sharing the same
    /// atlas/viewport/font system, rendered by `render_overlay` as a
    /// separate draw AFTER the main `render`. Anything prepared here
    /// paints on top of *everything* in the main pass — image quads AND
    /// main-pass glyphs — which one shared renderer cannot express (one
    /// renderer = one z-plane). First consumer is the nav-spill row
    /// overlay; the layer is general on purpose (tooltips, toasts, ghost
    /// text all want the same "text above the world" plane).
    overlay_renderer: TextRenderer,
    /// Overlay-line runs, kept apart from the main layer's so overlay churn (rows appear/disappear with the spill
    /// timer) never takes the stable chrome rows' buffers.
    overlay_runs: ShapedRuns,

    metrics: Metrics,
}

/// What a buffer was shaped for. The size is in it, so a resized surface matches nothing shaped before.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BufferSig {
    text: String,
    bold: bool,
    italic: bool,
    width: u32,
    height: u32,
}

/// The buffers of one frame's runs, each with the signature it was shaped for. A run takes the buffer the last frame
/// shaped for the same signature, wherever that sat in the frame's list: a ring that moved a row, or a row that gained
/// a run, puts every later run in another slot with the same text. Only a run with no such buffer is shaped, into a
/// buffer the last frame no longer needs. One frame of memory.
#[derive(Default)]
pub(in crate::ui) struct ShapedRuns {
    buffers: Vec<Buffer>,
    sigs: Vec<BufferSig>,
}

impl ShapedRuns {
    /// Make `buffers[i]` the shaping of `lines[i]` at `width` by `height`; the number of runs shaped.
    pub(in crate::ui) fn shape(&mut self, fonts: &mut FontSystem, metrics: Metrics, width: u32, height: u32, lines: &[Line]) -> usize {
        let sigs: Vec<BufferSig> = lines
            .iter()
            .map(|line| BufferSig { text: line.text.clone(), bold: line.bold, italic: line.italic, width, height })
            .collect();
        let mut last: Vec<Option<Buffer>> = std::mem::take(&mut self.buffers).into_iter().map(Some).collect();
        let last_sigs = std::mem::take(&mut self.sigs);
        let mut slots_of: HashMap<&BufferSig, Vec<usize>> = HashMap::new();
        for (slot, sig) in last_sigs.iter().enumerate() {
            slots_of.entry(sig).or_default().push(slot);
        }
        let mut next: Vec<Option<Buffer>> =
            sigs.iter().map(|sig| slots_of.get_mut(sig).and_then(Vec::pop).and_then(|slot| last[slot].take())).collect();
        let mut spare = last.into_iter().flatten();
        let mut shaped = 0;
        for ((slot, sig), line) in next.iter_mut().zip(&sigs).zip(lines) {
            if slot.is_some() {
                continue;
            }
            let mut buf = spare.next().unwrap_or_else(|| Buffer::new(fonts, metrics));
            buf.set_size(fonts, Some(sig.width as f32), Some(sig.height as f32));
            let mut attrs = Attrs::new().family(Family::Monospace);
            if line.bold {
                attrs = attrs.weight(Weight::BOLD);
            }
            if line.italic {
                attrs = attrs.style(Style::Italic);
            }
            set_buffer_text(&mut buf, fonts, &line.text, attrs, Shaping::Advanced);
            buf.shape_until_scroll(fonts, false);
            *slot = Some(buf);
            shaped += 1;
        }
        self.buffers = next.into_iter().flatten().collect();
        self.sigs = sigs;
        shaped
    }

    /// Give every held buffer `metrics`, which re-shapes it in place at its current size; its signature stays right.
    fn set_metrics(&mut self, fonts: &mut FontSystem, metrics: Metrics) {
        for buf in &mut self.buffers {
            buf.set_metrics(fonts, metrics);
        }
    }
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
        // Second renderer on the SAME atlas — glyphon supports N renderers
        // per atlas (each holds only its own vertex buffers), so the
        // overlay shares every cached glyph with the main pass for free.
        let overlay_renderer =
            TextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None);

        Self {
            font_system,
            swash_cache,
            viewport,
            atlas,
            text_renderer,
            overlay_renderer,
            overlay_runs: ShapedRuns::default(),
            runs: ShapedRuns::default(),
            metrics: Metrics::new(14.0 * scale, 18.0 * scale),
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
        self.runs.set_metrics(&mut self.font_system, metrics);
        // Overlay buffers zoom in lockstep — a font-scale change while a
        // spill overlay is up would otherwise render the overlay at the
        // old glyph size until its sig next changed.
        self.overlay_runs.set_metrics(&mut self.font_system, metrics);
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
    pub fn monospace_advance(&mut self) -> Option<f32> { monospace_advance(&mut self.font_system, self.metrics) }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        lines: &[Line],
        extras: &[ExtraArea],
    ) -> Result<()> {
        // Latency instrumentation: how many runs we actually shape this
        // frame vs how many take a buffer an earlier frame shaped for the
        // same content. The keystroke-echo path tends to dirty 1-2 cells in
        // the LLM pane while leaving the other ~30+ Lines untouched, so
        // most frames should report `reshape ≪ total`.
        let prepare_t0 = std::time::Instant::now();
        let reshape_count = self.runs.shape(&mut self.font_system, self.metrics, width, height, lines);
        let skip_count = lines.len() - reshape_count;

        let chrome_areas = self.runs.buffers.iter().zip(lines.iter()).map(|(buf, line)| {
            // Default fg sits at `cells::DEFAULT_FG`, (204, 204, 204) — the VS Code Dark+ /
            // GitHub Dark default-foreground tone. Full-white (238+)
            // looks bright-white over a near-black background and is
            // fatiguing in long sessions; reserve that for explicit ANSI
            // 15 / Color::White from the chrome. DIM ≈ 65% gets you
            // ~133-grey, still above the contrast cliff at 14px mono.
            let (mut r, mut g, mut b) = line.color.unwrap_or(super::cells::DEFAULT_FG);
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

    pub fn render(&self, render_pass: &mut wgpu::RenderPass<'_>) -> Result<()> {
        self.text_renderer
            .render(&self.atlas, &self.viewport, render_pass)
            .context("glyphon render failed")?;
        Ok(())
    }

    /// Shape + upload the overlay layer's lines for this frame. MUST be
    /// called every frame the caller wants overlay output — a glyphon
    /// renderer re-renders its last prepared geometry, so skipping the
    /// call on an "overlay just vanished" frame would ghost the previous
    /// frame's overlay forever. An empty `lines` is the cheap and correct
    /// way to clear (prepare with zero areas). `fade` applies opacity to
    /// lines at or after its index, leaving earlier overlay lines opaque.
    pub fn prepare_overlay(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        lines: &[Line],
        fade: Option<(usize, f32)>,
    ) -> Result<()> {
        self.overlay_runs.shape(&mut self.font_system, self.metrics, width, height, lines);

        let areas: Vec<TextArea> = self
            .overlay_runs
            .buffers
            .iter()
            .zip(lines.iter())
            .enumerate()
            .map(|(index, (buf, line))| {
                // Same default-fg / DIM conventions as the main chrome
                // areas so an overlay row is colour-identical to the
                // underlying row it covers.
                let (mut r, mut g, mut b) = line.color.unwrap_or(super::cells::DEFAULT_FG);
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
                    default_color: Color::rgba(r, g, b, fade.filter(|(start, _)| index >= *start)
                        .map(|(_, opacity)| (opacity * 255.0).round() as u8).unwrap_or(255)),
                    custom_glyphs: &[],
                }
            })
            .collect();

        self.overlay_renderer
            .prepare(
                device,
                queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                areas,
                &mut self.swash_cache,
            )
            .context("glyphon overlay prepare failed")?;
        Ok(())
    }

    /// Draw the overlay layer. Call AFTER `render` (and after any quads
    /// that should sit under the overlay, e.g. its backing strips) — draw
    /// order inside the pass is the z-order.
    pub fn render_overlay(
        &self,
        render_pass: &mut wgpu::RenderPass<'_>,
    ) -> Result<()> {
        self.overlay_renderer
            .render(&self.atlas, &self.viewport, render_pass)
            .context("glyphon overlay render failed")?;
        Ok(())
    }

    pub fn trim(&mut self) {
        self.atlas.trim();
    }
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
