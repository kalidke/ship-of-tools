# ADR 0012: Frontend rendering stack

**Status:** Accepted (provisional — re-validate during the M1 spike)
**Date:** 2026-05-07

## Context

ADR 0003 (revised) commits to a native local window owning the renderer. ADR 0011 splits rendering into a ratatui chrome layer and a parallel preview-layer surface. This ADR pins the specific Rust libraries that implement both layers.

Constraints: cross-platform (Windows + Linux, macOS later), single binary, GPU-accelerated text and image, sustainable licenses, mature enough for a dev-tools workload, room to grow into video and 3D plots without rewriting the surface.

## Decision

| Concern | Choice | Rationale |
|---|---|---|
| Window / event loop | `winit` | De-facto cross-platform standard; works with wgpu and the rest of the Rust GUI ecosystem. |
| GPU surface | `wgpu` | Cross-platform (Vulkan / Metal / DX12 / GLES via ANGLE). Integrates with everything below. |
| Text shaping/layout | `cosmic-text` | Best-in-class Rust text shaper; handles unicode, BiDi, emoji, fixed-width metrics. Used by Cosmic, Zed-class apps. |
| Vector / 2D drawing | `vello` | Modern compute-shader 2D engine on wgpu; good fit for SVG-style content and crisp HiDPI. `tiny-skia` (CPU) is the fallback if vello requires too-new GPU features on the user's hardware. |
| Raster decode | `image` | Boring, ubiquitous. |
| SVG raster | `resvg` | Mature, no JS, handles MathJax-shape SVG well. Documented limitations are tolerable for our use. |
| Inline math (server-side) | **MathJax** SVG output via a Node sidecar (per session) | Codex correction: KaTeX outputs HTML, not SVG; MathJax does SVG. We render math in the kernel/backend, ship `image/svg+xml` to the frontend, rasterise via `resvg`. |
| Full LaTeX docs | `tectonic` (Rust LaTeX engine, no system TeX needed) → PDF → page raster | Used only when a plugin emits LaTeX as a document, not inline. |
| Markdown parse | `comrak` (CommonMark + GFM extensions) | We render the AST ourselves through cosmic-text; no HTML rendering path. |
| Syntax highlighting | `syntect` | Sublime-compatible grammars; well-trodden. |
| Chrome model | `ratatui` (custom `Backend`) | Per ADR 0011. |
| Input | `winit` events → ratatui keymap dispatcher; preview-layer handlers re-dispatched as in ADR 0011. | — |

**Out:** `ratatui-image`, `crossterm`, `terminal-image-protocol-of-any-kind`, browser/webview (kept as the option-B fallback only).

## Consequences

- Frontend dependency graph is large but boring; nothing exotic, all maintained.
- macOS and Linux work essentially out of the box on the wgpu side; Windows GPU support varies on older hardware — wgpu's GLES-via-ANGLE backend is the safety net.
- Distribution: per-platform binary, ~30–60 MB. Acceptable for a dev tool.
- The MathJax sidecar adds a Node dependency to the *backend host* (remote machine, where Node is already common). Vendoring MathJax + a small JS shim ships with the package. The frontend has no Node dep.
- `tectonic` adds a non-trivial dep to the backend, but only loads when full-LaTeX previews are requested. Defer to M5+ — inline math via MathJax covers M1 spike requirements.
- If the M1 spike surfaces a wgpu/vello incompatibility on the user's Windows GPU, fall back to `tiny-skia` (CPU rasterisation) before considering option B. CPU 2D is enough for chrome + previews; GPU is a performance optimisation, not a correctness requirement.
- Re-validate this ADR after the spike. If two of {wgpu, vello, cosmic-text, resvg} surprise us, revisit.
