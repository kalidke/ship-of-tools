# rust/backend/src/files/preview: a file's preview, scale and crop (files)

What a client sees when it selects a file node: `preview.get` builds the payload from a kernel plugin or, failing that,
the file's own bytes; `preview.set_scale` stores a user-entered physical scale (ADR 0034) and re-serves the preview;
`image.crop` cuts a region of an image into the row's captures folder (ADR 0022). Part of files; charter:
rust/backend/src/files/CLAUDE.md.

## Files
- `mod.rs`: `preview.get`: the payload builder, the byte caps, the plugin gate and the raster downsample.
- `scale.rs`: `preview.set_scale`, the `<image>.scale.json` sidecar and a PNG's `pHYs` density.
- `crop.rs`: `image.crop`: a region of an image written as a PNG under `.sot/captures/`.

## Start here
`mod.rs` `build_preview_payload` for what a preview holds; `scale.rs` `handle_preview_set_scale` for scale writes.

## Rules
- Text over `PREVIEW_BYTE_CAP` (2 MiB) is truncated; binary over `PREVIEW_BINARY_CAP` (512 MiB) is refused with a note,
  never cut (`read_bytes_preview`).
- A raster over `PREVIEW_DOWNSAMPLE_TRIGGER` ships downsampled to `PREVIEW_DOWNSAMPLE_MAX_DIM` with its scale rescaled,
  or raw when the decode fails (`downsample_oversize_raster`, `rescale_physical_scale`).
- Input over the byte cap skips the kernel plugin unless its output is bounded (`is_bounded_output_plugin`: video,
  HDF5, PDF).
- The sidecar is written through a unique `O_EXCL` temp and a rename, verbatim (`write_scale_sidecar_atomic`).
- A PNG `pHYs` is trusted only with a valid CRC and is never written (`png_phys_nm_per_px`).
