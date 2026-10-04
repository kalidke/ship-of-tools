# rust/frontend/src/ui/preview: the preview pane (fe-ui)

The preview pane draws what the kernel sends for the cursored node: markdown and source text shaped by cosmic-text,
PNG and SVG bitmaps as wgpu quads, and the in-pane editor's buffer. Part of fe-ui; charter: rust/frontend/src/ui/CLAUDE.md.

## Files
- `mod.rs`: declares the subfolders and quad, and re-exports `png`, `svg` and `highlight` at their old `preview::` paths.
- `image/`: image previews (PNG decode, SVG rasterization).
- `markdown/`: markdown and source text shaped by cosmic-text, with tree-sitter highlighting.
- `editor/`: in-pane editing.

## Start here
`markdown/mod.rs` for text previews; `image/png.rs` for bitmaps. The shared quad pipeline is still read from
rust/frontend/src/preview/quad.rs.

## Rules
- The old paths `crate::preview::{markdown, highlight, png, svg, quad}` resolve through this mod.rs's re-exports, because
  main.rs declares `use ui::preview;`.
- quad is read from rust/frontend/src/preview/quad.rs by a `#[path]` attribute until it moves to ui/render/.
