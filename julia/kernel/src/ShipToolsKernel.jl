module ShipToolsKernel

using Base64
using ConceptExplorerCore
# Built-in "core ships as plugins to itself" plugins. Loaded eagerly so
# the standard FileType subtypes are present from the first request —
# third-party plugins still come in via `plugins.load`. If a kernel
# image ever wants to start without these (e.g. a minimal sandbox), the
# `using` here is the only thing to drop.
using ShipToolsJsonDoc
using ShipToolsJuliaSource
using ShipToolsMarkdown
using ShipToolsPDFFile
using ShipToolsPlainText
using ShipToolsTomlDoc
using ShipToolsVideoFile
using JSON3
using JuliaSyntax
using JuliaSyntax: @K_str
# K"..." is JuliaSyntax's exported @K_str, imported explicitly at the top —
# re-declaring it locally is an error on Julia 1.11 (ADR 0030 pipeline caught it).
using SHA

export serve

const PROTOCOL_VERSION = 1

"""
    serve(io_in::IO, io_out::IO; project_root::AbstractString = pwd())

Run the kernel NDJSON dispatch loop on the given streams. One JSON request
per line on `io_in`; one JSON response per line on `io_out`, with optional
length-prefixed blob bytes following any response whose payload carries
`"blob": {"len": N, "mime": ...}`.

The wire format mirrors `docs/adr/0001-protocol.md`: envelopes are
`{v, id, kind, op, payload}`, with optional `rev` for revision-bearing
frames. The kernel is a pure-function service — it never bumps a session
revision of its own — so kernel responses leave `rev` unset and the backend
attaches its own session revision when proxying.

Stderr is free-text logging. The backend never reads it as data.
"""
function serve(io_in::IO, io_out::IO; project_root::AbstractString = pwd())
    state = KernelState(project_root)
    println(stderr, "sot-kernel ready · project_root=$(state.project_root) · julia=$(VERSION)")
    flush(stderr)

    for line in eachline(io_in)
        isempty(strip(line)) && continue
        req = try
            JSON3.read(line)
        catch e
            write_envelope(io_out, "res", 0, "kernel.parse_error",
                Dict(:error => "bad request: $(e)"))
            continue
        end

        id = get(req, :id, UInt64(0))
        op = get(req, :op, "")
        payload = get(req, :payload, Dict{Symbol,Any}())

        try
            # invokelatest so methods added by a prior `plugins.load` (which
            # mutates the world via `using`) are visible to subsequent ops in
            # the same serve loop.
            Base.invokelatest(dispatch, io_out, state, id, op, payload)
        catch e
            bt = sprint(showerror, e, catch_backtrace())
            println(stderr, "kernel exception in op=$op: $bt")
            flush(stderr)
            write_envelope(io_out, "res", id, op,
                Dict(:error => sprint(showerror, e), :code => "kernel_exception"))
        end
    end
end

mutable struct KernelState
    project_root::String
    # cache of (path, sha256_of_bytes) → JuliaSyntax.SyntaxNode root, so
    # repeat queries against an unchanged file skip the reparse. Cleared on
    # purpose-built ops if needed; mostly invalidated by content hash.
    parse_cache::Dict{String,Tuple{Vector{UInt8}, JuliaSyntax.SyntaxNode}}
end

KernelState(project_root::AbstractString) = KernelState(String(project_root), Dict())

include("definitions.jl")
include("preview.jl")
include("project_scan.jl")
include("tokenize.jl")

# ---- dispatcher ----

function dispatch(io::IO, state::KernelState, id, op, payload)
    if op == "kernel.hello"
        handle_hello(io, state, id, payload)
    elseif op == "modules.list"
        handle_modules_list(io, state, id, payload)
    elseif op == "file.parse"
        handle_file_parse(io, state, id, payload)
    elseif op == "plugins.list"
        handle_plugins_list(io, state, id, payload)
    elseif op == "plugins.load"
        handle_plugins_load(io, state, id, payload)
    elseif op == "file.preview"
        handle_file_preview(io, state, id, payload)
    elseif op == "function.methods"
        handle_function_methods(io, state, id, payload)
    elseif op == "project.discover"
        handle_project_discover(io, state, id, payload)
    elseif op == "project.scan"
        handle_project_scan(io, state, id, payload)
    elseif op == "markdown.tokenize"
        handle_markdown_tokenize(io, state, id, payload)
    else
        write_envelope(io, "res", id, op,
            Dict(:error => "unknown op: $op", :code => "unknown_op"))
    end
