# ADR 0034: dynamic scalebar for the preview pane

Status: **proposed** (2026-07-18). Design converged with the frontend (render
side) and SMLMAnalysis (scale source). Not yet implemented — this pins the
`extras` schema, the `preview.set_scale` op, the hybrid resolution order, the
two-bar render, and the live-entry→sidecar flow so the pieces can be built
against a fixed contract.

## Context

Raster image previews — SMLM/DNA-PAINT/MINFLUX renders, camera frames, survey
PNGs — carry **no axes**, so there is no at-a-glance sense of physical size. A
physical scalebar (e.g. `500 nm`) is standard in the field. We want an optional,
**dynamic** scalebar overlaid on raster previews: physically accurate at any
zoom, and readable even when the source image has no burned-in bar.

Two facts shape the design:

- The scale is **known and deterministic** where these images are produced —
  e.g. `SMLMRender.RenderInfo.pixel_size_nm` (`camera_px_nm / zoom`). It is just
  **not persisted with the saved PNG** today (no OME/`pHYs` in the SMLM stack).
- Scale can be **anisotropic**: XZ/axial views have `x_nm_per_px != z_nm_per_px`.
  (SMLMAnalysis's own renders are XY-isotropic only — `z` is color, not a spatial
  axis — so Keith's anisotropic case comes from other sources, but the contract
  must handle it.)

## Decision

### 1. Scale source: a kernel-side **hybrid resolver**

The scale nm/px is resolved **kernel-side**, so Rust never parses image metadata
(keeps the Rust↔Julia seam honest — same as every other `FileType` payload). For
a raster preview the kernel resolves `physical_scale` by trying, in order:

1. **Sidecar** — `<image>.scale.json` next to the file.
2. **Embedded metadata** — TIFF/OME resolution tags, or PNG `pHYs`
   (best-effort; `pHYs` is px/metre integer, lossy at nm scale — read but never
   *write* it, see §5).
3. **`.concept` annotation** — `physical_scale` in the node's `.concept/`
   frontmatter (user/orchestrator-authored).
4. **None** — no `physical_scale` extra; the FE offers live entry (§4).

The **sidecar is the primary and only *written* tier** (from the pipeline on
save, or from live entry). Embedded + `.concept` are read-only resolution tiers
for images the pipeline never touched (standard microscopy TIFFs, hand-annotated
datasets).

### 2. Schema — `extras.physical_scale`, an axes array

Carried in `PreviewGetRes.extras` (ADR 0021; already flows end-to-end — the FE
already reads `extras.page`/`page_count`, so this is **zero wire change**):

```json
"physical_scale": {
  "axes": [
    { "name": "x", "nm_per_px": 2.0 },
    { "name": "y", "nm_per_px": 2.0 }
  ],
  "unit": "nm"
}
```

- **Axes array** (not `x_nm_per_px`/`y_nm_per_px`) so axis *names* travel — an XZ
  view is `[{name:"x"}, {name:"z"}]`, and the FE labels each bar correctly.
- Isotropic renders emit two equal axes; the FE **collapses equal axes to one
  bar**. Anisotropic sources fill different values with **no schema change**.
- Image array layout is `(rows=y, cols=x)`, so `axes[0]` (x) is the **horizontal**
  image axis.
- The **same schema is used on the write side** (`preview.set_scale`, §4) so read
  and write can't drift.

### 3. FE render — adaptive, source-anchored, raster-only

- **Adaptive length:** snap to a nice `1/2/5 × 10ⁿ` value spanning ~10–20% of the
  visible pane, relabeled in `unit`.
- **Anchoring (the load-bearing detail):** the bar length keys off the
  **source→screen mapping** (`canvas_w / src_w`), **never** the raster buffer
  size. Zoom re-rasters the PNG at higher resolution (`preview_page_raster_zoom`)
  — the buffer dims change but the physical mapping does not, so keying off the
  buffer breaks after every zoom re-raster. (Same reason ROI is source-px,
  ADR 0022.) Pin the bar to the visible `preview_rect` so it survives pan and
  letterbox.
- **One bar vs two:** one horizontal bar when axes are equal; when anisotropic,
  an **L-shape in one corner** — a horizontal rect (x) and a vertical rect (the
  axial axis), each with an **axis-aligned label** near it. Text is **not
  rotated**: the shaper (glyphon + cosmic-text, `text.rs`) renders axis-aligned
  glyph runs only, so the vertical bar's label sits axis-aligned beside it, not
  along it.
- **Scope:** raster-only, gated by the existing `is_image_node_id` (plots/figures
  with their own axes skip naturally; no interaction with the downsample path
  since scale is per-source-px).
- **Style:** corner placement, theme tokens (light/dark aware).

### 4. Toggle + live-entry fallback

- **Toggle:** a preview keybind, **default off**, only *active* when
  `physical_scale` is present — mirrors the pagination `n`/`p` binds (only live
  when page extras exist).
- **Live entry when no scale is found:** toggling on with no `physical_scale`
  opens a one-line prompt (reuse **`NavPrompt`**, gpu.rs:1740 — the existing
  floating modal used by `Ctrl+N`, with FE-side positive-float validation; no new
  UI) to type nm/px (+ unit, + both axes if anisotropic). The entered value:
  1. renders the bar immediately, and
  2. **persists as the `<image>.scale.json` sidecar** so the hybrid resolver
     finds it next time — i.e. live entry *authors* the sidecar (the
     user-authored tier of §1).

### 5. `preview.set_scale` op (the live-entry write path)

New op — the FE captures the input but the **backend writes the sidecar** (the
file is on backend disk; the FE is remote — same shape as `image.crop` writing
`.sot/captures`):

```
preview.set_scale { node_id, physical_scale: {axes:[{name,nm_per_px}], unit} }
```

- Same `physical_scale` schema as the read-side extra (no drift).
- Backend writes `<image>.scale.json` **atomically** (temp file + rename) and
  **confined to a registered workspace root** (like every other backend write).
- On success the backend re-emits `preview.get` with the new `physical_scale` so
  the FE renders the bar without a round-trip guess.

## Phasing

- **Phase 1 (MVP):** sidecar read (kernel) + FE overlay + toggle + adaptive
  single bar. SMLMAnalysis adds its ~3-line `<image>.scale.json` emit
  (`axes:[{x,P},{y,P}]`) so its renders ship scalebar-ready. Proves the whole
  kernel→FE path end-to-end with no protocol change.
- **Phase 2:** live entry — `NavPrompt` input + `preview.set_scale` op + atomic
  backend sidecar write.
- **Phase 3:** the other resolver tiers (embedded TIFF/OME/`pHYs` read,
  `.concept` annotation) + the two-bar anisotropic render.

None of the later phases changes the Phase-1 contract (schema, op, anchoring are
fixed here).

## Consequences

- Complements — does not replace — SMLM's existing *burned-in* scalebar
  (`RenderConfig.scalebar`): the preview bar is dynamic (adapts to zoom) and works
  on images rendered *without* a burned bar, so renders can stop burning it in.
- One new op (`preview.set_scale`); the read path is a pure additive `extras` key
  (no protocol change).
- The FE overlay reuses existing machinery (quad overlay layer, the text shaper,
  `NavPrompt`), so FE cost is low.
- A wrong sidecar/annotation silently mislabels scale — the bar is only as
  trustworthy as its source; live-authored sidecars are explicit user intent.
