# Discovery & Configuration

!!! warning "Status: partly built"
    Runtime resolution (path to `FileType`) is built. The `[sot].extensions` key (ADR 0006) is a design that is not
    built: nothing reads the `[sot]` table. Project-root detection below is what each process does today, not one
    shared rule.

```@meta
CurrentModule = ConceptExplorerCore
```

Two questions decide which plugins run and what they apply to: *which packages does the kernel load*, and *which
project am I in*. This page says what is built for each, shows the designed `[sot].extensions` table (unbuilt), and
explains how a path is resolved to a [`FileType`](@ref) at runtime once the right packages are loaded.

## Enabling plugins: the `[sot].extensions` key

What loads today: the seven standard plugins load with the kernel (`julia/kernel/src/ShipToolsKernel.jl`), and the
HDF5 example lazy-loads on the first `.h5` preview through the kernel's built-in extension table
(`LAZY_PLUGIN_FOR_EXT` in `julia/kernel/src/preview.jl`). The kernel loads only the packages in its own project,
`julia/kernel`. A third-party plugin loads only from a source checkout: add it to the kernel's `Project.toml` as a
`[deps]` entry and a `[sources]` path, then either add a `using` line to the kernel or map its file extensions in
`LAZY_PLUGIN_FOR_EXT` (the steps are in [Writing a FileType Plugin](filetype.md)). On a release install the kernel
lives inside the pinned release checkout, and updates refuse to move a checkout with local changes.

A plugin's [`FileType`](@ref) subtype is discovered automatically *once the package is loaded*. The design, not built,
is that the project's `Project.toml` carries a `[sot]` table listing the extensions to load:

```toml
[sot]
extensions = ["HDF5Preview", "MyOrgPlugin"]
```

In that design the kernel calls `Base.require` on each entry at startup, in declared order, before serving any
requests, and anything not listed is not loaded, even if installed. The alternative, auto-scanning loaded packages for
`ConceptExplorerCore` dependents, was rejected in favor of the explicit list. Nothing implements either today.

## Project-root detection

There is no single project-root rule. Each process answers "what project is this?" for itself:

- **The daemon.** `sotd` takes `--project-root <path>`, else `$SOT_PROJECT_ROOT`, else its working directory, as the
  filesystem root the Files-mode tree exposes. A row's own root is its project directory.
- **The REPL.** The row's persistent REPL activates the row's project directory as its environment, whether or not it
  holds a `Project.toml`. A fresh run of a file (`repl.run_file` with `fresh`) uses the row's `Project.toml` if it has
  one, else the nearest `Project.toml` above the file, else the row's directory.
- **The kernel.** It runs `--project=julia/kernel`, reads project sources statically, and never activates or loads
  the user's environment.

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
kernel calls [`preview`](@ref) on it. Nothing calls [`parse_entities`](@ref).

Loading the package into the kernel is enough to make its file kind previewable: its subtype joins the `file_types()`
scan that `file_type_for` walks. There is no central registry. See
[The Dispatch ABI](abi.md) for the contract behind these calls and
[Writing a FileType Plugin](filetype.md) for implementing `matches` / `preview` and for the one Rust change a plugin
with bounded output needs.

## See also

- [Writing a FileType Plugin](filetype.md) — `matches` / `preview` implementation.
- [Worked Example: HDF5](hdf5.md) — a real external plugin and how the kernel loads it.
- [API — ConceptExplorerCore](../ref/api-core.md) — `file_types`, `file_type_for`.
