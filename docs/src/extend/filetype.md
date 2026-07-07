# Writing a FileType Plugin

```@meta
CurrentModule = ConceptExplorerCore
```

A `FileType` plugin teaches the explorer to recognise and preview a new kind of
file. It is the smallest, most common extension you can write — a package, one
abstract-type subtype, and two methods. No Rust changes, no registration call:
`using YourPackage` and the dispatch tables grow.

This page is the step-by-step build. For the conceptual contract see
[The Dispatch ABI](abi.md); for a complete external package see the
[HDF5 worked example](hdf5.md); for turning the plugin on in a project see
[Discovery & Configuration](discovery.md).

## What a FileType plugin does

Two things, both dispatched on your subtype:

| Method | Returns | Role |
|--------|---------|------|
| [`matches`](@ref)`(::Type{T}, path)` | `Bool` | claim a path (usually by extension) |
| [`preview`](@ref)`(::Type{T}, path)` | [`PreviewPayload`](@ref) | render it for the preview pane |

[`parse_entities`](@ref)`(::Type{T}, path)` is optional — it yields the
[`ConceptEntity`](@ref) values a file declares, for the concept layer. Phase-1
plugins return an empty vector; see [below](#Optional:-parse_entities).

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
Pkg.add("ConceptExplorerCore")
```

Your `Project.toml` should carry `ConceptExplorerCore` under `[deps]`. The
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
zero Rust changes.

### Choosing a MIME

Pick a MIME the frontend already renders, or emit `text/markdown` and let its
markdown renderer do the layout. MIMEs in use by the built-in plugins:

| MIME | Frontend rendering | Used by |
|------|--------------------|---------|
| `text/markdown` | comrak + cosmic-text (headings, code fences, inline math) | Markdown plugin; the HDF5 example wraps its metadata tree in fenced markdown |
| `text/plain` | plain UTF-8 text | error fallbacks |
| `application/json` | plain UTF-8 text today (pretty-print / syntax colour planned) | JSON plugin |
| `image/png` | native image quad | the PNG example in [the ABI page](abi.md) |
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
phase-1 built-in returns an empty vector — the method exists so the concept layer
can grow later without an ABI change:

```julia
ConceptExplorerCore.parse_entities(::Type{MyKind}, path::AbstractString) =
    ConceptExplorerCore.ConceptEntity[]
```

A later revision can yield one [`ConceptEntity`](@ref) per logical unit (the
Markdown plugin notes heading-hierarchy entities as the obvious next step), each
carrying an [`ast_hash`](@ref) for staleness provenance.

## Step 5 — Enable it

Loaded subtypes are discovered automatically, but *which* packages a project
loads is explicit. Add your package to the project's `Project.toml`:

```toml
[sot]
extensions = ["MyPreview"]
```

The kernel `Base.require`s each entry at startup, in order, before serving
requests — so after `install` + this edit + a kernel restart, opening a matching
file routes through your `preview`. See [Discovery & Configuration](discovery.md)
for the full discovery rule.

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
- [Discovery & Configuration](discovery.md) — enabling plugins, project-root rules.
- [The Dispatch ABI](abi.md) — the conceptual contract behind every method here.
- [API — ConceptExplorerCore](../ref/api-core.md) — symbol-by-symbol reference.
