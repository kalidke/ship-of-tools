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
using FileWatching.Pidfile: trymkpidlock

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

# The longest a plugin's tools may run for one request, in seconds, all of them together. The kernel serves one request
# at a time and the daemon gives up on a kernel answer at 10 s (`KERNEL_REQUEST_TIMEOUT`), so a request's tools end with
# room left to answer.
const TOOL_WAIT = 8

"""
    ToolFailure(program, what, exited)

What `read_within` throws when a process gives no output: it did not start, ran past the bound, or ended badly. Its
text is fixed words (the program's name and what happened), never the command, its environment or its output, because
a plugin puts it in a note a person reads and a command built from a JLL carries the whole environment. `exited` is
true when the process ran and ended with a failing status.
"""
struct ToolFailure <: Exception
    program::String
    what::String
    exited::Bool
end

Base.showerror(io::IO, e::ToolFailure) = print(io, e.program, " ", e.what)

"""
    error_words(e) -> String

The words a note, a reply or a log line may show for a caught exception, never the exception itself: a `ToolFailure`'s
text; a fixed sentence for a failed process or an I/O error, whose text may hold a command and with a JLL's command the
whole environment; the exception's type name for anything else. It does not cover a message someone built from a
command and wrapped in an `ErrorException`, so no code interpolates a command into a message.
"""
error_words(e) = e isa ToolFailure ? sprint(showerror, e) :
                 e isa ProcessFailedException ? "a process failed" :
                 e isa Base.IOError || e isa SystemError ? "an input/output error" :
                 "$(nameof(typeof(e)))"

"""
    read_within(cmd, seconds) -> Vector{UInt8}

`read(cmd)` with a bound: when `seconds` pass the process is killed (KILL, so a tool that ignores TERM ends too) and
its output pipe is closed, so a descendant still holding that pipe cannot hold the call. Every failure is a
[`ToolFailure`](@ref) naming the program and what happened (could not start, did not finish within the bound, exit
code or signal). As `read(cmd)` does, it reads through `open(cmd)`, whose stdin is `devnull`.
"""
function read_within(cmd::Cmd, seconds::Real)
    program = basename(cmd.exec[1])
    p = try
        open(cmd)
    catch
        throw(ToolFailure(program, "could not start", false))
    end
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
    timedout[] && throw(ToolFailure(program, "did not finish within $(seconds) s", false))
    if !success(p)
        throw(ToolFailure(program, p.termsignal != 0 ? "was ended by signal $(p.termsignal)" :
                                                       "exited with code $(p.exitcode)", true))
    end
    out
end

"""
    ToolSet(name, jll, uuid, compat, programs, size)

External programs a plugin runs, and Julia's copy of them for a computer that has none: the JLL package `jll` (with its
`uuid` and `compat`) holding every program in `programs`, `size` its download in words ("160 MB"), `name` what the
person's note calls it. See [`find_tools`](@ref).
"""
struct ToolSet
    name::String
    jll::String
    uuid::String
    compat::String
    programs::Vector{String}
    size::String
end

# The longest a fetch may run, in seconds, and the wait after a failed one before the next is tried.
const FETCH_BOUND = 600
const FETCH_PACE = Ref(600)
# The fetch's lock: a dead holder's lock is reclaimed once its file is LOCK_STALE s old; the holder touches it every
# LOCK_REFRESH s, and a live one silent for 5 * LOCK_STALE s counts as dead. The child gets both from `fetch_args`.
# START_AGE is how long a started fetch counts as running before its child has taken the lock.
const LOCK_STALE = 2
const LOCK_REFRESH = 0.5
const START_AGE = 15

# The arguments of `fetch_tool.jl` (its path first), whose `main` reads them in this order.
fetch_args(dir, set::ToolSet; bound = FETCH_BOUND) =
    string.([joinpath(@__DIR__, "fetch_tool.jl"), dir, set.jll, set.uuid, set.compat, bound, LOCK_STALE, LOCK_REFRESH, set.programs...])

# Seams a test stands other things in: the kernel's own lookup, the folder holding every copy ("" is the user's depot),
# and the command that fetches one.
const WHICH = Ref{Function}(Sys.which)
const TOOL_HOME = Ref("")
const TOOL_FETCH = Ref{Function}((dir, set) -> addenv(
    `$(Base.julia_cmd()) --startup-file=no --project=$dir $(fetch_args(dir, set))`,
    "JULIA_LOAD_PATH" => Sys.iswindows() ? "@;@stdlib" : "@:@stdlib"))   # the fetch's own: its folder and the standard library

