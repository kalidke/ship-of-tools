---
target: files/rust/frontend/src/gpu.rs
target_kind: file
synced_against: spike-placeholder
synced_at: 2026-05-11T15:00Z
authored_by: orchestrator
---

# gpu.rs concept annotation

This file owns the winit + wgpu surface lifecycle for the Ship of Tools
frontend. It's the entry point for the rendering pipeline: chrome text
(via ratatui→cosmic-text), preview-layer quads (PNG/SVG/markdown), and
the keyboard input plumbing all funnel through here.

Notable invariants:
- The wgpu surface is reconfigured on `Resized`; the text layer's
  glyph atlas resizes accordingly.
- `--capture <path>` mode keeps requesting redraws until
  `frame_counter == CAPTURE_FRAME` so the transport task has time to
  push hello/tree/preview events before the swapchain readback fires.
