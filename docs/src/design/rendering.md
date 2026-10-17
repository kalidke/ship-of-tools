# Frontend Rendering

!!! note "Status: built"
    The window, its two drawing layers and the preview renderers in the table below are built. Video does not play in
    the pane (the pane shows a poster frame), and nothing renders full LaTeX documents.

The window draws every pixel itself. It is a
native program, not a terminal program. No terminal graphics
protocol delivers acceptable rich previews through ssh and a terminal multiplexer, and
the universally-supported fallback (halfblocks) is too low-fidelity for figures, math, and
rendered docs, so none is used.

For where rendering sits in the whole system see
[Architecture at a Glance](../guide/architecture.md); for what each preview kind
looks like in use see [Previews](../guide/previews.md).

## A native window, pixels we own

The frontend is a **native Rust window** (`winit`) with a GPU-backed canvas
(`wgpu`). It writes no escape sequences and is not hosted in a terminal. Image
fidelity is bounded by what the host GPU can display, not by what the terminal
stack happens to forward. A PNG file round-trips as: the daemon reads the bytes → forwards
them over the [line protocol](protocol.md) → frontend decodes via the `image`
crate → uploads as a wgpu texture → composites into the preview rectangle. (The kernel emits PNG only for a PDF page or
a video poster frame; a REPL figure arrives as an `image` frame from the REPL.)

## Two layers, one surface

ratatui is **chrome only**. Its `Backend` trait emits a stream of cells —
`(char, fg, bg, mods)` — which fits text chrome and fits images, math, and
markdown not at all. Rather than smuggle rich content through cells (sentinel
chars, "pretend this rect is a PNG"), Ship of Tools runs two layers that share one wgpu
surface and one event loop but render through distinct APIs:

**Layer 1 — ratatui chrome.** The mode-tree layout, status bar, modals, borders,
labels, focus state, keymaps, scroll indicators, the cursor. Painted by a custom
`Backend` impl that maps cells to glyphs in the wgpu canvas with a fixed-width
font (cosmic-text) — explicitly *not* crossterm.

**Layer 2 — the preview surface.** Takes a `Rect` from ratatui's layout and a
`PreviewPayload` from the daemon and draws directly into the wgpu surface inside
that rect, bypassing the cell stream. It dispatches on the payload's `mime`:

| MIME | Renderer |
|------|----------|
| `text/plain` and other `text/*` except markdown | cosmic-text line layout |
| `application/vnd.sot.tokens+json` | pre-tokenized spans (Julia source), colored by kind |
| `text/markdown` | `comrak` AST → flowed text, tables, inline images and math; fenced code highlighted with tree-sitter |
| `image/png`, `image/jpeg`, `image/gif`, `image/webp`, `image/bmp` | `image` decode → wgpu texture |
| `image/svg+xml` (incl. MathJax math) | `resvg` rasterize → wgpu texture |

PDF pages and video posters arrive as `image/png`: the PDF plugin rasterizes one page with poppler and the video
plugin extracts one poster frame with ffmpeg, both on the host where the file lives. Video does not play in the pane;
see [Previews](../guide/previews.md).

Chrome text and markdown shape with one cosmic-text `FontSystem`, so text shaping and measurement agree across both
layers.

A consequence worth stating: plugins emit `PreviewPayload(mime, bytes, extras)`
and never see ratatui or the wgpu surface. Adding a payload type is adding a renderer
to the preview dispatch (`render_preview_source` in `rust/frontend/src/ui/preview/pane.rs`).

## Inline math — the MathJax sidecar

Inline math is rendered **server-side to MathJax SVG** and shipped to the frontend
as `image/svg+xml`, which the preview layer rasterizes through `resvg`.
MathJax is the choice because it
emits SVG; KaTeX outputs HTML, which there is no path for here. The sidecar is a
small Node process (one per daemon, started on the first render) on the backend host, running `mathjax-full`. The
frontend has no Node dependency.

## The stack

The libraries that implement both layers:

| Concern | Choice |
|---------|--------|
| Window / event loop | `winit` |
| GPU surface | `wgpu` |
| Text shaping / layout | `cosmic-text`, drawn through a `glyphon` atlas |
| Raster decode | `image` |
| SVG raster | `resvg` |
| Inline math | MathJax SVG via a Node sidecar |
| Markdown parse | `comrak` (CommonMark + GFM) |
| Syntax highlighting | `tree-sitter-highlight` (Julia, Rust, Python, Bash, JSON and TOML grammars) |
| Chrome model | `ratatui` (custom `Backend`, `WgpuBackend`) |

## What is deliberately rejected

- **Sixel, iTerm2 and the other terminal image protocols, halfblocks: none, anywhere.**
- **No browser/webview** in the chosen path. ADR 0012 names a Tauri/wry web frontend
  only as the option-B fallback — taken only if preview requirements broaden into
  arbitrary HTML / iframes / WebGL. Across that swap the kernel, backend, line
  protocol, and core IR are unchanged; only the frontend surface differs.
