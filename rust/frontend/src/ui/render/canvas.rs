//! The text canvas: the glyph text layer, the chrome's cell grid (ratatui over the cell backend) and the cell
//! metrics both are laid out with.

use anyhow::Result;
use ratatui::Terminal;

use super::cells::WgpuBackend;
use super::gpu::Gpu;
use super::surface::{cell_grid_for, BASE_CELL_H, BASE_CELL_W, BASE_CHROME_ORIGIN_X, BASE_CHROME_ORIGIN_Y};
use super::text::{ExtraArea, Line, TextLayer};

/// The render resources' text stage. The cell metrics are a function of the startup scale and the text-scale
/// multiplier, the monospace advance measured at them; the chrome grid is re-derived from the surface size whenever
/// either changes, so the cell count never disagrees with the cell size. Shaping, preparing and painting text are its
/// operations.
pub(in crate::ui) struct Canvas {
    text: TextLayer,
    terminal: Terminal<WgpuBackend>,
    /// Combined multiplier (`cli.scale * window.scale_factor()`) applied to
    /// all text + cell metrics. Captured once at startup; ScaleFactorChanged
    /// is currently ignored.
    scale: f32,
    /// Runtime font-scale multiplier on top of `scale`. Bumped via `Ctrl+=` / `Ctrl+-` (reset by `Ctrl+0`). Affects
    /// chrome cell metrics, the TextLayer's per-line metrics, and the
    /// preview's flowed-text buffer simultaneously (user picked
    /// "Global only" — same change applies to every pane).
    text_scale_mult: f32,
    cell_w: f32,
    cell_h: f32,
    origin_x: f32,
    origin_y: f32,
}

impl Canvas {
    pub(in crate::ui) fn new(
        text: TextLayer, terminal: Terminal<WgpuBackend>, scale: f32, cell_w: f32, cell_h: f32, origin_x: f32,
        origin_y: f32,
    ) -> Self {
        Self { text, terminal, scale, text_scale_mult: 1.0, cell_w, cell_h, origin_x, origin_y }
    }

    pub(in crate::ui) fn scale(&self) -> f32 { self.scale }
    pub(in crate::ui) fn text_scale_mult(&self) -> f32 { self.text_scale_mult }
    pub(in crate::ui) fn cell_w(&self) -> f32 { self.cell_w }
    pub(in crate::ui) fn cell_h(&self) -> f32 { self.cell_h }
    pub(in crate::ui) fn origin_x(&self) -> f32 { self.origin_x }
    pub(in crate::ui) fn origin_y(&self) -> f32 { self.origin_y }

    /// Set the runtime font-scale multiplier (clamped to 0.5..=3 so the chrome cannot be zoomed out of use) on a
    /// `width` by `height` surface: the cell metrics, the text layer's line metrics and the chrome grid follow.
    pub(in crate::ui) fn apply_text_scale(&mut self, mult: f32, width: u32, height: u32) {
        self.text_scale_mult = mult.clamp(0.5, 3.0);
        let s = self.scale * self.text_scale_mult;
        self.cell_h = BASE_CELL_H * s;
        self.origin_x = BASE_CHROME_ORIGIN_X * s;
        self.origin_y = BASE_CHROME_ORIGIN_Y * s;
        self.text
            .set_metrics(cosmic_text::Metrics::new(14.0 * s, 18.0 * s));
        // Re-measure monospace advance at the new metrics so the cell
        // grid still matches cosmic-text's actual glyph positioning
        // after a runtime font-size change. Fall back to BASE_CELL_W * s
        // if shape fails (no monospace font installed).
        self.cell_w = self.text.monospace_advance().unwrap_or(BASE_CELL_W * s);
        // Re-derive the chrome grid against the new cell metrics —
        // without this the cell count (cols, rows) stays at the old
        // value while each cell paints at the new (bigger) size, and
        // content extends past the wgpu surface edge. The window
        // doesn't resize; the grid does.
        self.rederive_grid(width, height);
    }

    /// The surface's new size: the text viewport and the chrome grid follow.
    pub(in crate::ui) fn resize(&mut self, queue: &wgpu::Queue, width: u32, height: u32) {
        self.text.resize(queue, width, height);
        self.rederive_grid(width, height);
    }

    /// Recompute the chrome grid from the surface size and cell metrics and tell ratatui.
    fn rederive_grid(&mut self, width: u32, height: u32) {
        let (cols, rows) = cell_grid_for(width, height, self.cell_w, self.cell_h, self.origin_x, self.origin_y);
        self.terminal.backend_mut().resize(cols, rows);
        // ratatui needs to know the grid changed so it reallocates its buffers.
        let _ = self
            .terminal
            .resize(ratatui::layout::Rect::new(0, 0, cols, rows));
    }

    /// The shaping stage: `shape` builds or resizes a buffer with the canvas's font system.
    pub(in crate::ui) fn shape<T>(&mut self, shape: impl FnOnce(&mut glyphon::FontSystem) -> T) -> T {
        shape(self.text.font_system_mut())
    }

    /// The chrome stage: ratatui draws one frame into the cell backend.
    pub(in crate::ui) fn draw_chrome(&mut self, paint: impl FnOnce(&mut ratatui::Frame)) -> std::io::Result<()> {
        self.terminal.draw(paint).map(|_| ())
    }

    /// The cell backend the last chrome frame drew into, for projecting its lines and borders to pixels.
    pub(in crate::ui) fn chrome(&self) -> &WgpuBackend { self.terminal.backend() }

    /// The chrome grid's size in cells.
    pub(in crate::ui) fn grid_size(&self) -> std::io::Result<ratatui::layout::Size> { self.terminal.size() }

    /// Prepare the frame's text: the chrome lines and the extra areas (preview, captions, scalebar, tables).
    pub(in crate::ui) fn prepare_text(&mut self, gpu: &Gpu, lines: &[Line], extras: &[ExtraArea]) -> Result<()> {
        self.text.prepare(gpu.device(), gpu.queue(), gpu.config().width, gpu.config().height, lines, extras)
    }

    /// Prepare the overlay text (nav spill, help peek, close dialog), the lines from `fade.0` on at `fade.1`.
    pub(in crate::ui) fn prepare_overlay(&mut self, gpu: &Gpu, lines: &[Line], fade: Option<(usize, f32)>) -> Result<()> {
        self.text.prepare_overlay(gpu.device(), gpu.queue(), gpu.config().width, gpu.config().height, lines, fade)
    }

    pub(in crate::ui) fn render_text(&self, rpass: &mut wgpu::RenderPass<'_>) -> Result<()> { self.text.render(rpass) }
    pub(in crate::ui) fn render_overlay(&self, rpass: &mut wgpu::RenderPass<'_>) -> Result<()> {
        self.text.render_overlay(rpass)
    }

    /// After a frame: the glyph atlas drops what it did not use.
    pub(in crate::ui) fn trim(&mut self) { self.text.trim(); }
}
