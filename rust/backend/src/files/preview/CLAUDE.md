Part of files: see ../CLAUDE.md.

A plugin's output past `PREVIEW_BYTE_CAP` (2 MiB) is cut at it when its mime is text, and goes to the bytes-level reader
when it is any other mime (token JSON grows with its file). The exception is a picture (an `image/` mime) from a
bounded-output plugin (`is_bounded_output_plugin`: PDF, video, HDF5): its size is the plugin's (a PDF page rasterized for
a zoomed pane, a video's poster), not its input's, and the reader would answer with the file's own bytes, so it ships
whole up to `MAX_BOUNDED_PICTURE_BYTES` (4096 x 4096 x 4 bytes, 64 MiB, a chosen bound on what one kernel line may make
the daemon hold). A bounded plugin's picture past it is refused, never sent to the reader: `try_plugin_preview` answers
`Err("preview_too_large", ...)` and the window keeps its pane and says so on the status line. A bounded plugin's other
non-text output goes to the reader as any other does, and a fallback is named by a WARN with the size and mime.
Behavior tests: `preview_gate_tests::a_bounded_plugins_picture_past_the_text_cap_ships_whole`
(`fit_plugin_output`) and the two through `try_plugin_preview` over a stand-in kernel beside it.

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
