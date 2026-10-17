# The Dispatch ABI

!!! warning "Status: partly built"
    Only `FileType` is wired end to end; the other five pluggable types are declared in core and nothing implements
    them. Nothing discovers a user's packages and nothing reads a `[sot]` table: a `FileType` plugin loads only when
    it is a dependency of the kernel's own project, so adding one means editing a source checkout of Ship of Tools
    (see [Writing a FileType Plugin](filetype.md), Step 5).

```@meta
CurrentModule = ConceptExplorerCore
```

The Ship of Tools extension model is its defining idea: **multiple dispatch is the plugin
system.** [`ConceptExplorerCore`](../ref/api-core.md) defines a small set of
abstract types, and methods on those types *are* the ABI. A package extends
Ship of Tools by being loaded in the kernel — the dispatch tables then grow, with no
registration call. Today "loaded in the kernel" means a dependency of `julia/kernel` plus a `using` line or a
`LAZY_PLUGIN_FOR_EXT` row in a source checkout; nothing loads a package from a user's project.

This page is the conceptual contract. For the symbol-by-symbol reference see
[API — ConceptExplorerCore](../ref/api-core.md); for step-by-step tutorials see
[Writing a FileType Plugin](filetype.md) and [Writing a Mode Plugin](mode.md);
for a complete external package see the [HDF5 worked example](hdf5.md).

## The pluggable types

Six abstract types name everything that can be extended:

| Type | What it pluralizes | A plugin adds … | Status |
|------|--------------------|-----------------|--------|
| [`FileType`](@ref) | file kinds the explorer can preview / parse | `struct MyFile <: FileType end` | built |
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
preview(::Type{<:FileType}, path, params) -> PreviewPayload   # the form the kernel calls; falls back to the above
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
module MyPreview

using ConceptExplorerCore

struct MyFile <: FileType end

ConceptExplorerCore.matches(::Type{MyFile}, path) =
    endswith(lowercase(path), ".myext")

ConceptExplorerCore.preview(::Type{MyFile}, path) =
    PreviewPayload("text/plain", read(path))

end
```

Once the package is loaded in the kernel (see [Discovery](discovery.md)), `.myext` files
are previewed through it, with no registration call.

## [The serialization seam](@id abi-serialization-seam)

The kernel and the daemon talk in JSON lines; the daemon and the window use Rust types. Julia values do not cross as
structs:

- A plugin's [`preview`](@ref) returns a [`PreviewPayload`](@ref): `mime`, `data` bytes and an `extras` dictionary.
  The kernel re-encodes it as the reply to `file.preview`: `mime`, the bytes as base64 in `blob_base64`, `extras`, and
  for text-like MIME types the decoded `text`. The daemon forwards the bytes to the window as a Rust `PreviewPayload`
  (`rust/protocol/src/ir.rs`), and the window picks a renderer from `mime`.
- [`TreeNode`](@ref) — `id`, `label`, `kind`, `has_children`, `badges` and a kind-defined `payload` — is declared in
  core and no Julia code constructs one outside core's tests. Rust builds every tree node, from the kernel's
  `project.scan` reply and the daemon's file listings.

Because the window dispatches on the MIME type, **adding a new `FileType` needs no frontend change**; if its output
must be bounded, the daemon's preview gates also name its extensions (see [Writing a FileType Plugin](filetype.md),
Step 5). `Mode` is NOT BUILT (the modes are native Rust views; see [Writing a Mode Plugin](mode.md)).

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
`subtypes(FileType)` scan), and the first claimant of a path is chosen by
[`file_type_for`](@ref). *Which* packages the kernel loads is fixed by the
kernel's own project and sources, not by the user's project — see [Discovery & Configuration](discovery.md).

## Next steps

- [Writing a FileType Plugin](filetype.md)
- [Writing a Mode Plugin](mode.md)
- [Worked Example: HDF5](hdf5.md)
- [API — ConceptExplorerCore](../ref/api-core.md)
