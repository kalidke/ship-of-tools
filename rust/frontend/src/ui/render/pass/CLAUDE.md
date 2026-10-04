# rust/frontend/src/ui/render/pass: the frame's render pass (fe-ui)

One wgpu render pass draws the whole frame: preview bitmaps first, the chrome text over them, the overlays last. Each
section of that pass is a `State` method called in order by `State::redraw`, so the order in `redraw` is the z-order;
the preview pane's sections are here. Part of fe-ui; charter: rust/frontend/src/ui/CLAUDE.md.

## Files
- `mod.rs`: declares the files below.
- `preview.rs`: the preview pane's draws: the PNG canvas and its ROI bookkeeping (`paint_preview_png`), the scalebar and
  caption bands, math and figure blocks, the code panels and borders, and the strike lines.

## Start here
`State::redraw` in ../../app/frame.rs for the order of the pass; `paint_preview_png` in preview.rs for a change to how an
image is placed, zoomed or clipped.

## Rules
- A draw that scissors to a pane resets the scissor to the whole surface before it returns, so no later draw is clipped
  (`paint_preview_png`, `paint_figure_bands`, `paint_media_blocks`).
- A batch of rects is one `Quad::render_many`; a per-rect `render` loop would leave only the last rect in the quad's
  vertex buffer (`paint_code_panels`, `paint_code_borders`, `paint_strike_lines`).
- The image view's ROI is recomputed every frame from the pane geometry, and cleared when no image shows
  (`paint_preview_png`).
