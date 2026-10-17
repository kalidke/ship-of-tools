# Writing a FileType Plugin

!!! warning "Status: partly built"
    The `FileType` dispatch (`matches`, `preview`) is built. Nothing reads a `[sot]` table or loads a package for you:
    the kernel loads only the packages in its own project, so a new plugin is added to a source checkout of Ship of
    Tools (Step 5), not enabled from a user's project.

```@meta
CurrentModule = ConceptExplorerCore
```

A `FileType` plugin teaches the explorer to recognize and preview a new kind of
file. It is the smallest, most common extension you can write — a package, one
abstract-type subtype, and two methods. Once the kernel has loaded the package, its subtype joins the dispatch
tables (Step 5 below says how it gets loaded); a file kind whose preview output is bounded also
needs its extensions in one daemon function.

This page is the step-by-step build. For the conceptual contract see
[The Dispatch ABI](abi.md); for a complete external package see the
[HDF5 worked example](hdf5.md); for how plugins are discovered see
[Discovery & Configuration](discovery.md).

## What a FileType plugin does

Two things, both dispatched on your subtype:

| Method | Returns | Role |
|--------|---------|------|
| [`matches`](@ref)`(::Type{T}, path)` | `Bool` | claim a path (usually by extension) |
| [`preview`](@ref)`(::Type{T}, path)` | [`PreviewPayload`](@ref) | render it for the preview pane |

[`parse_entities`](@ref)`(::Type{T}, path)` is optional — it yields the
[`ConceptEntity`](@ref) values a file declares, for the concept layer. Nothing calls
it today (NOT BUILT), and the built-in plugins return an empty vector; see
[below](#Optional:-parse_entities).

At runtime the kernel finds every loaded subtype with [`file_types`](@ref) (a
`subtypes(FileType)` scan) and picks the first one whose `matches` returns `true`
with [`file_type_for`](@ref). Your job is to supply the subtype and its methods.

## Step 1 — Create the package

A plugin is an ordinary Julia package that depends on `ConceptExplorerCore`.
Generate one and add the dependency:

```julia
using Pkg
Pkg.generate("MyPreview")
Pkg.activate("MyPreview")
Pkg.develop(path="/path/to/ship-of-tools/core")
```

`ConceptExplorerCore` is unregistered, so use `Pkg.develop(path=...)` to point at
the local Ship of Tools `core/` checkout. Your `Project.toml` should carry
`ConceptExplorerCore` under `[deps]`. The
built-in Markdown plugin needs nothing else; only depend on what your `preview`
actually uses (the HDF5 plugin adds `HDF5`, the Julia-source plugin adds
`JuliaSyntax` and `JSON3`):

```toml
name = "MyPreview"
uuid = "..."                # filled in by Pkg.generate
version = "0.1.0"

[deps]
ConceptExplorerCore = "5a28ea34-b669-4214-86b0-825ecd8fbc7c"

[compat]
julia = "1.12"
```

## Step 2 — Declare the FileType subtype

In `src/MyPreview.jl`, `using ConceptExplorerCore` and define a singleton
subtype. The type carries no fields — it is purely a dispatch tag — so an empty
`struct ... end` is the whole declaration:

```julia
module MyPreview

using ConceptExplorerCore

export MyKind

struct MyKind <: ConceptExplorerCore.FileType end

end # module
```

That is enough for `file_types()` to see `MyKind` once the package is loaded.

## Step 3 — Claim paths with `matches`

Extend `ConceptExplorerCore.matches` for your subtype. Return `true` for paths
your plugin owns. Extension matching on a lowercased path is the standard idiom —
every built-in plugin does exactly this:

```julia
const MY_EXTENSIONS = (".myext", ".my")

ConceptExplorerCore.matches(::Type{MyKind}, path::AbstractString) =
    any(endswith(lowercase(path), ext) for ext in MY_EXTENSIONS)
```

`file_type_for` walks `file_types()` in order and returns the first subtype whose
`matches` is `true`. Keep your predicate narrow — claim only paths you can
actually render — so you don't shadow another plugin.

## Step 4 — Render with `preview`

Extend `ConceptExplorerCore.preview` to return a [`PreviewPayload`](@ref). A
payload is a MIME string plus the bytes the frontend should render; the
two-argument constructor `PreviewPayload(mime, data)` covers the common case (a
third `extras::Dict` argument is available for renderer hints):

```julia
function ConceptExplorerCore.preview(::Type{MyKind}, path::AbstractString)
    bytes = read(path)
    ConceptExplorerCore.PreviewPayload("text/markdown", bytes)
end
```

The MIME you choose decides which frontend renderer draws the preview — this is
the only contract the frontend cares about, and it is why a new `FileType` needs
no Rust change — unless its output must be bounded, in which case the daemon's
preview gates (`is_bounded_output_plugin` in the backend) also name its extensions.

The kernel always calls the three-argument form `preview(T, path, params)`, where `params` holds the request's
parameters (the PDF plugin reads `"page"`); a plugin that defines only the two-argument form gets the same call with
the parameters dropped. A paged plugin overrides the three-argument form and reports its position in `extras`
(`"page"`, `"page_count"`).

### Running an external tool

The kernel serves one request at a time, so a child process that never ends would freeze every preview for the row.
A plugin that runs an external program (the PDF and video plugins do) runs it through
`ConceptExplorerCore.read_within(cmd, seconds)`, which reads the program's output like `read(cmd)` and kills the
program when the bound passes. The built-in plugins use 20 seconds.

### Choosing a MIME

Pick a MIME the frontend already renders, or emit `text/markdown` and let its
markdown renderer do the layout. MIMEs in use by the built-in plugins:

| MIME | Frontend rendering | Used by |
|------|--------------------|---------|
| `text/markdown` | comrak + cosmic-text (headings, code fences, inline math) | Markdown plugin; the HDF5 example wraps its metadata tree in fenced markdown |
| `text/plain` | plain UTF-8 text | Plain-text plugin (`.txt`); error fallbacks |
| `text/x-toml` | plain UTF-8 text | TOML plugin |
| `application/json` | plain UTF-8 text today (pretty-print / syntax color planned) | JSON plugin |
| `image/png` | native image quad | PDF plugin (one page) and video plugin (a poster frame) |
| `application/vnd.sot.tokens+json` | pre-tokenised span renderer (`{spans:[{text,kind},…]}`) | Julia-source plugin |

If none fits, the pragmatic choice is to format your content as markdown
(`text/markdown`) — that is what the HDF5 plugin does, emitting a header plus a
fenced `text` tree. Returning `text/plain` is the right move for an error path
(open failed, parse failed): surface the failure as visible text rather than
faking a successful preview.

### Failing visibly

Do not let a bad file produce a fake-success preview. The HDF5 plugin catches an
open or read failure and returns a `text/plain` payload describing the error, so
the gap is on screen instead of a silent blank. Match that: render the error,
don't swallow it.

## Optional: `parse_entities`

If your file kind declares conceptual units the concept layer should track
(functions, types, sections, datasets), implement `parse_entities`. Every
built-in plugin returns an empty vector — the method exists so the concept layer
can grow later without an ABI change:

```julia
ConceptExplorerCore.parse_entities(::Type{MyKind}, path::AbstractString) =
    ConceptExplorerCore.ConceptEntity[]
```

Nothing calls `parse_entities` today: the concept layer does not read it, so
whatever a plugin returns is unused (NOT BUILT). A later revision could yield one
[`ConceptEntity`](@ref) per logical unit (the Markdown plugin notes
heading-hierarchy entities as the obvious next step), each carrying an
[`ast_hash`](@ref) for staleness provenance; `ast_hash` has no method or caller
today either.

## Step 5 — Enable it

Loaded subtypes are discovered automatically, but nothing loads your package for you. The kernel runs only its own
project, `julia/kernel`, and never activates the user's environment; no code reads an `[sot]` table from a
`Project.toml` (see [Discovery & Configuration](discovery.md)). A plugin therefore loads only from a source checkout
of Ship of Tools:

1. Make the package a dependency of the kernel: a `[deps]` entry and a `[sources]` path in
   `julia/kernel/Project.toml`, as the built-in plugins and `HDF5Preview` have.
2. Load it. Add a `using MyPreview` line to `julia/kernel/src/ShipToolsKernel.jl`, which loads it at kernel start. For
   a package with heavy dependencies, add a row to `LAZY_PLUGIN_FOR_EXT` in `julia/kernel/src/preview.jl` instead,
   mapping each lowercase extension (`".myext"`) to the package name; the kernel runs `using` on it the first time a
   file with that extension is previewed.
3. Resolve the kernel's environment:
   `julia --project=julia/kernel -e 'using Pkg; Pkg.resolve(); Pkg.instantiate()'`. Without it the kernel cannot load
   the package, and every Julia-aware preview fails, not only yours. The kernel loads its plugins once, when it starts,
   and has no narrower restart than the daemon's: restart `sotd` (a kernel that died on the missing package is
   respawned by the daemon on a backoff of 250 ms up to 30 s).
