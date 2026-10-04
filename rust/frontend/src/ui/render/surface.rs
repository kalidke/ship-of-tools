//! The GPU surface and the cell grid on it: base cell metrics, the startup logos, the clear colour,
//! the chrome grid, the resize and text-scale methods, and what launch builds on the surface
//! (device and queue, text layer and grid, solid-colour quads, logos).

use super::*;
use crate::ui::init::CellMetrics;

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

pub(in crate::ui) struct GpuSurface {
    pub(in crate::ui) surface: wgpu::Surface<'static>,
    pub(in crate::ui) device: wgpu::Device,
    pub(in crate::ui) queue: wgpu::Queue,
    pub(in crate::ui) config: wgpu::SurfaceConfiguration,
    pub(in crate::ui) surface_format: wgpu::TextureFormat,
}

pub(in crate::ui) fn create_gpu_surface(window: &Arc<Window>, settings: &Settings) -> Result<GpuSurface> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..Default::default()
    });

    let surface = instance
        .create_surface(window.clone())
        .context("failed to create wgpu surface")?;

    // We draw glyph quads and image blits — a 2D workload an integrated
    // GPU handles fine — so the default is LowPower. Asking for the
    // discrete adapter on a hybrid-graphics laptop keeps it awake for the
    // whole session (~11 W measured on an idle RTX 4070, 2026-07-31); an
    // awake dGPU cannot power-gate. `[gpu] power_preference = "high"`
    // opts back in for desktops with a real GPU. No-op on single-adapter
    // machines, where HighPerformance already resolved to the iGPU.
    // Binds once, here — changing the key needs an FE restart.
    let power_preference = match settings.gpu_power_preference {
        crate::settings::GpuPowerPreference::Low => wgpu::PowerPreference::LowPower,
        crate::settings::GpuPowerPreference::High => wgpu::PowerPreference::HighPerformance,
    };
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference,
        compatible_surface: Some(&surface),
        force_fallback_adapter: false,
    }))
    .context("no compatible wgpu adapter found")?;
    tracing::info!(
        requested = ?settings.gpu_power_preference,
        adapter = %adapter.get_info().name,
        device_type = ?adapter.get_info().device_type,
        backend = ?adapter.get_info().backend,
        "wgpu adapter selected"
    );

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("sot-device"),
            required_features: wgpu::Features::empty(),
            required_limits:
                wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            memory_hints: wgpu::MemoryHints::Performance,
        },
        None,
    ))
    .context("failed to request wgpu device")?;

    let size = window.inner_size();
    let surface_caps = surface.get_capabilities(&adapter);
    let surface_format = surface_caps
        .formats
        .iter()
        .copied()
        .find(|f| f.is_srgb())
        .unwrap_or(surface_caps.formats[0]);

    let config = wgpu::SurfaceConfiguration {
        // COPY_SRC enables the `--capture` readback path. Cheap when
        // unused; widely supported on the wgpu backends we care about
        // (DX12/Vulkan/Metal).
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        format: surface_format,
        width: size.width.max(1),
        height: size.height.max(1),
        // AutoVsync, NOT `present_modes[0]`: the capability list's order
        // is driver-specific, so [0] picked a non-vsync mode (Immediate/
        // Mailbox) on some GPUs — visible flicker/tearing since day one on
        // those machines, worst in fullscreen where DWM composition stops
        // masking it (a Windows FE box finding, 2026-07-12; the same build was clean
        // on hardware whose driver lists Fifo first). AutoVsync =
        // FifoRelaxed where supported, else Fifo — vsynced on every
        // backend.
        present_mode: wgpu::PresentMode::AutoVsync,
        alpha_mode: surface_caps.alpha_modes[0],
        view_formats: vec![],
        // One queued frame fewer between input and photon.
        desired_maximum_frame_latency: 1,
    };
    surface.configure(&device, &config);
    Ok(GpuSurface { surface, device, queue, config, surface_format })
}

