"""
    ConceptExplorerCore

The plugin ABI for Ship of Tools.

A small set of abstract types defines what is pluggable; methods on them are
the contract. A package extends the system simply by being `using`-ed — the
dispatch tables grow, with no registration step and no manifest.

The surface is three abstract types — [`FileType`](@ref),
[`ConceptEntity`](@ref) and [`AnnotationKind`](@ref) — plus two serialization
structs, [`TreeNode`](@ref) and [`PreviewPayload`](@ref), that carry opaque
payloads across the Rust↔Julia boundary.

Implementation status: **`FileType` is the surface that is wired
end-to-end today** — the seven standard file types ship as ordinary
`matches`/`preview` methods with no privileged access, and third-party plugins
(e.g. the HDF5 example) travel the identical path. The other two types are
declared design seams: no concrete subtype of `ConceptEntity` or
`AnnotationKind` exists yet. The current navigation modes are built into the
frontend/backend. Future mods are specified by
[ADR 0051](https://github.com/kalidke/ship-of-tools/blob/main/docs/history/adr/0051-mods.md#the-mod)
and are not built.

See the *Extending* section of the manual — *The Dispatch ABI* and the
*Writing a FileType Plugin* / HDF5 worked-example pages — for the full
contract.
"""
module ConceptExplorerCore

using InteractiveUtils: subtypes
using JSON3
using JuliaSyntax
using SHA

export FileType, ConceptEntity, AnnotationKind
export TreeNode, PreviewPayload
export preview, parse_entities, ast_hash, applicable_annotations, matches
export file_types, file_type_for

"""
    FileType

Abstract supertype for the kinds of file the explorer can preview and parse —
PNG, Julia source, Markdown, JSON, and so on.

A plugin adds a kind with `struct MyKind <: FileType end`, declares which paths
it claims with [`matches`](@ref), and implements [`preview`](@ref) (and, where
the kind declares concept entities, [`parse_entities`](@ref)). Loaded subtypes
are discovered automatically by [`file_types`](@ref); the best match for a path
is resolved by [`file_type_for`](@ref). Adding a `FileType` requires zero
changes on the Rust side.
"""
abstract type FileType end

"""
    ConceptEntity

Abstract supertype for a conceptual unit a file declares — a function, type,
module, or math derivation — as distinct from the file's raw bytes.

[`parse_entities`](@ref) extracts them from a file; [`ast_hash`](@ref) gives
each a stable provenance key (used to detect annotation staleness), and
[`applicable_annotations`](@ref) reports which [`AnnotationKind`](@ref)s may
decorate it.
"""
abstract type ConceptEntity end

"""
    AnnotationKind

Abstract supertype for a category of LLM/user-authored annotation that can be
attached to a [`ConceptEntity`](@ref) — type-meaning, math-derivation, and so
on. [`applicable_annotations`](@ref) reports which kinds are valid for a given
entity.
"""
abstract type AnnotationKind end

"""
    preview(::Type{<:FileType}, path) -> PreviewPayload
    preview(::Type{<:FileType}, path, params::AbstractDict) -> PreviewPayload

Render a file at `path` to a typed preview payload. Plugins extend this
method on their own `<:FileType` subtype.

The 3-arg form carries request parameters (e.g. `"page"` for paginated
content — ADR 0021). Callers (the kernel) always invoke the 3-arg form;
the fallback below drops the params so 2-arg plugins work untouched.
Paginated plugins override the 3-arg form and report position via
`PreviewPayload.extras` (e.g. `"page"`/`"page_count"`).
"""
function preview end

preview(T::Type{<:FileType}, path, params::AbstractDict) = preview(T, path)

"""
    parse_entities(::Type{<:FileType}, path) -> Vector{ConceptEntity}

Extract the conceptual entities the file declares (modules, types,
functions, etc.). Phase-1 returns an empty vector for unknown types.
"""
function parse_entities end

"""
    ast_hash(e::ConceptEntity) -> String

Stable identifier for a concept entity's textual realisation. Used as the
provenance key in `.concept/` annotation frontmatter (`synced_against`).
"""
function ast_hash end

