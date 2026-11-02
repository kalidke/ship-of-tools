Part of files: see ../CLAUDE.md.

PNG pHYs supplies a physical scale only for metre-unit densities of at least 100,000 pixels per metre on both axes;
lower values are print density regardless of other chunks. A valid sidecar still takes priority, generated plugin
bytes are excluded, and anisotropic pHYs is suppressed. Behavior test: `scale::phys_scale_tests::
print_density_is_not_a_physical_scale_even_with_writer_metadata` in the existing backend unit suite.