4. If the preview's size does not grow with the file's (a metadata tree, a poster frame; the HDF5, PDF and video
   plugins are in this group), add its extensions to
   `is_bounded_output_plugin` in `rust/backend/src/files/preview/mod.rs`. Without that, the daemon never calls the
   plugin for a file over its 2 MiB input cap and falls back to the raw bytes. This is a Rust change.

On a release install the kernel lives in the pinned release checkout, and updates refuse to move a checkout with local
changes (see [Updating and rollback](../guide/updating.md)).

## Full skeleton

```julia
module MyPreview

using ConceptExplorerCore

export MyKind

struct MyKind <: ConceptExplorerCore.FileType end

const MY_EXTENSIONS = (".myext", ".my")

ConceptExplorerCore.matches(::Type{MyKind}, path::AbstractString) =
    any(endswith(lowercase(path), ext) for ext in MY_EXTENSIONS)

function ConceptExplorerCore.preview(::Type{MyKind}, path::AbstractString)
    bytes = read(path)
    ConceptExplorerCore.PreviewPayload("text/markdown", bytes)
end

ConceptExplorerCore.parse_entities(::Type{MyKind}, path::AbstractString) =
    ConceptExplorerCore.ConceptEntity[]

end # module
```

## Next steps

- [Worked Example: HDF5](hdf5.md) — a real third-party plugin, end to end.
- [Discovery & Configuration](discovery.md) — how plugins are found and loaded today.
- [The Dispatch ABI](abi.md) — the conceptual contract behind every method here.
- [API — ConceptExplorerCore](../ref/api-core.md) — symbol-by-symbol reference.