"""
    applicable_annotations(e::ConceptEntity) -> Vector{Type{<:AnnotationKind}}

Which annotation kinds can decorate this entity. Plugins extend.
"""
function applicable_annotations end

"""
    file_types() -> Vector{Type{<:FileType}}

Every loaded `FileType` subtype. Computed via `subtypes(FileType)` so the
list grows automatically when an extension package is `using`-ed.
"""
file_types() = subtypes(FileType)

"""
    file_type_for(path) -> Union{Type{<:FileType}, Nothing}

Best-match `FileType` for `path`. Each plugin's `FileType` subtype is
expected to define `matches(::Type{<:FileType}, path) -> Bool`; the first
match wins. Falls back to `nothing` when no plugin claims the path.
"""
function file_type_for(path::AbstractString)
    for T in file_types()
        if hasmethod(matches, Tuple{Type{T}, AbstractString}) && matches(T, path)
            return T
        end
    end
    return nothing
end

"""
    matches(::Type{<:FileType}, path) -> Bool

Plugin contract: return true if this `FileType` should claim `path`. Phase-1
plugins typically inspect the file extension. The first plugin to return
true gets the preview / parse_entities calls.
"""
function matches end

"""
    TreeNode(id, label, kind; has_children=false, badges=Symbol[], payload=Dict())

One node in a mode's navigation tree, and the unit of structure that
crosses to the frontend. Fields:

- `id::String` — opaque, kernel-defined identity (Rust never interprets it).
- `label::String` — display text.
- `kind::Symbol` — node kind for icon/coloring (`:module`, `:function`,
  `:pngfile`, …).
- `has_children::Bool` — whether the node can be descended into.
- `badges::Vector{Symbol}` — cross-cutting provenance/state marks (`:stale`,
  `:user_edited`, `:immutable`, …) rendered uniformly across every mode.
- `payload::Dict{String,Any}` — opaque, kind-specific data the kernel
  round-trips.

The keyword constructor fills the common defaults.
"""
struct TreeNode
    id::String
    label::String
    kind::Symbol
    has_children::Bool
    badges::Vector{Symbol}
    payload::Dict{String,Any}
end

TreeNode(id, label, kind; has_children=false, badges=Symbol[], payload=Dict{String,Any}()) =
    TreeNode(id, label, kind, has_children, badges, payload)

"""
    PreviewPayload(mime, data, extras=Dict())

A rendered preview crossing to the frontend: a MIME type, the raw `data` bytes,
and an opaque `extras` dictionary for kind-specific metadata (for example
`"page"`/`"page_count"` for paginated content). The frontend dispatches on
`mime` to choose a renderer and never needs to learn about new entity kinds.
"""
struct PreviewPayload
    mime::String
    data::Vector{UInt8}
    extras::Dict{String,Any}
end

PreviewPayload(mime, data) = PreviewPayload(mime, data, Dict{String,Any}())

"""
    read_within(cmd, seconds) -> Vector{UInt8}

`read(cmd)` with a bound: when `seconds` pass the process is killed (KILL, so a tool that ignores TERM ends too) and
its output pipe is closed, so a descendant still holding that pipe cannot hold the call; the error names the program
and the bound. As `read(cmd)` does, it reads through `open(cmd)`, whose stdin is `devnull`.
"""
function read_within(cmd::Cmd, seconds::Real)
    p = open(cmd)
    done, timedout = Ref(false), Ref(false)
    timer = Timer(seconds) do _
        done[] && return
        timedout[] = true
        process_running(p) && kill(p, Base.SIGKILL)
        close(p.out)
    end
    out = try
        bytes = read(p.out)
        wait(p)
        bytes
    catch
        timedout[] || rethrow()
        UInt8[]
    finally
        done[] = true
        close(timer)
    end
    timedout[] && error("$(basename(cmd.exec[1])) did not finish within $(seconds) s")
    success(p) || throw(ProcessFailedException(p))
    out
end

end # module