tool_dir(set::ToolSet) = joinpath(isempty(TOOL_HOME[]) ? joinpath(first(DEPOT_PATH), "ship-of-tools") : TOOL_HOME[], set.jll)

# Whether a fetch is running: its child holds the pidfile lock for its life, and the parent's stamp covers the start-up
# before the child has taken it (the child removes the stamp when it has). Reclaiming a dead holder's lock logs the
# lock's path, so the probe logs nothing: the kernel's stderr lines are fixed words.
function fetching(dir)
    lock = Base.CoreLogging.with_logger(Base.CoreLogging.NullLogger()) do
        trymkpidlock(joinpath(dir, "fetch.lock"); stale_age = LOCK_STALE, refresh = LOCK_REFRESH)
    end
    lock === false || close(lock)
    lock === false || time() - mtime(joinpath(dir, "started")) < START_AGE
end

# What the fetch recorded in `copy.txt` (lines `key=value`: the library path variable and its value, then each
# program's file), as one command per program with the library path ahead of the process's own; `nothing` if there is
# no record, or a program's file or a folder of its library path is gone (a deleted artifact, or the library folder of
# a Julia since replaced).
function fetched_copy(dir, set::ToolSet)
    try
        pairs = (split(l, "="; limit = 2) for l in eachline(joinpath(dir, "copy.txt")) if occursin('=', l))
        kv = Dict(String(k) => String(v) for (k, v) in pairs)
        all(p -> isfile(get(kv, p, "")), set.programs) || return nothing
        all(isdir, filter(!isempty, split(kv["libpath"], Sys.iswindows() ? ';' : ':'))) || return nothing
        env = kv["libpath_env"]
        libs = join(filter(!isempty, [kv["libpath"], get(ENV, env, "")]), Sys.iswindows() ? ';' : ':')
        Dict(p => addenv(Cmd([kv[p]]), env => libs) for p in set.programs)
    catch
        nothing
    end
end

"""
    find_tools(set) -> Union{Dict{String,Cmd}, String}

Where a plugin's tools come from, decided at each preview and never remembered past a fetch's end or a miss: the programs
on the kernel's search path if all of `set.programs` are there; else Julia's copy, once fetched; else a string, the words
of a note saying why there is no tool yet. The fetch is a child process started here and never waited for: the first
preview that finds neither tool starts it and answers at once with the note, a preview while it runs says so and starts
none (the parent stamps the start before it spawns, so the child's start-up counts), one at a time per tool for this
user on this computer whatever number of kernels run (the lock is the child's own, so a child that died holds nothing). It is bounded by `FETCH_BOUND` seconds; a failure is kept as fixed words, shown by
every preview until `FETCH_PACE[]` seconds have passed, after which the next preview tries again. The copy is the JLL's
files in the user's depot, run as its own processes with the JLL's library path: nothing is loaded into this process.
"""
function find_tools(set::ToolSet)
    found = [WHICH[](p) for p in set.programs]
    all(!isnothing, found) && return Dict(p => Cmd([f]) for (p, f) in zip(set.programs, found))
    dir = tool_dir(set)
    copy = fetched_copy(dir, set)
    copy === nothing || return copy
    what = "Julia's copy of $(set.name) (about $(set.size))"
    mkpath(dir)
    fetching(dir) && return "$what is still being downloaded. Select the file again when it ends."
    failed = joinpath(dir, "failed.txt")
    left = FETCH_PACE[] - (time() - mtime(failed))
    if isfile(failed) && left > 0
        return "$what could not be downloaded: $(first(readline(failed), 200)). The next try is in $(ceil(Int, left / 60)) minutes."
    end
    try
        touch(joinpath(dir, "started"))
        open(joinpath(dir, "fetch.log"), "a") do log
            run(pipeline(TOOL_FETCH[](dir, set); stdin = devnull, stdout = log, stderr = log); wait = false)
        end
    catch
        rm(joinpath(dir, "started"); force = true)
        return "$what could not be downloaded: the download could not start."
    end
    "$(set.name) was not found on this computer, so $what is being downloaded now, once for this computer. Select the file again when it ends."
end

end # module
