# Writing a Mode Plugin

!!! note "Status: not built"
    `Mode` is declared in core and nothing implements it. The window's modes are native Rust views, and no mode-plugin
    package exists. This page describes the contract a third-party mode would dispatch on; the plugin-shaped surface
    that works today is [`FileType`](@ref) (see [Writing a FileType Plugin](filetype.md)).

```@meta
CurrentModule = ConceptExplorerCore
```

A [`Mode`](@ref) is, by design, a switchable root for the collapsible nav tree. The
built modes (Files, Modules, Sessions, and Hosts) are native Rust views, not
`Mode` subtypes: `Mode` is declared in core and NOT BUILT. A hotkey swaps which
mode's tree fills the navigation pane, and cursor position is preserved per mode
across switches. A mode plugin would supply a new such root.

This page documents the mode contract. For the conceptual overview see
[The Dispatch ABI](abi.md); for the symbol reference see
[API — ConceptExplorerCore](../ref/api-core.md).

## The Mode contract

In the design (NOT BUILT), a mode is a singleton subtype of [`Mode`](@ref) plus three
dispatched methods. The same three-column shape applies to every mode; the methods supply the tree
and its previews:

| Method | Returns | Role |
|--------|---------|------|
| `tree_root(::Type{M}, project)` | [`TreeNode`](@ref) | the mode's root node for `project` |
| `tree_children(::Type{M}, node)` | `Vector{`[`TreeNode`](@ref)`}` | children of a node (lazy column fill) |
| `preview_for(::Type{M}, node)` | [`PreviewPayload`](@ref) | preview for the selected node |

These mirror the [`FileType`](@ref) methods ([`matches`](@ref) /
[`preview`](@ref) / [`parse_entities`](@ref)) in style: a small set of generic
functions, one method per mode. As [the ABI page](abi.md) notes, core does not
ship the generic `tree_root` / `tree_children` / `preview_for` stubs in phase 1 —
they are dispatched the same way and implemented by the modes layered above core.

## The tree IR

Both mode outputs cross the Rust↔Julia boundary as opaque-payload structs, so the
frontend renders any mode without learning its internals:

- [`TreeNode`](@ref) — one node of a column. Fields: `id` (opaque to Rust),
  `label`, `kind` (a `Symbol` like `:module`, `:function`, `:pngfile`),
  `has_children`, `badges` (a `Vector{Symbol}` for the cross-cutting color layer,
  e.g. `:stale`, `:user_edited`, `:immutable`), and a kind-defined
  `payload::Dict{String,Any}` the frontend never inspects. The convenience
  constructor is `TreeNode(id, label, kind; has_children=false, badges=Symbol[],
  payload=Dict())`.
- [`PreviewPayload`](@ref) — the rendered preview for a node: `mime`, `data`
  bytes, and an `extras` dictionary. The frontend dispatches on `mime` to pick a
  renderer, exactly as it does for file previews.

Because the payloads are opaque, a `Mode` plugin would need no Rust change —
the frontend would draw whatever tree the kernel sends and render whatever the
preview's MIME names. That is the design; it is not built, since nothing sends a
mode's tree from a `Mode` subtype today.

## How a third-party mode would dispatch

The contract is symmetrical with the FileType case. A mode package would:

1. Depend on `ConceptExplorerCore` and `using` it.
2. Declare a singleton subtype: `struct MyMode <: ConceptExplorerCore.Mode end`.
3. Implement `tree_root` to build the root [`TreeNode`](@ref) from the project,
   setting `kind`, `has_children`, and any color-layer `badges`.
4. Implement `tree_children` to expand a node into the next column's nodes,
   reading the parent node's `payload` to know what to enumerate.
5. Implement `preview_for` to return a [`PreviewPayload`](@ref) for the selected
   node, choosing a MIME the frontend renders (see the MIME table in
   [Writing a FileType Plugin](filetype.md)).

Loading would follow the same route as FileType plugins, which is a kernel dependency
today, not a project setting — see [Discovery & Configuration](discovery.md).

## The current core modes

For reference, the modes the window has today, each a native Rust view (the ones a
hotkey switches between in the running app; the Modules tree is filled from the
kernel's project scan):

| Mode | Column 1 → 2 → 3 | Preview |
|------|------------------|---------|
| Files | parent dir → current dir → contents | file at appropriate fidelity |
| Modules (read-only) | modules → definitions (types, functions, macros, submodules) | method source + concept artifact |
| Sessions | workspaces → timeline → detail | session detail |
| Hosts | configured hosts → endpoint detail | host endpoint summary |

Types, Math, and Outputs modes are designed but not yet built; Agents mode is
not built (pinned for later). The [Modes](../guide/modes.md) page lists the full intended set and what is built.

## Why modes aren't packaged yet

Extracting a mode into a standalone package is deferred, not blocked: the
contract above is exactly what such a package would implement. Until the generic
stubs and a third-party mode example exist, treat this page as the design of the
mode seam and use the [`FileType`](@ref) path for a plugin you can ship today.

## See also

- [The Dispatch ABI](abi.md) — the conceptual contract and the serialization seam.
- [Writing a FileType Plugin](filetype.md) — the fully plugin-shaped surface today.
- [API — ConceptExplorerCore](../ref/api-core.md) — `Mode`, `TreeNode`, `PreviewPayload`.
