# The Dispatch ABI

```@meta
CurrentModule = ConceptExplorerCore
```

The Ship of Tools extension model is its defining idea: **multiple dispatch is the plugin
system.** [`ConceptExplorerCore`](../ref/api-core.md) defines a small set of
abstract types, and methods on those types *are* the ABI. A package extends
Ship of Tools simply by being `using`-ed — the dispatch tables grow, with no
registration call, no plugin manifest, and no central list to edit.

This page is the conceptual contract. For the symbol-by-symbol reference see
[API — ConceptExplorerCore](../ref/api-core.md); for step-by-step tutorials see
[Writing a FileType Plugin](filetype.md) and [Writing a Mode Plugin](mode.md);
for a complete external package see the [HDF5 worked example](hdf5.md).

## The pluggable types

Six abstract types name everything that can be extended:

| Type | What it pluralizes | A plugin adds … |
|------|--------------------|-----------------|
| [`FileType`](@ref) | file kinds the explorer can preview / parse | `struct PngFile <: FileType end` |
| [`Mode`](@ref) | switchable nav-tree roots | `struct FilesMode <: Mode end` |
| [`ConceptEntity`](@ref) | conceptual units a file declares | a function / type / derivation entity |
| [`AnnotationKind`](@ref) | categories of concept annotation | a `TypeMeaning` kind |
| [`Tool`](@ref) | actions the orchestrator can call | a `ReadFile` tool |
| [`Capture`](@ref) | structured REPL outputs | a `FigureCapture` |

## The contract

Each pluggable type has a small set of methods a plugin implements. Defined in
core today:

```julia
# FileType
matches(::Type{<:FileType}, path)        -> Bool              # claim a path
preview(::Type{<:FileType}, path)        -> PreviewPayload    # render it
parse_entities(::Type{<:FileType}, path) -> Vector{ConceptEntity}

# ConceptEntity
ast_hash(::ConceptEntity)                -> String            # provenance key
applicable_annotations(::ConceptEntity)  -> Vector{Type{<:AnnotationKind}}
```

The remaining surfaces — `tree_root` / `tree_children` / `preview_for` for
[`Mode`](@ref), `tool_spec` / `tool_call` for [`Tool`](@ref), and
`capture_payload` for [`Capture`](@ref) — are dispatched the same way and are
implemented by the mode / tool / capture plugins layered above core. They are
part of the contract even though core does not define the generic stubs in
phase 1.

### A minimal FileType plugin

```julia
module PngPreview

using ConceptExplorerCore

struct PngFile <: FileType end

ConceptExplorerCore.matches(::Type{PngFile}, path) =
    endswith(lowercase(path), ".png")

ConceptExplorerCore.preview(::Type{PngFile}, path) =
    PreviewPayload("image/png", read(path))

end
```

`using PngPreview` and PNGs are previewable. Nothing else changed — no
registration step, and crucially no Rust code.

## The serialization seam

The Rust↔Julia boundary is crossed by exactly two generic structs, both with
**opaque payloads** so Rust never has to learn about new entity kinds:

- [`TreeNode`](@ref) — one node of a mode's column tree: `id`, `label`, `kind`,
  `has_children`, `badges`, and a kind-defined `payload` dictionary.
- [`PreviewPayload`](@ref) — a rendered preview: `mime`, `data` bytes, and an
  `extras` dictionary. The frontend dispatches on `mime` to pick a renderer.

Because both carry opaque, kernel-defined payloads, **adding a new `FileType`
requires zero Rust changes** — and a `Mode` will too, once the mode-plugin seam
is wired (modes are kernel-hosted today; see [Writing a Mode Plugin](mode.md)).
The frontend renders whatever the MIME says and draws the tree the kernel sends.

## Core is a plugin to itself

The core modes and the standard file types are implemented as methods on these
same abstract types — they receive **no privileged access**. If core ever needs
something the ABI cannot express, the rule is to *fix the ABI*, not to
special-case core. This keeps the extension surface honest: third-party plugins
travel exactly the path core travels.

## Discovery

Loaded `FileType` subtypes are found automatically with [`file_types`](@ref) (a
`subtypes(FileType)` scan), and the best match for a path is chosen by
[`file_type_for`](@ref). *Which* extension packages a project loads is declared
explicitly — see [Discovery & Configuration](discovery.md).

## Next steps

- [Writing a FileType Plugin](filetype.md)
- [Writing a Mode Plugin](mode.md)
- [Worked Example: HDF5](hdf5.md)
- [API — ConceptExplorerCore](../ref/api-core.md)
