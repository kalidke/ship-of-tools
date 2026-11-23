# API — Kernel

The Julia **kernel** (`ShipToolsKernel`) is the plugin host process. It owns the
dispatch tables, computes mode trees, indexes the project, hashes ASTs, and
produces Julia-aware previews — exposing all of it to the backend daemon over an
NDJSON line protocol.

!!! note "Reference under construction"
    Per-symbol API docs for `ShipToolsKernel` are not yet wired into this site (the
    kernel module is not currently a dependency of the docs build). For now:

    - The wire protocol it speaks: [Line Protocol](../design/protocol.md).
    - The ABI it dispatches on: [API — ConceptExplorerCore](api-core.md) and
      [The Dispatch ABI](../extend/abi.md).

    Source: `julia/kernel/src/ShipToolsKernel.jl`.

## [Built-in preview plugins](@id builtin-preview-plugins)

The standard file types ship under `julia/plugins/` and the kernel loads them at
startup. The MIME type tells the frontend which renderer to use; what each one
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

The PDF plugin also answers a page's words and where they are on the page, in place of the picture, when the
request's params hold `words` as `true`: MIME `application/vnd.sot.page-words+json`, a JSON document
`{"page": P, "cut": false, "lines": [[[x0, y0, x1, y1, "text"], ...], ...]}`. Each inner list is one word of one
line, in the reading order poppler's `pdftotext` gives; a box is in 65535ths of the drawn page's width and height
from its top left corner; `cut` is true when the page held more words than the 512 KiB the document may take. It
is for selecting text on a PDF page, which the window does not do yet: nothing sends the request.
