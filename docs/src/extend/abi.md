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

| Type | What it pluralizes | A plugin adds … | Status |
|------|--------------------|-----------------|--------|
| [`FileType`](@ref) | file kinds the explorer can preview / parse | `struct PngFile <: FileType end` | built |
| [`Mode`](@ref) | switchable nav-tree roots | `struct FilesMode <: Mode end` | declared, NOT BUILT (no implementation) |
| [`ConceptEntity`](@ref) | conceptual units a file declares | a function / type / derivation entity | declared, NOT BUILT (no implementation) |
| [`AnnotationKind`](@ref) | categories of concept annotation | a `TypeMeaning` kind | declared, NOT BUILT (no implementation) |
| [`Tool`](@ref) | actions an agent could call | a `ReadFile` tool | declared, NOT BUILT (no implementation) |
| [`Capture`](@ref) | structured REPL outputs | a `FigureCapture` | declared, NOT BUILT (no implementation) |

Only `FileType` is wired end to end. The other five are declared in core and
nothing subtypes or dispatches on them: the window's modes are native Rust views,
not `Mode` subtypes.

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

The remaining surfaces — `tree_root` / `tree_children` / `preview_for` for
[`Mode`](@ref), `tool_spec` / `tool_call` for [`Tool`](@ref), and
`capture_payload` for [`Capture`](@ref) — are the design for the mode / tool /
capture plugins. Core does not define the generic stubs and nothing implements
them: NOT BUILT.

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
daemon's preview gates also name its extensions. `Mode` is NOT BUILT (the modes
are native Rust views; see [Writing a Mode Plugin](mode.md)).
The frontend renders whatever the MIME says and draws the tree the kernel sends.

## Core is a plugin to itself

The standard file types are implemented as methods on `FileType`, the same
abstract type a third-party plugin extends — they receive **no privileged
access**. (The core modes are not: they are native Rust views, since `Mode` is
not built.) If core ever needs
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
