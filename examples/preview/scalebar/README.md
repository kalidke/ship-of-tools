# Dynamic scalebar examples (ADR 0034)

The preview pane can overlay a **physically-accurate scalebar** on raster image
previews (SMLM/DNA-PAINT/MINFLUX renders, camera frames) that carry no axes. The
bar is dynamic: it stays correct at any zoom and re-labels to a nice
`1 / 2 / 5 × 10ⁿ` value spanning ~10–20 % of the pane.

Toggle it with **`b`** while previewing a raster (the bind is only active when the
image has a known scale). A `.scale.json` sidecar is the scale source.

## The sidecar

The scale lives in `<image>.scale.json` **next to the image** (e.g. `render.png`
→ `render.png.scale.json`). Its contents are the `physical_scale` object the
backend attaches to the preview:

```json
{ "axes": [ { "name": "x", "nm_per_px": 2.0 }, { "name": "y", "nm_per_px": 2.0 } ], "unit": "nm" }
```

- `axes[0]` is the **horizontal** image axis (image layout is `rows = y`,
  `cols = x`).
- Isotropic renders emit two **equal** axes; the FE collapses them to one bar.
- Anisotropic sources (e.g. an XZ view) emit **different** values per axis.

The sidecar is the primary (and only *written*) tier; embedded metadata
(TIFF/OME, PNG `pHYs`) and `.concept` annotations are read-only resolver tiers
that come later.

## The two examples

| Example | Image | Scale | Renders |
|---------|-------|-------|---------|
| **Isotropic** | [`../sample.png`](../sample.png) (512²) | `x = y = 2 nm/px` → 1024 nm FOV | one horizontal bar (~`200 nm`) |
| **Anisotropic (XZ)** | [`xz_view.png`](xz_view.png) (512²) | `x = 5 nm/px`, `z = 20 nm/px` | see note ↓ |

**Anisotropic note:** the `xz_view.png` sidecar is a genuine anisotropic case — an
axial (XZ) slice where the lateral pixel size (5 nm) differs from the axial one
(20 nm), as in astigmatism-based 3D SMLM. **Phase 1 renders a single bar from
`axes[0]` (the lateral / x bar, ~`500 nm`).** The **second (axial / z) bar** — an
L-shape in the corner with its own axis-aligned label — is **Phase 3** (ADR 0034
§Phasing); this example is here so that path has real data the day it lands.

## Seeing it live

Requires a scalebar-capable backend: the daemon must include
`handlers::merge_scale_sidecar` (post-2026-07-18). A daemon predating that
serves the image but never sends `physical_scale`, so `b` does nothing. Rebuild
+ restart `sotd`, preview one of these images, and press `b`.