end

function handle_hello(io::IO, state::KernelState, id, _payload)
    # ADR 0030 §1/§2: report the kernel's REAL embedded package version (from
    # its Project.toml, resolved at runtime) instead of a hardcoded string, and
    # advertise the wire-contract PROTOCOL_VERSION so the backend can validate
    # BE↔kernel skew at hello (belt-and-suspenders — they ship as a unit).
    # `pkgversion` returns `nothing` if the module wasn't loaded as a package
    # (e.g. via `include`); fall back so the field is always a version string.
    ver = pkgversion(ShipToolsKernel)
    res = Dict(
        :kernel => "sot-kernel",
        :version => ver === nothing ? "0.0.0" : string(ver),
        :protocol => PROTOCOL_VERSION,
        :julia => string(VERSION),
        :project_root => state.project_root,
        :features => ["modules.list", "file.parse", "file.preview",
                      "function.methods", "project.discover", "project.scan",
                      "markdown.tokenize",
                      "plugins.list", "plugins.load"],
    )
    write_envelope(io, "res", id, "kernel.hello", res)
end

"""
    handle_plugins_list

Walk `subtypes(ConceptExplorerCore.FileType)` and report each plugin's
`FileType` subtype. Loaded plugins automatically appear here once their
module has been `using`-ed — no registration call required. Validates the
plugin ABI per the project's "core ships as plugins to itself" rule.
"""
function handle_plugins_list(io::IO, state::KernelState, id, _payload)
    types = ConceptExplorerCore.file_types()
    entries = [Dict(
        :name => string(nameof(T)),
        :module => string(parentmodule(T)),
        :matches_defined => hasmethod(ConceptExplorerCore.matches,
                                       Tuple{Type{T}, AbstractString}),
        :preview_defined => hasmethod(ConceptExplorerCore.preview,
                                       Tuple{Type{T}, AbstractString}),
    ) for T in types]
    write_envelope(io, "res", id, "plugins.list", Dict(:file_types => entries))
end

"""
    handle_plugins_load

Load a Julia package by name from the kernel's environment so its
`FileType` extensions register. Phase-1 only loads packages that are
already on the kernel's load path (added via Pkg.develop / Pkg.add); a
future revision can spawn a fresh Pkg sandbox for untrusted plugins.
"""
function handle_plugins_load(io::IO, state::KernelState, id, payload)
    name = String(get(payload, :name, ""))
    if isempty(name)
        write_envelope(io, "res", id, "plugins.load",
            Dict(:error => "missing name", :code => "bad_request"))
        return
    end
    try
        Core.eval(Main, Meta.parse("using $name"))
        write_envelope(io, "res", id, "plugins.load",
            Dict(:loaded => name, :file_types_count => length(ConceptExplorerCore.file_types())))
    catch e
        write_envelope(io, "res", id, "plugins.load",
            Dict(:error => sprint(showerror, e), :code => "load_failed", :name => name))
    end
end

"""
    handle_modules_list

Returns the modules currently loaded in this kernel image. For phase 1 this
is `Main` + everything visible from `Base.loaded_modules`. The frontend can
use this to seed Modules-mode's left column.

Each entry carries `path` when `Base.pathof(mod)` resolves to a source
file — that's how the frontend gets from a module name to a `file.parse`
target without a separate `module.locate` op. `null` for built-ins and
modules with no on-disk source (stdlib / synthetic).
"""
function handle_modules_list(io::IO, state::KernelState, id, _payload)
    pairs = sort!(collect(Base.loaded_modules); by = p -> string(p.first.name))
    mods = Dict[]
    for (pkgid, mod) in pairs
        path = try
            Base.pathof(mod)
        catch
            nothing
        end
        push!(mods, Dict(
            :name => string(pkgid.name),
            :uuid => string(pkgid.uuid),
            :is_main => (pkgid.name == :Main),
            :path => path === nothing ? nothing : String(path),
        ))
    end
    write_envelope(io, "res", id, "modules.list", Dict(:modules => mods))
