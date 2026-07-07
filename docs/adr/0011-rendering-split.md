# ADR 0011: Rendering split — chrome via ratatui, previews via parallel surface

**Status:** Accepted
**Date:** 2026-05-07

## Context

ADR 0003 (revised) committed to rendering pixels ourselves in a native window. The next question is: what is ratatui's job, and what is the rich-content renderer's job?

ratatui's `Backend` trait gives a stream of cells `(char, fg, bg, mods)`. That model fits text chrome well and image/math/markdown not at all — those have semantics that don't reduce to characters. Trying to encode rich content as cells (sentinel chars, side-channel lookups, "pretend this rect is a PNG") fights the model and produces brittle code.

Codex (consulted 2026-05-07) confirmed the split: ratatui as **chrome/layout/input model**; rich previews as a **parallel rendering path** keyed to the layout's rectangles.

## Decision

Two layers share one wgpu surface, one event loop, and one layout pass — but they render through distinct APIs.

**Layer 1: ratatui chrome.** Layout (the mode tree, status bar, modals), borders, labels, focus state, keymaps, scroll indicators, the cursor. Painted via a custom `Backend` impl that maps cells to glyphs in the wgpu canvas using a fixed-width font (cosmic-text). This is everything that already exists in ratatui-style codebases.

**Layer 2: preview-layer surface.** Takes a `Rect` from ratatui's layout and a `PreviewPayload` from the kernel; draws directly into the wgpu surface inside that rect, bypassing the cell stream. Implementations dispatch on `mime`:

- `text/plain` → cosmic-text (or a simpler line breaker) with optional syntax highlighting via `syntect`.
- `text/markdown` → markdown AST → flowed text + inline images + math blocks (math comes back through this same dispatch as `image/svg+xml`).
- `image/png`, `image/jpeg`, `image/webp` → `image` crate decode → wgpu texture.
- `image/svg+xml` (incl. MathJax math output) → `resvg` rasterise → wgpu texture.
- `application/pdf` (future) → `pdfium-rs` or `mupdf-rs` page rasterise → wgpu texture.
- `video/*` (future) → frame stream → texture-per-frame.
- `application/x-wgpu-scene` (future) → plugin draws via a passed `wgpu::RenderPass`.

The two layers compose by Z-order: chrome on top of previews where they overlap (e.g., a focus border is drawn after the preview content fills the rect).

**Interaction:** input events go to ratatui first; if they hit a region owned by a preview widget that registered handlers (e.g., scroll within a markdown view), they get re-dispatched to the preview-layer's handler.

**Fonts and metrics:** cosmic-text is the single source of truth for text shaping/measurement, used by both layers. Cell width = single emoji width = consistent box-drawing alignment, because both go through the same shaper.

## Consequences

- Plugins emit `PreviewPayload(mime, bytes, extras)`. They never see ratatui or the wgpu surface. Adding a new payload type requires registering a renderer in the preview-layer dispatch and possibly a new MIME, no Rust frontend changes for *most* additions.
- `ratatui-image` is explicitly not used. Inline images don't go through cells.
- HiDPI / DPR is the preview-layer's job: rects are in cell coordinates, but wgpu sampling is in device pixels. Conversion is centralised in the preview-layer.
- Damage tracking is tractable because rendering is split: chrome reflows when layout changes; preview surfaces invalidate when their payload or rect changes. Don't redraw the whole frame every event.
- Selection/copy across cells *and* previews is real engineering — deferred past M2 but the data model needs to support text spans crossing the layer boundary (e.g., copying from a markdown preview into the system clipboard).
- Accessibility is deferred but tracked. The dual-layer model makes it harder than a single-tree GUI; revisit when M5+ lands.
