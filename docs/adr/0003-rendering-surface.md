# ADR 0003: Rendering surface (revised)

**Status:** Accepted — supersedes the original ADR 0003 ("Terminal image protocol")
**Date:** 2026-05-07 (revised same day after pivot)

## Context

The user's primary deployment is Windows local → SSH → tmux on Linux remote. Inline image protocols (sixel / Kitty / iTerm2) are not reliable end-to-end through that stack: tmux strips DCS by default, passthrough is fragile even with `allow-passthrough on`, and not every local terminal speaks the chosen protocol. The universally-supported fallback (halfblocks) is too low-fidelity for the concept-explorer use case — figures, math, and rendered docs at halfblocks resolution are unusable.

DevEnv exists specifically because no terminal-protocol path delivers acceptable rich previews on this stack. If they did, the user would be using yazi-in-tmux. Designing around terminal graphics protocols inverts the project's premise.

The original ADR 0003 ("ratatui-image autodetect with env override") implicitly accepted halfblocks as a fallback, which is the failure mode this project is meant to eliminate.

## Decision

**Render pixels ourselves in a native local window. No terminal graphics protocols at any layer.**

- The frontend is a **native Rust window** (winit) with a GPU-backed canvas (wgpu, with vello/tiny-skia/cosmic-text on top — see ADR 0012). It does not write escape sequences anywhere; it is not hosted in a terminal.
- The frontend uses **ratatui as the chrome/layout/input model only** — the layout machinery, widgets for borders/columns/labels, focus/keymap state. ratatui's `Backend::draw` writes cells into our own canvas via a custom `Backend` impl (not crossterm).
- A **parallel preview-layer surface** draws rich content (PNG, markdown, MathJax-SVG, video, plots) directly into rectangles ratatui's layout pass computed, bypassing the cell stream entirely. See ADR 0011.
- Backup direction if the preview-layer requirements broaden into "arbitrary HTML / iframes / WebGL / interactive plots" within the spike: switch to **option B** (Tauri/wry desktop app wrapping a web frontend). The kernel, backend, line protocol, and core IR are unchanged across that swap.

## Consequences

- **Sixel, Kitty graphics, iTerm2, halfblocks are not used anywhere.** `ratatui-image`, `crossterm` are removed from the frontend. The `img-probe` scratch binary is deleted.
- The frontend now ships per-platform binaries (Windows + Linux + macOS), not just the Linux-side workspace. The cross-compile/distribution story is real product work, not deferred polish.
- Image fidelity is bounded by what wgpu + the host GPU can display, not by what the user's terminal stack happens to forward. PNG roundtrips are now: kernel emits bytes → backend forwards over protocol → frontend decodes via `image` → uploads as wgpu texture → composites in the preview rect.
- Inline math is rendered server-side to **MathJax SVG** (Codex correction: KaTeX outputs HTML, not SVG) and shipped as `image/svg+xml`; the frontend rasterises via `resvg`. Full LaTeX documents render via `tectonic` to PDF, page-rasterised.
- Original ADR 0003's downscaling-in-kernel rationale survives: large images still get capped before transport. Cap raised to ~16 MB now that we control the renderer.
- A spike must validate this direction before further commitment — see plan.md M1.
