Part of files: see ../CLAUDE.md.

A plugin's output past `PREVIEW_BYTE_CAP` (2 MiB) is cut at it when it is text and goes to the bytes-level reader when
it is any other mime (token JSON grows with its file), except a bounded-output plugin's (`is_bounded_output_plugin`:
PDF, video, HDF5), which ships whole: a PDF page rasterized for a zoomed pane or a video's poster is sized by the
plugin, and the bytes-level reader would answer with the file's own bytes. That ship is bounded at `MAX_BOUNDED_PICTURE_BYTES` (4096 x 4096 x 4 bytes, 64 MiB, the PDF
plugin's 4096 px cap on its bounded axis; its other axis follows the page's aspect and nothing else bounds the
kernel's reply), past which the bytes-level reader answers as before. A fallback is never silent on the daemon's side:
`plugin output exceeds cap on non-text mime; falling back to bytes-level reader` (WARN, with the size and mime) names it,
while the window logs nothing when it then shows "Not previewed as text". Behavior test:
`preview_gate_tests::a_bounded_plugins_picture_past_the_text_cap_ships_whole` (`fit_plugin_output`).

PNG pHYs supplies a physical scale only for metre-unit densities of at least 100,000 pixels per metre on both axes;
lower values are print density regardless of other chunks. A valid sidecar still takes priority, generated plugin
bytes are excluded, and anisotropic pHYs is suppressed. Behavior test: `scale::phys_scale_tests::
print_density_is_not_a_physical_scale_even_with_writer_metadata` in the existing backend unit suite.

`preview_gate_tests::microscope_header_reaches_preview_with_scale_through_a_file_and_a_symlink` runs
`build_preview_payload` (the `preview.get` assembly) on the captured 5120 × 5120 RGB header in
`fixtures-header.bin`, completed with CRC-correct IDAT and IEND chunks at the observed 11,903,190-byte file size.
It proves both axes reach `extras.physical_scale` as 4.7785 nm per pixel (within 1e-4), through the bytes reader,
sidecar tier and embedded-scale merge, for the file and, on Unix, a second row's symbolic link outside its root.
Windows runs the direct-file case without creating a link. The image is below the backend downsample trigger;
the window retains the served dimensions separately from a texture shrunk to its GPU's limit for the scale bar.
