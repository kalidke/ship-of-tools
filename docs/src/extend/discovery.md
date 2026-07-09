# Discovery & Configuration

```@meta
CurrentModule = ConceptExplorerCore
```

Two questions decide which plugins run and what they apply to: *which extension
packages does a project load*, and *which project am I in*. Ship of Tools answers both
explicitly — an `[sot]` table in the project's `Project.toml`, and an
outermost-`Project.toml` root rule. This page covers both, and how a path is
resolved to a [`FileType`](@ref) at runtime once the right packages are loaded.

## Enabling plugins: the `[sot].extensions` key

!!! warning "Design, not yet shipped (v0.3.x)"
    The `[sot].extensions` mechanism on this page is the **committed design**
    (ADR 0006) but is **not implemented yet**. What works today: the seven
    standard plugins load automatically with the kernel; the HDF5 example
    lazy-loads on first `.h5` preview (via a built-in extension table); and any
    other plugin package already in the kernel's environment can be loaded at
    runtime with the `plugins.load` kernel op (`using`-by-name). To enable a
    third-party plugin today, `Pkg.develop`/`Pkg.add` it into
    `julia/kernel`'s environment and load it with `plugins.load`.

A plugin's [`FileType`](@ref) subtype is discovered automatically *once the
package is loaded* — but **whether** a package loads is an explicit choice, not an
auto-scan. The design: the project's `Project.toml` carries a `[sot]` table
listing the extensions to load:

```toml
[sot]
extensions = ["HDF5Preview", "MyOrgPlugin"]
```

The kernel calls `Base.require` on each entry at startup, in declared order,
before serving any requests. Anything not listed is **not loaded, even if it is
installed** in the active environment.

The alternative — auto-scanning loaded packages for `ConceptExplorerCore`
dependents — was rejected in favour of this explicit list. The reasoning:

- **Predictable.** No magic load order, no "why is this plugin active?" debugging.
- **Visible.** Adding a plugin is three explicit steps — install it, add it to
  `extensions`, restart the kernel.
- **Compatible with static tool registration.** The full plugin set is known at
  kernel startup.

Because there is no auto-discovery, plugins are discoverable only through
documentation — so a plugin README should lead with its install + enable snippet.

## Project-root detection

The kernel needs one unambiguous answer to "what project is this?" The rule:

- Walk upward from the current directory until a `Project.toml` is found. That
  directory is the project root.
- In monorepos with nested `Project.toml` files (umbrella + sub-packages), the
  **outermost** one — highest in the tree — wins by default.
- Override with the backend's `--project <path>` flag.
- If no `Project.toml` is found before the filesystem root, the backend exits with
  an error telling you to run inside a Julia project or pass `--project`.

The kernel then activates that environment (`Pkg.activate(<root>)`), and plugin
discovery reads `[sot].extensions` from that **same** `Project.toml`. Root
detection and plugin loading are anchored on one file.

## Runtime resolution: path → FileType

With the right packages loaded, resolving a path to a renderer is two functions
in `ConceptExplorerCore`:

| Function | Returns | Behaviour |
|----------|---------|-----------|
| [`file_types`](@ref)`()` | `Vector{Type{<:FileType}}` | every loaded subtype — `subtypes(FileType)` |
| [`file_type_for`](@ref)`(path)` | `Union{Type{<:FileType}, Nothing}` | first subtype whose [`matches`](@ref) is `true`, else `nothing` |

`file_type_for` walks `file_types()` in order; for each subtype it checks there is
a `matches(::Type{T}, path)` method and calls it, returning the **first** type
that claims the path. When nothing claims it, the result is `nothing` — a visible
"no plugin handles this" rather than a guessed render. Once a type is chosen, the
kernel calls [`preview`](@ref) (and, where wired, [`parse_entities`](@ref)) on it.

This is why enabling a plugin is enough to make its file kind previewable: load
the package (via `extensions`), and its subtype joins the `file_types()` scan that
`file_type_for` walks. No central registry, no Rust change. See
[The Dispatch ABI](abi.md) for the contract behind these calls and
[Writing a FileType Plugin](filetype.md) for implementing `matches` / `preview`.

## A complete `[sot]` snippet

A project that enables the HDF5 example plus a hypothetical org plugin:

```toml
name = "MyProject"
uuid = "..."
version = "0.1.0"

[deps]
# ... your project's own dependencies ...

[sot]
extensions = ["HDF5Preview", "MyOrgPlugin"]
```

The extension packages themselves must be resolvable in the project's
environment (added to `[deps]` or otherwise on the load path), since
`Base.require` loads them by name.

!!! note "Phase-2 diagnostic"
    There is no auto-discovery of installed-but-unenabled plugins today.
    A possible `--list-installed-extensions` diagnostic is flagged for a later
    phase to surface candidates you have not enabled yet.

## See also

- [Writing a FileType Plugin](filetype.md) — `matches` / `preview` implementation.
- [Worked Example: HDF5](hdf5.md) — enabling a real external plugin.
- [API — ConceptExplorerCore](../ref/api-core.md) — `file_types`, `file_type_for`.
