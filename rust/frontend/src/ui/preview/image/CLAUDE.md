# rust/frontend/src/ui/preview/image: image previews (fe-ui)

Bitmap previews: each file turns bytes into a `Quad` on the shared textured-quad pipeline. Part of fe-ui; charter:
rust/frontend/src/ui/CLAUDE.md.

## Files
- `mod.rs`: declares `png` and `svg` and brings the shared `quad` module into scope for them.
- `png.rs`: decodes PNG bytes to RGBA8, fits them to the GPU's texture limit and uploads a quad.
- `svg.rs`: rasterizes SVG bytes with resvg at a caller-given pixel size and uploads a quad.

## Start here
`quad_and_source_dims_from_png_bytes` in png.rs for a standalone image; `quad_from_svg_bytes` in svg.rs for math.

## Rules
- png.rs decodes by sniffing the bytes, not the file name (`with_guessed_format` in its decode path).
- svg.rs rasterizes at exactly the pixel size the caller asks and never fits to the rect (`quad_from_svg_bytes`).
