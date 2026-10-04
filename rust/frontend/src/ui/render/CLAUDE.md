# rust/frontend/src/ui/render: the window's pixels (fe-ui)

Everything that turns state into pixels without knowing what a pane or a mode is: glyph text shaped by cosmic-text and
drawn from a glyphon atlas, the ratatui cell backend, and the textured-quad pipeline. Part of fe-ui; charter:
rust/frontend/src/ui/CLAUDE.md. ADRs 0003, 0011 and 0012 give the design.

## Files
- `mod.rs`: declares the files below.
- `text.rs`: `TextLayer`, the glyph-atlas text layer and its one `FontSystem`.
- `cells.rs`: `WgpuBackend`, the ratatui `Backend` that keeps a cell grid, and its projection into text lines.
- `quad.rs`: `QuadPipeline`, the textured-quad pipeline every bitmap goes through.

## Start here
`text.rs` for how text is shaped and drawn; `quad.rs` for bitmaps. The render-pass order is still in `State::redraw`
in ../mod.rs.

## Rules
- Chrome text and markdown shape with one `FontSystem`, lent by `TextLayer::font_system_mut`.
- Bitmaps are drawn as quads, never through the cell grid: `QuadPipeline` draws them and `WgpuBackend` holds only cells.
- Old paths `crate::text` and `crate::chrome` resolve through `use` lines in main.rs; `crate::chrome` is `cells.rs`.