end

"""
    resolve_request_path(state, path) -> String

Resolve a request `path` (project-relative or absolute; wire paths use
forward slashes cross-platform) against `project_root`. On Windows the
forward slashes MUST become backslashes before the filesystem call: the
daemon canonicalizes `project_root` into a `\\\\?\\` verbatim
(extended-length) path, and verbatim paths bypass Win32 normalization — a
`/` in the joined path is treated as a literal filename character, so
`isfile` reports "no such file" for a file that exists (found by the docs
capture pipeline, 2026-07-02).
"""
function resolve_request_path(state::KernelState, path::AbstractString)
    p = Sys.iswindows() ? replace(String(path), '/' => '\\') : String(path)
    return isabspath(p) ? p : joinpath(state.project_root, p)
end

"""
    handle_file_parse

Read the file at `payload.path` (relative to project_root if not absolute),
parse via JuliaSyntax, return:

- the AST hash (SHA-256 of the canonical pruned kind+text walk)
- top-level definition names + their kinds (function, struct, module, …)

The hash is what concept-annotation provenance is keyed on per ADR 0005.
"""
function handle_file_parse(io::IO, state::KernelState, id, payload)
    path = get(payload, :path, "")
    if isempty(path)
        write_envelope(io, "res", id, "file.parse",
            Dict(:error => "missing path", :code => "bad_request"))
        return
    end
    fullpath = resolve_request_path(state, path)
    if !isfile(fullpath)
        write_envelope(io, "res", id, "file.parse",
            Dict(:error => "no such file: $fullpath", :code => "io_error"))
        return
    end
    bytes = read(fullpath)
    ast_hash = bytes2hex(SHA.sha256(bytes))
    src = String(copy(bytes))
    tree = try
        JuliaSyntax.parseall(JuliaSyntax.SyntaxNode, src; filename = fullpath)
    catch e
        write_envelope(io, "res", id, "file.parse",
            Dict(:error => sprint(showerror, e), :code => "parse_error",
                 :ast_hash => ast_hash))
        return
    end
    defs = collect_definitions(tree)
    write_envelope(io, "res", id, "file.parse",
        Dict(:ast_hash => ast_hash, :path => fullpath, :definitions => defs))
end

"""
    handle_function_methods

Look up `\$module.\$name` in the currently-loaded image, call `methods()`
on it, and return one row per method:

```
{methods: [{module, name, file, line, sig, ast_hash}, …]}
```

- `module`, `name` echo the request (so the frontend doesn't have to
  thread them through its splice logic).
- `file`/`line` come straight from the `Method` object.
- `sig` is the standard `string(m)` repr (e.g. `bar(x::Int) @ Foo
  /path/to/file.jl:42`); the frontend trims the location half if it
  wants a cleaner column.
- `ast_hash` is re-derived by re-parsing the source file and matching
  the definition whose name + line match — keeps per-method drift
  detection consistent with `file.parse`. `null` when the source isn't
  available (Base, ccall-only methods) or no matching definition was
  found.

Errors short-circuit with `code` of `bad_request` (missing args) /
`module_not_found` / `function_not_found`. Per-method errors during
hashing degrade silently to `ast_hash: null` rather than failing the
whole response.
"""
function handle_function_methods(io::IO, state::KernelState, id, payload)
    mod_name = String(get(payload, :module, ""))
    fn_name  = String(get(payload, :name, ""))
    if isempty(mod_name) || isempty(fn_name)
        write_envelope(io, "res", id, "function.methods",
            Dict(:error => "missing module or name", :code => "bad_request"))
        return
    end
    mod = nothing
    for (pkgid, m) in Base.loaded_modules
        if string(pkgid.name) == mod_name
            mod = m
            break
        end
    end
    if mod === nothing
        write_envelope(io, "res", id, "function.methods",
            Dict(:error => "module not loaded: $mod_name", :code => "module_not_found"))
        return
    end
    fn = try
        getfield(mod, Symbol(fn_name))
    catch e
        write_envelope(io, "res", id, "function.methods",
            Dict(:error => sprint(showerror, e), :code => "function_not_found",
                 :module => mod_name, :name => fn_name))
        return
    end
    ms = methods(fn)
    # ast_hash cache keyed by source file path — re-parsing once per file
    # keeps the response O(files) rather than O(methods).
    file_defs = Dict{String, Vector{Dict}}()
    out = Dict[]
    for m in ms
        file = string(m.file)
        line = Int(m.line)
        ast_hash = nothing
        if isfile(file)
            defs = get(file_defs, file) do
                src = try
                    String(read(file))
                catch
                    ""
                end
                isempty(src) && return Dict[]
                tree = try
                    JuliaSyntax.parseall(JuliaSyntax.SyntaxNode, src; filename = file)
                catch
                    nothing
                end
                isnothing(tree) ? Dict[] : collect_definitions(tree)
            end
            file_defs[file] = defs
            for d in defs
                if d[:name] == fn_name && d[:line] == line
                    ast_hash = d[:ast_hash]
                    break
                end
            end
        end
        push!(out, Dict(
            :module   => mod_name,
            :name     => fn_name,
            :file     => file,
            :line     => line,
            :sig      => string(m),
            :ast_hash => ast_hash,
        ))
    end
    write_envelope(io, "res", id, "function.methods", Dict(:methods => out))
