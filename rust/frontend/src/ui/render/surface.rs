//! The surface's size and cell grid: base cell metrics, the startup logos, the clear colour,
//! the chrome grid for a window, and the resize and text-scale methods that re-derive it.

use super::*;

/// Dark square-ish app logo, embedded at build time from the repo root.
/// Drawn miniature flanking each session badge in the bottom strip. Cosmetic
/// only — a decode failure leaves `State::logo_quad` None and the strip renders
/// exactly as before.
pub(in crate::ui) const LOGO_DARK_PNG: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../logo-dark.png"));
/// Wide full-text "wordmark" logo, embedded at build time from the repo root.
/// Drawn small at the top-left of the nav pane. Cosmetic only — a decode
/// failure leaves `State::wordmark_quad` None.
pub(in crate::ui) const LOGO_WORDMARK_PNG: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../logo-wordmark-dark.png"));

/// Base cell metrics in physical pixels at 1.0 scale. State multiplies these
/// by the effective scale (`cli.scale * window.scale_factor()`) at startup.
/// Monospace 14 px / 18 px line height yields roughly 8.4 advance for most
/// fonts; we round to 9 so cells align cleanly with integer pixel positions.
/// cosmic-text-derived metrics will replace these constants once the font
/// system is queried directly.
pub(in crate::ui) const BASE_CELL_W: f32 = 9.0;
pub(in crate::ui) const BASE_CELL_H: f32 = 18.0;
pub(in crate::ui) const BASE_CHROME_ORIGIN_X: f32 = 12.0;
pub(in crate::ui) const BASE_CHROME_ORIGIN_Y: f32 = 12.0;

/// Translate a "desired visible sRGB colour" into the `wgpu::Color` that
/// `LoadOp::Clear` should carry, so the same painted bg appears the same
/// across machines.
///
/// Why: wgpu interprets clear values per the surface format. On an sRGB
/// target (`Bgra8UnormSrgb`) the value is treated as **linear-light** and
/// the hardware sRGB-encodes it on write — so a `0.02` clear comes out
/// at sRGB ≈ `#272727` (dark gray). On a non-sRGB target (`Bgra8Unorm`)
/// the value is the literal pixel — same `0.02` lands as `#050505`
/// (near-black). Without this conversion, two machines whose adapters
/// happen to land on different swapchain formats render the chrome at
/// different brightness levels.
pub(in crate::ui) fn clear_color_for_surface(visible_srgb: (f64, f64, f64), is_srgb_target: bool) -> wgpu::Color {
    let convert = |c: f64| {
        if !is_srgb_target {
            c
        } else if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    wgpu::Color {
        r: convert(visible_srgb.0),
        g: convert(visible_srgb.1),
        b: convert(visible_srgb.2),
        a: 1.0,
    }
}

/// Chrome grid (cols, rows) for a window, with the bottom session strip's
/// band held back: `strip_reserved_rows` comes off `rows` HERE, once, because
/// the strip floats off `config.height` while everything the chrome draws
/// floats off this grid — including the FE/BE version stamp on the bottom
/// border line, which the strip's names row otherwise lands on top of. The
/// owner pre-authorised moving the bottom pane boundary up for exactly this.
pub(in crate::ui) fn cell_grid_for(
    width: u32,
    height: u32,
    cell_w: f32,
    cell_h: f32,
    ox: f32,
    oy: f32,
) -> (u16, u16) {
    let cols = ((width as f32 - 2.0 * ox).max(0.0) / cell_w).floor() as u16;
    let rows = ((height as f32 - 2.0 * oy).max(0.0) / cell_h).floor() as u16;
    let rows = rows.saturating_sub(strip_reserved_rows(cell_h, oy));
    (cols.max(1), rows.max(1))
}

/// Resolve a Sessions row's agent state from its node payload into the render
/// tone plus a wilt flag (true = active state gone stale). `None` when there
/// is no agent state to show, so the row renders exactly as it did before
/// state-nav. `now` is injected so the staleness check stays unit-testable.
/// Built-in monitor-width tier for the startup font-scale SEED — used only
/// when no per-host persisted zoom and no `[font] scale` settings key exist.
/// Wide displays read better a notch larger (maintainer note, 2026-07-03: 1.1 on a
/// 4096×1728 @ 96 DPI ultrawide; 3440 catches the common ultrawide widths).
/// Physical pixels, pre-DPR — DPR scaling is already applied separately.
pub(in crate::ui) fn default_font_scale_for_width(width_px: u32) -> f32 {
    if width_px >= 3440 {
        1.1
    } else {
        1.0
    }
}

impl State {
    /// Bump the runtime font-scale multiplier and propagate. Recomputes
    /// chrome cell metrics, updates the TextLayer's per-line metrics,
    /// and replays the cached preview source so an open .jl / .md
    /// reflows at the new size. Clamped to a sane range so the user
    /// can't soft-lock the chrome by zooming to 0.01.
    pub(in crate::ui) fn apply_text_scale(&mut self, mult: f32) {
        self.text_scale_mult = mult.clamp(0.5, 3.0);
        let s = self.scale * self.text_scale_mult;
        self.cell_h = BASE_CELL_H * s;
        self.chrome_origin_x = BASE_CHROME_ORIGIN_X * s;
        self.chrome_origin_y = BASE_CHROME_ORIGIN_Y * s;
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
        let (cols, rows) = cell_grid_for(
            self.config.width,
            self.config.height,
            self.cell_w,
            self.cell_h,
            self.chrome_origin_x,
            self.chrome_origin_y,
        );
        self.terminal.backend_mut().resize(cols, rows);
        let _ = self
            .terminal
            .resize(ratatui::layout::Rect::new(0, 0, cols, rows));
        // Replay the cached preview source — without this the open
        // file would stay at its original scale until the user
        // navigated to a different file.
        if let Some((mime, bytes)) = self.preview_src.clone() {
            self.render_preview_source(&mime, &bytes);
        }
    }

    pub(in crate::ui) fn resize(&mut self, new_size: PhysicalSize<u32>) {
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }
        self.config.width = new_size.width;
        self.config.height = new_size.height;
        self.surface.configure(&self.device, &self.config);
        self.text
            .resize(&self.queue, self.config.width, self.config.height);

        let (cols, rows) = cell_grid_for(
            self.config.width,
            self.config.height,
            self.cell_w,
            self.cell_h,
            self.chrome_origin_x,
            self.chrome_origin_y,
        );
        self.terminal.backend_mut().resize(cols, rows);
        // ratatui needs to know the grid changed so it reallocates its buffers.
        let _ = self
            .terminal
            .resize(ratatui::layout::Rect::new(0, 0, cols, rows));
    }
}