pub(in crate::ui) struct TextGrid {
    pub(in crate::ui) text: TextLayer,
    pub(in crate::ui) cell_w: f32,
    pub(in crate::ui) terminal: Terminal<WgpuBackend>,
}

pub(in crate::ui) fn build_text_grid(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    config: &wgpu::SurfaceConfiguration,
    surface_format: wgpu::TextureFormat,
    metrics: CellMetrics,
) -> Result<TextGrid> {
    let CellMetrics { scale, cell_h, chrome_origin_x, chrome_origin_y } = metrics;
    let mut text = TextLayer::new(&device, &queue, surface_format, scale);
    text.resize(&queue, config.width, config.height);

    // Derive cell_w from the actual monospace glyph advance instead
    // of BASE_CELL_W = 9.0 — the static constant didn't match cosmic-
    // text's real advance (~7.7px for Consolas at 14pt), and the gap
    // grew with column count, making the cursor visibly outpace the
    // typed text in REPL / LLM panes. Fall back to the constant on
    // shape failure so a missing monospace font doesn't kill startup.
    let measured = text.monospace_advance();
    let cell_w = measured.unwrap_or(BASE_CELL_W * scale);
    tracing::info!(
        measured = measured.unwrap_or(0.0),
        cell_w,
        "monospace advance measured"
    );

    let (cols, rows) = cell_grid_for(
        config.width,
        config.height,
        cell_w,
        cell_h,
        chrome_origin_x,
        chrome_origin_y,
    );
    let backend = WgpuBackend::new(cols, rows);
    let terminal = Terminal::new(backend)
        .context("failed to construct ratatui Terminal over WgpuBackend")?;
    Ok(TextGrid { text, cell_w, terminal })
}

pub(in crate::ui) struct SolidQuads {
    pub(in crate::ui) quad_pipeline: QuadPipeline,
    pub(in crate::ui) selection_bg_quad: Quad,
    pub(in crate::ui) overlay_back_quad: Quad,
    pub(in crate::ui) code_bg_quad: Quad,
    pub(in crate::ui) code_border_quad: Quad,
    pub(in crate::ui) strike_line_quad: Quad,
    pub(in crate::ui) scalebar_bar_quad: Quad,
    pub(in crate::ui) scalebar_back_quad: Quad,
    pub(in crate::ui) caption_back_quad: Quad,
}

