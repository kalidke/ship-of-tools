# rust/frontend/src/ui/preview/image: image previews (fe-ui)

Bitmap previews: each file turns bytes into a `Quad` on the shared textured-quad pipeline. Part of fe-ui; charter:
rust/frontend/src/ui/CLAUDE.md.

## Files
- `mod.rs`: declares the image files and brings the shared `quad` module into scope for them.
- `figures.rs`: decodes markdown-figure fetch replies, tracks failed and pending figures, builds figure metrics and dispatches fetches.
- `keys.rs`: Image preview keys: zoom, pan, reset and the scalebar.
- `overlay.rs`: the figure caption store and the scalebar and caption draw geometry.
- `png.rs`: decodes PNG bytes to RGBA8, fits them to the GPU's texture limit and uploads a quad.
- `roi.rs`: the raster-node test, the ROI capture to `image.crop` and its applied report.
- `svg.rs`: rasterizes SVG bytes with resvg at a caller-given pixel size and uploads a quad.
- `view.rs`: the view geometry: letterbox, zoom bound, pan scaling and the source-pixel ROI mapping both ways.
- `replies.rs`: figure, ROI crop and pixel-size replies

## Start here
`quad_and_source_dims_from_png_bytes` in png.rs for a standalone image; `quad_from_svg_bytes` in svg.rs for math; `solve_roi_view` and `visible_roi_px` in view.rs for zoom, pan and ROI work.

## Rules
- png.rs decodes by sniffing the bytes, not the file name (`with_guessed_format` in its decode path).
- svg.rs rasterizes at exactly the pixel size the caller asks and never fits to the rect (`quad_from_svg_bytes`).
- Every image-geometry consumer takes `image_rect_for_caption`'s rect, never the pane rect.
- An ROI is in source-image pixels, the same on any display (`visible_roi_px`, `solve_roi_view`; ADR 0022).
- One source pixel never grows past 16x16 screen pixels (`png_zoom_max`, `MAX_PX_PER_SRC_PX`).
- The scalebar keys off the source-to-screen mapping, never the raster buffer size (`build_scalebar`; ADR 0034).
- Captions are keyed by (host, listed workspace slug, file); the store keeps at most 256 (CaptionStore). ROI readiness and consumption use the same host-qualified row key.
- A failed figure stays failed until a fresh preview reply clears `figure_failed` (`figure_already_handled`).
- Only a raster node qualifies for crop and scale, never a PDF page (`State::is_image_node_id`).
- Read, write and display use sot_protocol::physical_scale; invalid metadata is absent for fallback selection and cannot hide a valid PNG pHYs scale.
