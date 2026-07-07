# ADR 0022: Capture an image-preview ROI to the LLM pane

**Status:** Accepted
**Date:** 2026-06-12

## Context

The user inspects scientific images in the preview pane (PNG/figures), zooms into
a detail, and wants the orchestrator LLM (the in-pane `claude`) to *look at that
specific zoomed region* — "what's the artifact here?". Today the LLM pane has no
idea what's in the preview, and there's no way to hand it a crop.

Two framings surfaced: (a) a one-off "capture this ROI and send it" action, and
(b) the higher-level "make the LLM pane sot-aware so it knows what I'm looking
at". They converge: (b) is the foundation, (a) is one trigger on top of it.

This extends ADR 0019 (frontend control channel): `fe-state.json` already lets the
in-pane agent *observe* the FE (mode/focus/workspace), and `fe-commands/` lets it
*drive* the FE. We add the preview to what it can observe, and a capture to what it
can drive.

## Decision

### Crop from the source, on the backend

A new op **`image.crop {node_id, x, y, w, h}`** (coords in source-image px) resolves
the node to a path (`node_id_to_path`), decodes the **source file** with the `image`
crate, clamps the ROI to the image bounds, crops, and writes a PNG under
`<workspace_root>/.devenv/captures/<stem>-roi-<micros>.png` on the backend host. It
returns the path + the clamped rect + source dims.

Cropping from the source (not a framebuffer grab) is the key call: at a deep zoom a
screen grab is upsampled and blurry, but a source crop is pixel-perfect. And because
the in-pane `claude` runs *on the backend*, it `Read`s the returned path directly —
no pixel transfer over the wire.

PDFs are out of scope for v1: a PDF preview is a rasterized page, not the `.pdf`
file `image.crop` would try to decode. The op rejects non-images; the FE gates the
trigger to image node ids. Cropping a rasterized page is a v2.

### FE: compute the ROI, both triggers, awareness

- **ROI computation.** Each draw, when an image is shown, the FE maps the visible
  `canvas ∩ pane` rectangle (from zoom/pan/letterbox) to a source-pixel ROI via
  the pure, unit-tested `visible_roi_px` (canvas-relative fractions × native dims,
  so it's independent of any decode-time downsample). Stashed on `State.preview_roi`.
- **`fe-state.json` `preview` block** — `{node_id, path, dims, zoom, roi}`, the
  awareness foundation. Its signature buckets zoom (0.1) + ROI (32 px) so a
  continuous gesture rewrites at a coarse cadence, not every frame.
- **Two triggers, one method (`capture_roi`):**
  - **Push:** the **`C`** key in Preview focus.
  - **Pull:** the in-pane agent drops `{"cmd":"capture_roi"}` into `fe-commands/`
    (ADR 0019) — "look at what I'm zoomed into" without a hotkey.
- **Delivery.** `image.crop` reply → the FE bracketed-pastes
  `"Look at this cropped region of <name> — ROI … Read the image at: <path>"` into
  the LLM pane (the BL pty). **No trailing Enter** — the user can add context and
  submit, so a shared pane's partial input is never clobbered and no half-formed
  prompt fires.

## Consequences

- **Positive:** the LLM pane becomes preview-aware with zero new transport on the
  observe side (rides `fe-state.json`) and one small op on the crop side. Generalizes
  past images — the `preview` block is a hook for future "what am I looking at" asks.
- **Positive:** full-fidelity crops; no large pixel blobs on the wire (path only).
- **Negative / accepted:** PDF-page ROI deferred to v2. Capture targets the current
  BL pty as "the LLM pane"; on an agent workspace that's the agent, not the
  orchestrator — acceptable for v1.
- **Negative / accepted:** captures accumulate under `.devenv/captures/`
  (gitignored); no auto-cleanup yet. Revisit if it grows.
- The backend half ships first and deploys on the backend host; the FE half is
  verified locally (`visible_roi_px` unit tests) and end-to-end once the backend is
  live.
