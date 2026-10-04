# examples: sample inputs and the external plugin example (files, sidecars)

Sample files for previews and demos, and the one FileType plugin that lives outside the kernel's built-ins. Part of
files and sidecars; charters: rust/backend/src/files/CLAUDE.md, rust/backend/src/sidecars/CLAUDE.md.

## Files
- `plugins/`: external plugin examples. `plugins/HDF5Preview` is the external FileType example: the kernel depends on it through `[sources]` and loads it on the first `.h5`, `.hdf5` or `.hdf` preview (`LAZY_PLUGIN_FOR_EXT`); it reads metadata only, never dataset contents, and caps its output at `MAX_LINES`.
- `preview/`: samples for previews and docs captures (docs/SCREENSHOTS.md, docs/tools/docs-media.sh); sample.png, the figure of the markdown and scalebar examples; sample.mp4, the video-file test's sample; REPL demos in `run_file/`, `timesteps/` and `wglshow/`.

## Rules
- A renamed or deleted sample updates its readers in the same commit.