pub(in crate::ui) fn build_solid_quads(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    surface_format: wgpu::TextureFormat,
) -> Result<SolidQuads> {
    let quad_pipeline = QuadPipeline::new(&device, surface_format);
    // 1×1 translucent yellow texture for the LLM-pane selection
    // highlight. Alpha 140 (~55%) lets the chrome's default-fg light
    // text on the dark bg remain legible through the tint — opaque
    // yellow would either bleach the (204,204,204) fg into a yellow-
    // green blur or force a fg-colour switch that the chrome
    // pipeline doesn't currently carry through selection state.
    let selection_bg_quad =
        Quad::from_rgba8(&device, &queue, &quad_pipeline, &[252, 240, 130, 140], 1, 1)
            .context("failed to build selection_bg_quad")?;
    // Nav-spill overlay backing: the surface's deep-navy tone
    // ((0.020, 0.035, 0.090) visible-srgb ≈ (5, 9, 23) u8) at
    // NAV_SPILL_BACK_ALPHA so the strip reads as the nav background
    // continuing over the preview, with imagery barely ghosting
    // through. Alpha is a tune-by-eye knob.
    let overlay_back_quad = Quad::from_rgba8(
        &device,
        &queue,
        &quad_pipeline,
        &[5, 9, 23, NAV_SPILL_BACK_ALPHA],
        1,
        1,
    )
    .context("failed to build overlay_back_quad")?;
    // Markdown code-bg panel. (52, 60, 92, 230) is VS-Code Dark+'s
    // `#1e1e1e`-leaning panel tone lifted a touch toward the
    // chrome's midnight-navy surface so the panel reads as "lifted
    // off the page" without looking glued to the deep-navy bg.
    // Slight alpha (230/255) softens the edge against the antialias
    // halo of surrounding non-code glyphs.
    let code_bg_quad =
        Quad::from_rgba8(&device, &queue, &quad_pipeline, &[52, 60, 92, 230], 1, 1)
            .context("failed to build code_bg_quad")?;
    // Code-block border — one step lighter than the bg quad so the
    // outline reads against the panel's slate fill. Full alpha
    // because a soft border just looks fuzzy at 1 px width.
    let code_border_quad =
        Quad::from_rgba8(&device, &queue, &quad_pipeline, &[88, 100, 140, 255], 1, 1)
            .context("failed to build code_border_quad")?;
    // Strikethrough line — matches the default-fg tone so the
    // line reads "the same colour as the text it's crossing
    // through" without per-glyph colour lookups. Alpha is full so
    // the line stands out crisply against the navy bg.
    let strike_line_quad =
        Quad::from_rgba8(&device, &queue, &quad_pipeline, &[204, 204, 204, 255], 1, 1)
            .context("failed to build strike_line_quad")?;
    // ADR 0034 scalebar: an opaque white bar over a translucent-black
    // backing box so the overlay reads on light OR dark rasters.
    let scalebar_bar_quad =
        Quad::from_rgba8(&device, &queue, &quad_pipeline, &[255, 255, 255, 255], 1, 1)
            .context("failed to build scalebar_bar_quad")?;
    let scalebar_back_quad =
        Quad::from_rgba8(&device, &queue, &quad_pipeline, &[0, 0, 0, 170], 1, 1)
            .context("failed to build scalebar_back_quad")?;
    // Caption backing: the same translucent black, a touch more opaque —
    // it sits under prose (which needs more contrast to stay readable than
    // a solid white bar does) and spans the pane width.
    let caption_back_quad =
        Quad::from_rgba8(&device, &queue, &quad_pipeline, &[0, 0, 0, 200], 1, 1)
            .context("failed to build caption_back_quad")?;
    Ok(SolidQuads {
        quad_pipeline,
        selection_bg_quad,
        overlay_back_quad,
        code_bg_quad,
        code_border_quad,
        strike_line_quad,
        scalebar_bar_quad,
        scalebar_back_quad,
        caption_back_quad,
    })
}

pub(in crate::ui) struct LogoQuads {
    pub(in crate::ui) logo_quad: Option<(Quad, u32, u32)>,
    pub(in crate::ui) wordmark_quad: Option<(Quad, u32, u32)>,
}

pub(in crate::ui) fn decode_logo_quads(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    quad_pipeline: &QuadPipeline,
) -> LogoQuads {
    // Decode the two embedded brand logos into textured quads. Both are
    // purely cosmetic chrome (strip badge flankers + nav wordmark), so a
    // decode/upload failure must NOT abort frontend startup — log a warning
    // and leave the field None; the affected draw is then simply skipped.
    // `quad_and_dims_from_bytes` builds a Linear-sampled quad (smooth
    // downscale) and returns the native (w, h) for aspect-ratio sizing.
    let logo_quad = match crate::preview::png::quad_and_dims_from_bytes(
        &device,
        &queue,
        &quad_pipeline,
        LOGO_DARK_PNG,
    ) {
        Ok((q, w, h)) => Some((q, w, h)),
        Err(e) => {
            tracing::warn!(error = %e, "failed to decode logo-dark.png; strip logos disabled");
            None
        }
    };
    let wordmark_quad = match crate::preview::png::quad_and_dims_from_bytes(
        &device,
        &queue,
        &quad_pipeline,
        LOGO_WORDMARK_PNG,
    ) {
        Ok((q, w, h)) => Some((q, w, h)),
        Err(e) => {
            tracing::warn!(error = %e, "failed to decode logo-wordmark-dark.png; nav wordmark disabled");
            None
        }
    };
    LogoQuads { logo_quad, wordmark_quad }
}
