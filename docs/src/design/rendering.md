# Frontend Rendering

Rendering everything ourselves is the project's reason to exist. The user's
primary deployment is Windows local → SSH → tmux on a Linux remote, and no
terminal graphics protocol delivers acceptable rich previews through that stack:
tmux strips DCS by default, passthrough is fragile, and the universally-supported
fallback (halfblocks) is too low-fidelity for figures, math, and rendered docs.
If a terminal path *did* work, the user would be running yazi-in-tmux. Designing
around terminal graphics would invert the premise.

For where rendering sits in the whole system see
[Architecture at a Glance](../guide/architecture.md); for what each preview kind
looks like in use see [Previews](../guide/previews.md).

## A native window, pixels we own

The frontend is a **native Rust window** (`winit`) with a GPU-backed canvas
(`wgpu`). It writes no escape sequences and is not hosted in a terminal. Image
fidelity is bounded by what the host GPU can display, not by what the terminal
stack happens to forward. A PNG round-trips as: kernel emits bytes → backend
forwards over the [line protocol](protocol.md) → frontend decodes via the `image`
crate → uploads as a wgpu texture → composites into the preview rectangle.

## Two layers, one surface

ratatui is **chrome only**. Its `Backend` trait emits a stream of cells —
`(char, fg, bg, mods)` — which fits text chrome and fits images, math, and
markdown not at all. Rather than smuggle rich content through cells (sentinel
chars, "pretend this rect is a PNG"), Ship of Tools runs two layers that share one wgpu
surface, one event loop, and one layout pass but render through distinct APIs:

**Layer 1 — ratatui chrome.** The mode-tree layout, status bar, modals, borders,
labels, focus state, keymaps, scroll indicators, the cursor. Painted by a custom
`Backend` impl that maps cells to glyphs in the wgpu canvas with a fixed-width
font (cosmic-text) — explicitly *not* crossterm.

**Layer 2 — the preview surface.** Takes a `Rect` from ratatui's layout and a
`PreviewPayload` from the kernel and draws directly into the wgpu surface inside
that rect, bypassing the cell stream. It dispatches on the payload's `mime`:

| MIME | Renderer |
|------|----------|
| `text/plain` | cosmic-text line layout, optional `syntect` highlighting |
| `text/markdown` | `comrak` AST → flowed text + inline images + math blocks |
| `image/png`, `image/jpeg`, `image/webp` | `image` decode → wgpu texture |
| `image/svg+xml` (incl. MathJax math) | `resvg` rasterize → wgpu texture |
| `application/pdf` | page rasterize → wgpu texture |
| `video/*` | frame stream → texture-per-frame |

The two layers compose by Z-order — chrome draws on top of previews where they
overlap, so a focus border lands after the preview content fills its rect. Input
goes to ratatui first; events that hit a region a preview widget registered for
(scrolling a markdown view) are re-dispatched to that preview's handler.
cosmic-text is the single source of truth for text shaping and measurement across
both layers, which is what keeps cell width and box-drawing alignment consistent.

A consequence worth stating: plugins emit `PreviewPayload(mime, bytes, extras)`
and never see ratatui or the wgpu surface. Adding a payload type is registering a
renderer (and possibly a new MIME) in the preview dispatch — for most additions,
no frontend changes beyond that.

## Inline math — the MathJax sidecar

Inline math is rendered **server-side to MathJax SVG** and shipped to the frontend
as `image/svg+xml`, which the preview layer rasterizes through `resvg`.
MathJax is the choice because it
emits SVG; KaTeX outputs HTML, which there is no path for here. The sidecar is a
small Node process (one per session) on the backend host — where Node is already
common — with vendored MathJax shipped in the package. The frontend has no Node
dependency. Full LaTeX *documents* (as opposed to inline math) render via
`tectonic` to PDF, page-rasterized; that dependency only loads when a plugin emits
document-level LaTeX.

## The stack

The libraries that implement both layers:

| Concern | Choice |
|---------|--------|
| Window / event loop | `winit` |
| GPU surface | `wgpu` (Vulkan / Metal / DX12 / GLES-via-ANGLE) |
| Text shaping / layout | `cosmic-text` |
| Vector / 2D drawing | `vello` (`tiny-skia` CPU fallback) |
| Raster decode | `image` |
| SVG raster | `resvg` |
| Inline math | MathJax SVG via a Node sidecar |
| Full LaTeX docs | `tectonic` → PDF → page raster |
| Markdown parse | `comrak` (CommonMark + GFM) |
| Syntax highlighting | `syntect` |
| Chrome model | `ratatui` (custom `Backend`) |

Constraints driving these: cross-platform single binary, GPU-accelerated text and
images, mature enough for a dev tool, room to grow into video and 3D plots without
rewriting the surface. The dependency graph is large but boring — nothing exotic,
all maintained. macOS and Linux work essentially out of the box on the wgpu side;
older Windows GPUs vary, with GLES-via-ANGLE as the safety net and `tiny-skia`
(CPU rasterization) as the correctness fallback before anything more drastic.

## What is deliberately rejected

- **Sixel, Kitty graphics, iTerm2, halfblocks — none, anywhere.** `ratatui-image`
  and `crossterm` are removed from the frontend; the old `img-probe` scratch
  binary is deleted.
- **No browser/webview** in the chosen path. A Tauri/wry web frontend is kept warm
  only as the option-B fallback — taken only if preview requirements broaden into
  arbitrary HTML / iframes / WebGL. Across that swap the kernel, backend, line
  protocol, and core IR are unchanged; only the frontend surface differs.
