Part of files: see ../CLAUDE.md.

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
