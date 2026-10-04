# julia/plugins: the built-in FileType plugins (sidecars)

"Core ships as plugins to itself": each package here uses only core's public methods. The kernel loads all seven at
startup (its `using` lines, plus `[deps]` and `[sources]` in julia/kernel/Project.toml). Part of sidecars; charter:
rust/backend/src/sidecars/CLAUDE.md. The external example is examples/plugins/HDF5Preview.

## Files
- `json-doc/`: module ShipToolsJsonDoc; `.json` to application/json.
- `julia-source/`: module ShipToolsJuliaSource; `.jl` to application/vnd.sot.tokens+json.
- `markdown/`: module ShipToolsMarkdown; `.md`, `.markdown` to text/markdown.
- `pdf-file/`: module ShipToolsPDFFile; `.pdf` to image/png with `page` and `page_count` extras, via poppler.
- `plain-text/`: module ShipToolsPlainText; `.txt` to text/plain.
- `toml-doc/`: module ShipToolsTomlDoc; `.toml` to text/x-toml.
- `video-file/`: module ShipToolsVideoFile; `.mp4 .webm .mov .mkv .m4v` to an image/png poster, via ffmpeg.

## Start here
json-doc for the minimal shape; pdf-file for params and extras.

## Rules
- Extensions are lowercase and disjoint.
- A missing external tool gives a text/markdown note, never a throw (the `preview` methods of ShipToolsPDFFile and
  ShipToolsVideoFile).
- julia-source's spans concatenate to the file byte for byte.
- A new plugin brings kernel `[deps]`, `[sources]`, a `using` line, and a test CI.yml runs (today only pdf-file and
  video-file have tests).

Records: ADR 0018, ADR 0021, docs/src/extend/filetype.md.
