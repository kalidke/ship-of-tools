# Worked Example: HDF5

```@meta
CurrentModule = ConceptExplorerCore
```

`HDF5Preview` is a complete, real `FileType` plugin shipped under
`examples/plugins/HDF5Preview/`. It adds metadata-only previews for `.h5` /
`.hdf5` / `.hdf` files. Its purpose is to **validate the dispatch ABI from
outside core** — it is a separate package, depends only on
`ConceptExplorerCore` and `HDF5`, and requires **zero Rust changes** to light up
HDF5 previews in the frontend.

Read it alongside the generic build in [Writing a FileType Plugin](filetype.md);
this page walks the actual code top to bottom.

## The package

`examples/plugins/HDF5Preview/Project.toml`:

```toml
name = "HDF5Preview"
uuid = "c0d7e0f3-5a1c-4b8e-9f23-2e5a6c4b1f88"
authors = ["kalidke"]
version = "0.1.0"

[deps]
ConceptExplorerCore = "5a28ea34-b669-4214-86b0-825ecd8fbc7c"
HDF5 = "f67ccb44-e63f-5c2f-98bd-6dc0ccc4ba2f"

[compat]
HDF5 = "0.17"
julia = "1.12"

[sources]
ConceptExplorerCore = { path = "../../../core" }
```

The `[sources]` entry is what lets this env instantiate **standalone**:
`ConceptExplorerCore` is unregistered, so without a path source `Pkg` fails in
`check_registered`. A plugin living in its own repository would point `path`
(or `url`) at wherever it keeps core.

Two dependencies, no more: `ConceptExplorerCore` for the ABI, `HDF5` for reading
the files. `HDF5` pulls in `HDF5_jll`, a self-contained, cross-platform binary —
deliberately chosen over shelling out to `h5ls` / `h5dump`, which are not present
on every backend host and would be a fresh-box-breaks failure mode.

## The module and the subtype

`examples/plugins/HDF5Preview/src/HDF5Preview.jl` opens with the two `using`s and
a singleton [`FileType`](@ref) subtype:

```julia
module HDF5Preview

using ConceptExplorerCore
using HDF5

export HDF5File

struct HDF5File <: ConceptExplorerCore.FileType end
```

`HDF5File` carries no fields — it is a dispatch tag. Once the plugin loads,
[`file_types`](@ref) (the `subtypes(FileType)` scan) sees it with no registration
call: the dispatch wiring *is* the ABI.

## `matches` — claiming the path

```julia
const HDF5_EXTENSIONS = (".h5", ".hdf5", ".hdf")

ConceptExplorerCore.matches(::Type{HDF5File}, path::AbstractString) =
    any(endswith(lowercase(path), ext) for ext in HDF5_EXTENSIONS)
```

The same lowercased-extension idiom every built-in uses. [`file_type_for`](@ref)
returns `HDF5File` for the first matching path it walks.

## `preview` — a metadata-only tree

This is the substance of the plugin. The design is **metadata-only**: it walks
groups, datasets, and attributes and reports name, shape, eltype, chunking, and
small attribute values — but **never reads dataset contents**, so the preview is
instant regardless of file size.

The representation it produces is **`text/markdown`**: a header line plus a fenced
`text` block containing an ASCII tree.

```julia
function ConceptExplorerCore.preview(::Type{HDF5File}, path::AbstractString)
    sz = try
        filesize(path)
    catch
        -1
    end
    file = nothing
    try
        file = HDF5.h5open(path, "r")
    catch e
        msg = "HDF5 open failed for $(basename(path)):\n\n$(sprint(showerror, e))"
        return ConceptExplorerCore.PreviewPayload("text/plain", Vector{UInt8}(msg))
    end
    try
        w = Walk()
        emit!(w, basename(path) * "/")
        walk_group!(w, file, "")
        szstr = sz < 0 ? "?" : Base.format_bytes(sz)
        header = "# HDF5 · $(basename(path))\n\n" *
                 "`$(path)` · $(szstr) · $(w.groups) groups · $(w.datasets) datasets\n\n"
        footer = w.omitted > 0 ?
            "\n… +$(w.omitted) more objects (capped at $(MAX_LINES) lines)\n" : ""
        body = header * "```text\n" * join(w.lines, "\n") * "\n" * footer * "```\n"
        return ConceptExplorerCore.PreviewPayload("text/markdown", Vector{UInt8}(body))
    catch e
        msg = "HDF5 read failed for $(basename(path)):\n\n$(sprint(showerror, e))"
        return ConceptExplorerCore.PreviewPayload("text/plain", Vector{UInt8}(msg))
    finally
        file !== nothing && close(file)
    end
end
```

