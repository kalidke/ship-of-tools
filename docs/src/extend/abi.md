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
[Writing a FileType Plugin](filetype.md);
for a complete external package see the [HDF5 worked example](hdf5.md).

## The pluggable types

Core declares three abstract types:

| Type | What it pluralizes | A plugin adds … | Status |
|------|--------------------|-----------------|--------|
| [`FileType`](@ref) | file kinds the explorer can preview / parse | `struct PngFile <: FileType end` | built |
| [`ConceptEntity`](@ref) | conceptual units a file declares | a function / type / derivation entity | declared, NOT BUILT (no implementation) |
| [`AnnotationKind`](@ref) | categories of concept annotation | a `TypeMeaning` kind | declared, NOT BUILT (no implementation) |

Only `FileType` is wired end to end. `ConceptEntity` and `AnnotationKind` are
declared design targets with no concrete subtypes. The window's navigation modes
are native Rust views.

## The contract

Each pluggable type has a small set of methods a plugin implements. Defined in
core today:

```julia
# FileType
matches(::Type{<:FileType}, path)        -> Bool              # claim a path
preview(::Type{<:FileType}, path)        -> PreviewPayload    # render it
parse_entities(::Type{<:FileType}, path) -> Vector{ConceptEntity}   # declared, NOT BUILT: no caller

# ConceptEntity
ast_hash(::ConceptEntity)                -> String            # declared, NOT BUILT: no method, no caller
applicable_annotations(::ConceptEntity)  -> Vector{Type{<:AnnotationKind}}   # declared, NOT BUILT: no method, no caller
```

`matches` and `preview` are built. `parse_entities` has methods (the standard
plugins return an empty vector) but nothing calls it; `ast_hash` and
`applicable_annotations` have no method at all. They are declared design targets.
The staleness hash that is built today is not `ast_hash`: see
[The Concept Layer](../guide/concept-layer.md).

### Planned mods

`Mode`, `Tool` and `Capture` have been removed from core.
[ADR 0051](https://github.com/kalidke/ship-of-tools/blob/main/docs/history/adr/0051-mods.md#what-is-deleted)
specifies what replaces them in a 0.7.Z after the 0.7.0 tag: `Mod` with
`tree_children` and the three-argument `preview` for views, `:repl` events for
structured output, and MCP servers if a mod needs to offer tools to agents.
The mod system is not built.

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

Once the package is loaded in the kernel (see [Discovery](discovery.md)), PNGs
are previewed through it: no registration step and no Rust code.

## The serialization seam

The Rust↔Julia boundary is crossed by exactly two generic structs, both with
**opaque payloads** so Rust never has to learn about new entity kinds:

- [`TreeNode`](@ref) — one node of a mode's column tree: `id`, `label`, `kind`,
  `has_children`, `badges`, and a kind-defined `payload` dictionary. Declared in
  core but not built: no Julia code constructs one today; the window's trees
  travel as the Rust wire type of the same shape.
- [`PreviewPayload`](@ref) — a rendered preview: `mime`, `data` bytes, and an
  `extras` dictionary. The frontend dispatches on `mime` to pick a renderer.

Because both carry opaque, kernel-defined payloads, **adding a new `FileType`
needs no Rust change** unless its output must be bounded, in which case the
daemon's preview gates also name its extensions. The frontend renders file
previews by MIME. Navigation modes are native Rust views; future view extensions
follow the planned mod design above.

## Core is a plugin to itself

The standard file types are implemented as methods on `FileType`, the same
abstract type a third-party plugin extends — they receive **no privileged
access**. Built-in navigation modes are native Rust views. If core ever needs
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
- [ADR 0051: mods](https://github.com/kalidke/ship-of-tools/blob/main/docs/history/adr/0051-mods.md)
- [Worked Example: HDF5](hdf5.md)
- [API — ConceptExplorerCore](../ref/api-core.md)
