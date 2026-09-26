# Previews

When you cursor a file in any mode, the preview pane renders it at the fidelity
appropriate to its type. The backend first asks the Julia kernel: a
[`FileType`](@ref) plugin claims the path with [`matches`](@ref) and returns a
[`PreviewPayload`](@ref) from [`preview`](@ref). When no plugin claims it, the
backend reads the file itself; that is how raster images and plain text are
served. The payload's `mime` tells the
frontend which renderer to use; the bytes are opaque to Rust. Adding a format is a
Julia-only change — see [The Dispatch ABI](../extend/abi.md) and
[Writing a FileType Plugin](../extend/filetype.md).

```@raw html
<DemoLoop name="pdf" caption="With the preview focused, n and p page through a PDF, then = zooms in to read it." />
```

```@raw html
<DemoShot name="preview-hdf5" caption="An HDF5 file: groups, datasets, shapes, element types and attributes." />
```

## Built-in file types

These plugins ship under `julia/plugins/` and the kernel loads them at startup.
Each is a built-in that travels the exact same dispatch path a third-party plugin
would.

| Extension | Representation | Fidelity |
|-----------|----------------|----------|
| `.jl` | token spans (kind + text) from `JuliaSyntax.jl` | syntax-highlighted source, no client-side re-tokenizing |
| `.md`, `.markdown` | raw UTF-8 bytes | rendered markdown (comrak + cosmic-text), including inline math |
| `.toml` | raw UTF-8 bytes | code/text renderer |
| `.json` | raw UTF-8 bytes | plain text today (pretty-print / colour planned) |
| `.txt` | raw UTF-8 bytes | plain text renderer |
| `.pdf` | one poppler-rasterized page | paged; `n`/`p` turn pages with the preview focused, rasterized backend-side to fit the pane |
| `.mp4`, `.webm`, `.mov`, `.mkv`, `.m4v` | a single ffmpeg poster frame | still poster in the pane; `o` opens playback in the browser |

The plugin module and MIME type behind each row are listed under
[Built-in preview plugins](@ref builtin-preview-plugins).

PNG (and other raster images) render directly through the frontend's image quad
path — the same path the PDF and video plugins reuse by returning `image/png`.

A few notes on the table:

- **Julia source** is tokenized on the kernel side into `{spans: [{text, kind}]}`,
  so the frontend colours it without parsing Julia. The token kinds map to the
  frontend's colour set (`keyword`, `comment`, `string`, `number`, `op`, `punct`,
  `type`, `ident`, `text`); concatenating every span reproduces the file
  byte-for-byte.
- **PDF** is the one format whose preview takes a page parameter — the only
  built-in that addresses *which part* of a file to render. Pages rasterize on the
  host where the file lives (poppler's `pdftoppm` / `pdfinfo`), matching the
  backend-side-decode model.
- **Video** deliberately does *not* play in the pane. A native player (HTML5
  `<video>` with hardware decode) beats streaming decoded frames over a socket, so
  the pane shows a poster and `o` pops the real file out to the browser.

When an external tool is missing (ffmpeg for video, poppler for PDF), the plugin
returns a `text/markdown` note explaining the gap — never a silent blank pane.

## Opens in the browser, not the pane

Some formats are interactive HTML and belong in a real browser rather than a
static pane. Consistent with the video policy, these are popped out to the OS
browser through the backend connection, local or remote, rather than rendered
in-pane:

- **HTML** (`sample.html`) — opened in the browser.
- **Pluto notebooks** (`pluto_demo.jl`, `pluto_demo2.jl`) — opened as live Pluto
  sessions in the browser. As `.jl` files they still get a syntax-highlighted
  source preview in the pane.
- **Quarto documents** (`quarto_julia.qmd`) — the rich rendered form opens in the
  browser; the source previews as text in-pane.

These all follow the same "rich/interactive content lives in the browser" policy.

## Pan and zoom images

With the preview focused (`Ctrl+Right`), `=` zooms an image in and the arrow
keys pan it. Same-size images in a directory share zoom and pan, so stepping through a run's plots keeps your
framing.

```@raw html
<DemoLoop name="zoom" caption="With the preview focused, = zooms an image and the arrow keys pan; the next same-size image opens at the same zoom and pan." />
```

## Capturing a preview region for the LLM

For raster image previews you can zoom in, then, with the preview focused
(`Ctrl+Right`), press `c` to crop the visible
region and hand it to the session's agent in the agent pane, with a question
such as "what's the artifact here?". The crop is taken from
the source image on the backend and delivered to the agent, so it
sees exactly the region you are looking at.

## Formats added by external plugins

File types do not have to be built-in. The [HDF5 worked example](../extend/hdf5.md)
ships as a separate package and claims `.h5` (see the `sample.h5` fixture) with no
Rust changes. It is a dependency of the kernel's environment and is registered in
the kernel's lazy-load table (`LAZY_PLUGIN_FOR_EXT`), which loads it on the first
`.h5` preview.

## Adding a new file type

A `FileType` plugin is small: a subtype, a [`matches`](@ref) predicate, and a
[`preview`](@ref) that returns a [`PreviewPayload`](@ref) with the right MIME.

```julia
module CsvPreview

using ConceptExplorerCore

struct CsvFile <: FileType end

ConceptExplorerCore.matches(::Type{CsvFile}, path) =
    endswith(lowercase(path), ".csv")

function ConceptExplorerCore.preview(::Type{CsvFile}, path)
    lines = readlines(path)
    ncols = length(split(lines[1], ","))
    table = String[]
    for (i, line) in enumerate(lines)
        push!(table, "| " * replace(line, "," => " | ") * " |")
        i == 1 && push!(table, "|" * repeat(" --- |", ncols))  # header rule
    end
    PreviewPayload("text/markdown", Vector{UInt8}(join(table, "\n")))
end

end
```

The plugin runs in the kernel, not in your REPL: add the package to the
`julia/kernel` environment (`Pkg.develop` or `Pkg.add`), then add it to the
kernel's `using` list or map its extension in `LAZY_PLUGIN_FOR_EXT`. Today that
works only from a source checkout. No Rust change is needed. See
[Discovery](../extend/discovery.md) for what loads today. For the full contract and discovery rules see
[The Dispatch ABI](../extend/abi.md) and [Writing a FileType Plugin](../extend/filetype.md).
