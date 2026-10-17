# API — Kernel

The Julia **kernel** (`ShipToolsKernel`) is the plugin host process. It owns the
dispatch tables, scans a project's modules and definitions statically, hashes
a file's bytes on `file.parse`, and produces Julia-aware previews — exposing all of it to the backend daemon over an
NDJSON line protocol.

!!! note "Reference under construction"
    Per-symbol API docs for `ShipToolsKernel` are not yet wired into this site (the
    kernel module is not currently a dependency of the docs build). For now:

    - The wire protocol it speaks: [Line Protocol](../design/protocol.md).
    - The ABI it dispatches on: [API — ConceptExplorerCore](api-core.md) and
      [The Dispatch ABI](../extend/abi.md).

    Source: `julia/kernel/src/ShipToolsKernel.jl`.

## [Built-in preview plugins](@id builtin-preview-plugins)

The seven standard file types ship under `julia/plugins/` and load at kernel start; HDF5Preview
(`examples/plugins/`) loads on the first `.h5`, `.hdf5` or `.hdf` preview. The MIME type tells the frontend which renderer to use; what each one
shows is in [Previews](../guide/previews.md).

| Extension | Plugin | MIME |
|-----------|--------|------|
| `.jl` | `ShipToolsJuliaSource` | `application/vnd.sot.tokens+json` |
| `.md`, `.markdown` | `ShipToolsMarkdown` | `text/markdown` |
| `.toml` | `ShipToolsTomlDoc` | `text/x-toml` |
| `.json` | `ShipToolsJsonDoc` | `application/json` |
| `.txt` | `ShipToolsPlainText` | `text/plain` |
| `.pdf` | `ShipToolsPDFFile` | `image/png` (+ `page`, `page_count` extras) |
| `.mp4`, `.webm`, `.mov`, `.mkv`, `.m4v` | `ShipToolsVideoFile` | `image/png` |