What the rendered payload contains:

- A markdown **header**: `# HDF5 · <basename>` then a line with the full path,
  human-readable size (`Base.format_bytes`), and the group/dataset counts.
- A fenced **`text` tree** of the file's structure, joined from the lines the
  walk accumulated.
- A footer line **only if** output was capped (see below).

The MIME is `text/markdown`, so the frontend's existing markdown renderer draws
it — no HDF5-specific renderer, no Rust. The plugin reuses a renderer that
already exists by formatting its content as markdown, which is the recommended
fallback in [Writing a FileType Plugin](filetype.md).

### Inside the walk

The tree is built by a depth-first walk that the plugin keeps bounded and safe:

- A `Walk` accumulator holds the output `lines`, running `groups` / `datasets`
  counts, and an `omitted` count.
- `dataset_label` formats one dataset line as `name {shape} eltype chunk` — e.g.
  `name {100×200} Float64 chunk {32×32}` — all from metadata, no data read.
- `attr_lines!` renders attributes marked `@`, showing values only for small
  scalars/strings (truncated at `MAX_ATTR_VALUE_CHARS = 60`), never reading large
  attribute arrays.
- `walk_group!` recurses with `├─` / `└─` connectors, closing each child it
  opens.

### Output guardrails and visible failure

Two project rules show up directly in the code:

- **Bounded output.** Output is capped at `MAX_LINES = 500`. Past the cap, lines
  are counted as `omitted` and surfaced in an explicit `… +N more objects` footer
  — it reports what was dropped rather than silently truncating.
- **No quiet fallback.** An open failure or a read failure returns a `text/plain`
  payload describing the error (via `sprint(showerror, e)`) — the gap is rendered
  on screen, never a fake-success blank or a panic.

## `parse_entities` — empty for phase 1

```julia
ConceptExplorerCore.parse_entities(::Type{HDF5File}, path::AbstractString) =
    ConceptExplorerCore.ConceptEntity[]
```

No concept entities yet. The docstring notes the future option: one
[`ConceptEntity`](@ref) per group/dataset, for annotation in the concept nav.

## Enabling it

The plugin is loaded the same explicit way as any extension — list it in the
project's `Project.toml` `[sot]` table:

```toml
[sot]
extensions = ["HDF5Preview"]
```

The kernel `Base.require`s each entry at startup, in declared order, before
serving requests (see [Discovery & Configuration](discovery.md)). For HDF5 specifically the kernel
also supports lazy loading — it does not eagerly `using HDF5Preview`; the plugin
is brought in on first `.h5` preview (or via a `plugins.load` op), so `HDF5_jll`
is pulled only when a user actually opens an HDF5 file, never at kernel startup.

## The point

`HDF5Preview` is a third-party-shaped package — its own `Project.toml`, its own
`[deps]`, living outside `core/`. It adds a previewable file type with **one
subtype and two methods**, and the frontend renders the result with **no Rust
changes** because the payload is just a MIME plus bytes. That is the extension
surface working exactly as [the ABI](abi.md) promises: core ships as plugins to
itself, and a plugin from outside core travels the identical path.

## See also

- [Writing a FileType Plugin](filetype.md) — the generic build this example follows.
- [Discovery & Configuration](discovery.md) — the `[sot].extensions` mechanism.
- [The Dispatch ABI](abi.md) — why zero Rust changes are needed.
