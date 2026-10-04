# rust/frontend/src/ui/render: the window's pixels (fe-ui)

Everything that turns state into pixels without knowing what a pane or a mode is: glyph text shaped by cosmic-text and
drawn from a glyphon atlas, the ratatui cell backend, the textured-quad pipeline, the surface's size and cell grid, and
frame capture. Part of fe-ui; charter:
rust/frontend/src/ui/CLAUDE.md. ADRs 0003, 0011 and 0012 give the design.

## Files
- `mod.rs`: declares the files and the folder below and re-exports `surface` and `capture` into `ui`.
- `text.rs`: `TextLayer`, the glyph-atlas text layer and its one `FontSystem`.
- `cells.rs`: `WgpuBackend`, the ratatui `Backend` that keeps a cell grid, and its projection into text lines.
- `quad.rs`: `QuadPipeline`, the textured-quad pipeline every bitmap goes through.
- `surface.rs`: base cell metrics, startup logos, `cell_grid_for`, `clear_color_for_surface`, `State::resize` and
  `State::apply_text_scale`, and what `State::new` builds on the surface (`create_gpu_surface`, `build_text_grid`,
  `build_solid_quads`, `decode_logo_quads`).
- `capture.rs`: the `--capture` trigger frame, `selfie_path`, and the texture readback to PNG (`stage_capture`,
  `finish_capture`).
- `pass/`: the frame's render pass, one `State` method per section (its own page).

## Start here
`text.rs` for how text is shaped and drawn; `quad.rs` for bitmaps; `surface.rs` for the cell grid. The render-pass order
is in `State::redraw` in ../app/frame.rs, and the sections it calls are in `pass/`.

## Rules
- Chrome text and markdown shape with one `FontSystem`, lent by `TextLayer::font_system_mut`.
- Bitmaps are drawn as quads, never through the cell grid: `QuadPipeline` draws them and `WgpuBackend` holds only cells.
- The chrome grid holds back the strip's rows once, in `cell_grid_for`.
- The clear colour is converted for the surface format in `clear_color_for_surface`.
- Old paths `crate::text` and `crate::chrome` resolve through `use` lines in main.rs; `crate::chrome` is `cells.rs`.