end

"""
    handle_project_discover

Walk up from `payload.path` looking for the nearest `Project.toml`. The
caller (a `repl.run_file` request, a "what's my project" diagnostic) wants
to know which `--project=...` argument to use when running this file.

Behaviour:
- If `path` is a file, start from its parent directory.
- If `path` is a directory, start from itself.
- Walk parents until we hit a `Project.toml` (return its directory) or the
  filesystem root (return the kernel's `project_root` as a fallback so the
  caller has a usable env even when the source tree doesn't have its own).
- `source` is `discovered` (found an own Project.toml), `fallback` (used
  the kernel's `project_root`), or `none` (no path resolved).

Wire shape:

```
req:  {kernel_op: "project.discover", kernel_payload: {path: "..."}}
res:  {project_dir, project_toml | null, source, fallback, path}
```
"""
function handle_project_discover(io::IO, state::KernelState, id, payload)
    path = String(get(payload, :path, ""))
    if isempty(path)
        write_envelope(io, "res", id, "project.discover",
            Dict(:error => "missing path", :code => "bad_request"))
        return
    end
    dir, toml, source = discover_project(path; fallback = state.project_root)
    write_envelope(io, "res", id, "project.discover", Dict(
        :path         => abspath(path),
        :project_dir  => dir,
        :project_toml => toml,
        :source       => string(source),
        :fallback     => state.project_root,
    ))
end

"""
    discover_project(path; fallback=nothing) -> (dir, toml, source)

Pure helper used both by `project.discover` directly and by any
project-aware op that needs to resolve a `--project=...` from a file path
(e.g. `repl.run_file`).

- `dir` — absolute path of the directory to pass as `--project=`, or
  `nothing` if neither a discovered `Project.toml` nor a fallback exists.
- `toml` — absolute path to the discovered `Project.toml`, or `nothing`
  when only the fallback applies.
- `source` — `:discovered` / `:fallback` / `:none`.
"""
function discover_project(path::AbstractString;
                          fallback::Union{AbstractString, Nothing} = nothing)
    abs_in = isabspath(path) ? String(path) : abspath(String(path))
    start_dir = if isdir(abs_in)
        abs_in
    elseif isfile(abs_in)
        dirname(abs_in)
    else
        # Path doesn't exist on disk (frontend sent a stale path, etc.).
        # Still try the textual walk — `dirname` of a nonexistent file is
        # well-defined and may point at a real directory with a Project.toml.
        dirname(abs_in)
    end
    dir = start_dir
    while !isempty(dir)
        toml = joinpath(dir, "Project.toml")
        if isfile(toml)
            return (dir, toml, :discovered)
        end
        parent = dirname(dir)
        parent == dir && break
        dir = parent
    end
    if fallback !== nothing && !isempty(String(fallback))
        return (String(fallback), nothing, :fallback)
    end
    return (nothing, nothing, :none)
end

# ---- wire helpers ----

function write_envelope(io::IO, kind, id, op, payload)
    env = Dict(:v => PROTOCOL_VERSION, :id => id, :kind => kind, :op => op, :payload => payload)
    JSON3.write(io, env)
    write(io, '\n')
    flush(io)
end

end # module
