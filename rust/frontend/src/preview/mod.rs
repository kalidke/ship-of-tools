// preview/ — preview-layer surface (parallel to ratatui chrome).
//
// Per ADR 0011: takes a Rect from ratatui's layout pass plus a PreviewPayload
// from the kernel; draws directly into the wgpu surface inside that rect,
// bypassing the cell stream entirely.
//
// Layout this module:
//   quad.rs   — shared wgpu pipeline for textured quads (fed by every renderer
//               that emits a bitmap: png, svg/resvg, future video frames, ...)
//   png.rs    — image::load_from_memory → RGBA8 → wgpu texture → quad
//   (svg.rs, markdown.rs land in subsequent spike steps)

pub mod highlight;
pub mod markdown;
pub mod png;
pub mod quad;
pub mod svg;
