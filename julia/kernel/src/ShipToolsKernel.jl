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

# Built-in plugins that are NOT eagerly `using`d at kernel startup (to keep
# their heavy deps out of the base image), keyed by the file extension they
# claim. On a `file.preview` miss we `using` the registered plugin once and
# re-resolve. Extensions lowercase, with leading dot. Add a row here when a new
# lazy plugin lands; the plugin must be a dep of the kernel env (Pkg.develop'd)
# so `using <name>` resolves.
const LAZY_PLUGIN_FOR_EXT = Dict{String,String}(
    ".h5"   => "HDF5Preview",
    ".hdf5" => "HDF5Preview",
    ".hdf"  => "HDF5Preview",
)

# Plugins we've already attempted to lazy-load this session, so a broken load
# (missing dep, precompile error) is tried once and then reported as a miss
# rather than retried — and spamming logs — on every subsequent preview.
const LAZY_LOAD_ATTEMPTED = Set{String}()

"""
    maybe_lazy_load_plugin(fullpath) -> Bool

If `fullpath`'s extension maps to a not-yet-loaded lazy plugin, `using` it once
(in `Main`, so its dispatch methods register globally) and return `true` on a
successful load. Returns `false` when there's no mapping, the load was already
attempted, or the load failed (logged to stderr). Caller re-resolves the
FileType via `invokelatest` because the new methods are a newer world age.
"""
function maybe_lazy_load_plugin(fullpath::AbstractString)
    ext = lowercase(splitext(fullpath)[2])
    name = get(LAZY_PLUGIN_FOR_EXT, ext, nothing)
    name === nothing && return false
    name in LAZY_LOAD_ATTEMPTED && return false
    push!(LAZY_LOAD_ATTEMPTED, name)
    try
        Core.eval(Main, Meta.parse("using $name"))
        println(stderr, "sot-kernel: lazy-loaded plugin $name for $ext")
        return true
    catch e
        println(stderr, "sot-kernel: lazy plugin load failed ($name): ",
                sprint(showerror, e))
        return false
    end
end

"""
    handle_file_preview

Route preview through the plugin dispatch table:
`ConceptExplorerCore.file_type_for(path)` picks the first matching
`FileType`, then `preview(::Type{T}, path)` returns a `PreviewPayload`.
Binary payloads are base64-encoded inline (`payload.blob_base64`); text
mimes also get a UTF-8 `text` field for convenience. If no plugin matches
the path, returns `{matched: false}` so callers can fall back to the
backend's bytes-level preview.

This is the runtime counterpart to `plugins.list`: it actually invokes
the dispatch surface, proving plugin extensions function end-to-end —
not just that the methods are defined.
"""
function handle_file_preview(io::IO, state::KernelState, id, payload)
    path = String(get(payload, :path, ""))
    if isempty(path)
        write_envelope(io, "res", id, "file.preview",
            Dict(:error => "missing path", :code => "bad_request"))
        return
    end
    fullpath = resolve_request_path(state, path)
    if !isfile(fullpath)
        write_envelope(io, "res", id, "file.preview",
            Dict(:error => "no such file: $fullpath", :code => "io_error"))
        return
    end
    T = ConceptExplorerCore.file_type_for(fullpath)
    if T === nothing
        # No loaded plugin claims this path. If a lazy built-in plugin is
        # registered for the extension, `using` it once and re-resolve — this
        # keeps heavy plugin deps (e.g. HDF5_jll) out of kernel startup while
        # still making preview "just work" the first time a user opens one.
        if maybe_lazy_load_plugin(fullpath)
            # Methods added by the `using` live in a newer world age than this
            # already-running function, so re-resolve via invokelatest.
            T = Base.invokelatest(ConceptExplorerCore.file_type_for, fullpath)
        end
    end
    if T === nothing
        write_envelope(io, "res", id, "file.preview",
            Dict(:matched => false, :path => fullpath))
        return
    end
    # Request params (ADR 0021, e.g. `page` for paginated previews).
    # Normalized to String keys at this seam — JSON3 objects key by Symbol,
    # but the plugin ABI (`preview(T, path, params::AbstractDict)`) shouldn't
    # inherit that wire detail. Always call the 3-arg form; core's fallback
    # drops the params for plugins that only define 2-arg.
    raw_params = get(payload, :params, nothing)
    params = Dict{String,Any}()
    if raw_params !== nothing
        for (k, v) in pairs(raw_params)
            params[string(k)] = v
        end
    end
    pp = try
        # invokelatest unconditionally: cheap, and required when T's plugin was
        # just lazy-loaded above (world-age).
        Base.invokelatest(ConceptExplorerCore.preview, T, fullpath, params)
    catch e
        write_envelope(io, "res", id, "file.preview",
            Dict(:error => sprint(showerror, e), :code => "plugin_threw",
                 :file_type => string(nameof(T))))
        return
    end
    out = Dict(
        :matched => true,
        :path => fullpath,
        :file_type => string(nameof(T)),
        :mime => pp.mime,
        :blob_base64 => Base64.base64encode(pp.data),
    )
    # Plugin-reported metadata (e.g. page/page_count). Forwarded opaquely by
    # the backend; the frontend reads only the keys it knows.
    if !isempty(pp.extras)
        out[:extras] = pp.extras
    end
    if startswith(pp.mime, "text/") || pp.mime == "application/json" ||
       endswith(pp.mime, "+json") || endswith(pp.mime, "+xml")
        out[:text] = String(copy(pp.data))
    end
    write_envelope(io, "res", id, "file.preview", out)
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

# Walk a JuliaSyntax tree, collect top-level definitions as
# {name, kind, line, ast_hash, parent}. Descends one level into module
# bodies so a typical Julia file (which usually wraps its contents in
# `module Foo ... end`) surfaces the inner definitions Modules-mode wants
# to see. `ast_hash` is per-entity: walking the entity's SyntaxNode
# subtree, the hash is stable under whitespace/comment edits (SyntaxNode
# already skips trivia) and sensitive to any structural or value change.
# Per-entity hashing is what concept-annotation `synced_against` is keyed
# on per ADR 0005 / CLAUDE.md — a file-level hash would mark every
# annotation stale on any edit, defeating the reactive-staleness UX.
# Build a single definition entry Dict from a child node, or `nothing` if the
# child isn't a recognized definition. Shared by `collect_definitions`
# (file.parse) and the project-scan walk (`scan_block_defs!`) so the entry shape
# — name/kind/line/ast_hash, plus struct fields/supertype and an optional
# `:parent` — lives in one place.
function def_entry(child, k, parent_name)
    name, kind_str, hash_node = definition_for(child, k)
    name === nothing && return nothing
    entry = Dict(
        :name => name,
        :kind => kind_str,
        :line => JuliaSyntax.source_line(child),
        :ast_hash => definition_ast_hash(hash_node),
    )
    if parent_name !== nothing
        entry[:parent] = parent_name
    end
    # Type-specific enrichments — F5 (supertype edge) + F6 (struct fields).
    # Both come from the same SyntaxNode we'd otherwise throw away after
    # hashing; cheap to extract so the Modules/Types nav can drill into a
    # type without a follow-up wire call.
    if kind_str == "struct"
        fields = extract_struct_fields(hash_node)
        isempty(fields) || (entry[:fields] = fields)
        sup = extract_supertype(hash_node)
        sup === nothing || (entry[:supertype] = sup)
    elseif kind_str == "abstract"
        sup = extract_supertype(hash_node)
        sup === nothing || (entry[:supertype] = sup)
    end
    return entry
end

function collect_definitions(node, parent_name = nothing)
    defs = Dict[]
    children = JuliaSyntax.children(node)
    isnothing(children) && return defs
    for child in children
        k = JuliaSyntax.kind(child)
        entry = def_entry(child, k, parent_name)
        entry === nothing && continue
        push!(defs, entry)
        if entry[:kind] == "module" && parent_name === nothing
            # Walk the module body one level deep so members surface in a
            # single-file parse (file.parse). `module_body_node` unwraps a
            # docstring wrapper so a documented module's block is found.
            # Nested modules and include()d submodule bodies are handled by the
            # project-scan walk (`scan_block_defs!`), not here.
            mnode = module_body_node(child, k)
            if !isnothing(mnode)
                for mk in JuliaSyntax.children(mnode)
                    if JuliaSyntax.kind(mk) == K"block"
                        append!(defs, collect_definitions(mk, entry[:name]))
                    end
                end
            end
        end
    end
    defs
end

# Returns `(name, kind_str, hash_node)`. `hash_node` is the SyntaxNode the
# entity's `ast_hash` should be computed from — usually the def itself,
# except for `K"doc"`-wrapped definitions where we strip the docstring so
# docstring edits don't invalidate the hash. The docstring is its own
# annotation surface; per-entity hash should be the code-only fingerprint.
function definition_for(child, k)
    if k == K"function"
        return (def_name(child), "function", child)
    elseif k == K"struct"
        return (def_name(child), "struct", child)
    elseif k == K"abstract"
        return (def_name(child), "abstract", child)
    elseif k == K"module"
        return (def_name(child), "module", child)
    elseif k == K"macro"
        return (def_name(child), "macro", child)
    elseif k == K"="
        # function f(x) = ... style. The lhs is a call; the name is the
        # call's first arg.
        kids = JuliaSyntax.children(child)
        if !isnothing(kids) && length(kids) >= 1 && JuliaSyntax.kind(kids[1]) == K"call"
            return (def_name(kids[1]), "function", child)
        end
    elseif k == K"doc"
        # Docstring + definition pair. The actual definition is the second
        # child (the first is the docstring expression).
        kids = JuliaSyntax.children(child)
        if !isnothing(kids) && length(kids) >= 2
            name, kind_str, _ = definition_for(kids[2], JuliaSyntax.kind(kids[2]))
            # Hash the inner def, not the K"doc" wrapper — docstring edits
            # leave `ast_hash` unchanged.
            return (name, kind_str, kids[2])
        end
    end
    return (nothing, "", child)
end

# The actual `module` SyntaxNode for a child that is a module definition,
# unwrapping a K"doc" docstring wrapper if present (`"docs" module M … end`
# parses as K"doc"[string, module]). Returns `nothing` if `child` isn't a
# module def. Used to reach the module body's block for descent — without this,
# a docstringed module's members are never collected (the block sits inside the
# `module` node, not the `doc` wrapper we'd otherwise walk).
function module_body_node(child, k)
    if k == K"module"
        return child
    elseif k == K"doc"
        kids = JuliaSyntax.children(child)
        if !isnothing(kids) && length(kids) >= 2 && JuliaSyntax.kind(kids[2]) == K"module"
            return kids[2]
        end
    end
    return nothing
end

# Walk a struct's K"block" body, collect typed and untyped field
# declarations into `[{name, type, line}]`. Skips inner constructors
# (K"function" / K"call" defs) and any other non-field expressions a
# user might put inside a struct body. `type` is the verbatim source
# text of the type expression (`Vector{Int}`, `Tuple{Symbol, Any}`) or
# the empty string for an untyped field. Default-value field declarations
# (`x::Int = 0`) recurse into the LHS to extract name+type, dropping
# the default — the default value isn't part of the field signature
# the nav cares about.
function extract_struct_fields(struct_node)
    fields = Dict[]
    kids = JuliaSyntax.children(struct_node)
    isnothing(kids) && return fields
    for c in kids
        JuliaSyntax.kind(c) == K"block" || continue
        body_kids = JuliaSyntax.children(c)
        isnothing(body_kids) && continue
        for stmt in body_kids
            push_field_if_field!(fields, stmt)
        end
    end
    return fields
end

function push_field_if_field!(fields, node)
    k = JuliaSyntax.kind(node)
    if k == K"::"
        kids = JuliaSyntax.children(node)
        if !isnothing(kids) && length(kids) == 2
            name_node, type_node = kids[1], kids[2]
            if JuliaSyntax.kind(name_node) == K"Identifier"
                push!(fields, Dict(
                    :name => string(JuliaSyntax.sourcetext(name_node)),
                    :type => string(JuliaSyntax.sourcetext(type_node)),
                    :line => JuliaSyntax.source_line(node),
                ))
            end
        end
    elseif k == K"Identifier"
        push!(fields, Dict(
            :name => string(JuliaSyntax.sourcetext(node)),
            :type => "",
            :line => JuliaSyntax.source_line(node),
        ))
    elseif k == K"="
        # `x::T = default` or `x = default` — pull name+type from LHS,
        # ignore the default expression on the RHS.
        kids = JuliaSyntax.children(node)
        if !isnothing(kids) && length(kids) >= 1
            push_field_if_field!(fields, kids[1])
        end
    end
    # K"function" / K"call" inside a struct body is an inner
    # constructor; the constructor-merge pass in `build_module_tree`
    # already handles those, so we skip here.
end

# Pull the supertype identifier (text) out of a K"struct" / K"abstract"
# definition. Walks the top-level children for a K"<:" expression and
# returns the source text of its RHS. Handles parametric forms like
# `Foo{T} <: Bar` and `Foo{T} <: Bar{T}` — for the parametric case the
# returned text is the full `Bar{T}`, which the nav can use as-is or
# strip down to just `Bar` for hierarchy grouping. Returns `nothing`
# when the type has no explicit supertype (defaults to `Any`).
function extract_supertype(type_node)
    kids = JuliaSyntax.children(type_node)
    isnothing(kids) && return nothing
    for c in kids
        if JuliaSyntax.kind(c) == K"<:"
            sub_kids = JuliaSyntax.children(c)
            if !isnothing(sub_kids) && length(sub_kids) == 2
                return string(JuliaSyntax.sourcetext(sub_kids[2]))
            end
        end
    end
    return nothing
end

# Per-entity AST hash. SHA-256 of a deterministic kind+leaf-text walk
# of the SyntaxNode subtree. SyntaxNode already excludes trivia
# (whitespace/comments), so the hash is whitespace- and comment-stable
# but flips on any structural or value change. NUL bytes separate fields
# to keep the byte stream unambiguous across kind/text boundaries.
function definition_ast_hash(node)
    ctx = SHA.SHA2_256_CTX()
    walk_for_hash!(ctx, node)
    bytes2hex(SHA.digest!(ctx))
end

function walk_for_hash!(ctx, node)
    SHA.update!(ctx, codeunits(string(JuliaSyntax.kind(node))))
    SHA.update!(ctx, UInt8[0x00])
    kids = JuliaSyntax.children(node)
    if isnothing(kids) || isempty(kids)
        SHA.update!(ctx, codeunits(JuliaSyntax.sourcetext(node)))
        SHA.update!(ctx, UInt8[0x00])
    else
        for c in kids
            walk_for_hash!(ctx, c)
        end
    end
end

function def_name(node)
    kids = JuliaSyntax.children(node)
    isnothing(kids) && return nothing
    for c in kids
        ck = JuliaSyntax.kind(c)
        if ck == K"Identifier"
            return string(JuliaSyntax.sourcetext(c))
        elseif ck == K"call"
            return def_name(c)
        elseif ck == K"<:" || ck == K"curly"
            # `struct Foo <: Bar`, `struct Vec{T}`, or the combination —
            # JuliaSyntax wraps the name in a K"<:" / K"curly" node before
            # exposing the Identifier. Descend once; the first nested
            # Identifier (or further-wrapped Identifier) is the def name.
            n = def_name(c)
            if n !== nothing
                return n
            end
        end
    end
    return nothing
end

# K"..." is JuliaSyntax's exported @K_str, imported explicitly at the top —
# re-declaring it locally is an error on Julia 1.11 (ADR 0030 pipeline caught it).

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

"""
    handle_project_scan

Walk the project's package source tree and return a nested
`modules → types/functions/submodules` structure. Drives the unified
Modules+Types navigation mode on the frontend.

Procedure:
1. Read `<project_root>/Project.toml` to find the package `name`.
2. Open `<project_root>/src/<name>.jl` (Julia package convention).
3. Recursively follow every `include("...")` call from that file.
4. For each file, run `collect_definitions` (the existing parser), then
   aggregate definitions into a module hierarchy: each module's children
   are types, functions, and submodules; functions whose name matches a
   sibling type's name are moved into that type's `:constructors` list.

Limitations of the v1:
- Constructors are detected by name-match only (inner/outer
  constructors merged); no signature collation yet.
- Per-function methods are still emitted as one entry per textual
  definition rather than grouped under a single function row; method
  collation is the next nav-system step.
- Parametric supertypes are returned as their full source text
  (`Bar{T}`); hierarchy grouping that strips parameters is the
  follow-up frontend step.

Wire shape (req has no fields):
```
res payload:
  project_root, package_name, entry_file,
  modules: [
    { name, file, line, ast_hash,
      types: [{ name, kind, file, line, ast_hash, constructors: [...],
                fields?: [{name, type, line}],   # struct only
                supertype?: "Bar" | "Bar{T}" }], # struct + abstract
      functions: [{ name, file, line, ast_hash }],
      submodules: [ ... same shape ... ] },
    ...
  ]
```
"""
function handle_project_scan(io::IO, state::KernelState, id, _payload)
    package_name = read_package_name(state.project_root)
    if isnothing(package_name)
        write_envelope(io, "res", id, "project.scan", Dict(
            :error => "no [name] in $(joinpath(state.project_root, "Project.toml"))",
            :code => "no_package",
        ))
        return
    end
    entry_path = joinpath(state.project_root, "src", "$(package_name).jl")
    if !isfile(entry_path)
        write_envelope(io, "res", id, "project.scan", Dict(
            :package_name => package_name,
            :entry_file => entry_path,
            :error => "missing entry file: $(entry_path)",
            :code => "no_entry",
        ))
        return
    end

    # Single context-carrying walk from the entry file: descend into module
    # bodies and follow include()s, attributing each def to the module it runs
    # in at runtime (see `scan_project_defs`). Group by physical file for the
    # tree builder, which keys `preview.get` off `:file`.
    defs = scan_project_defs(entry_path)
    file_defs = Dict{String, Vector{Dict}}()
    for d in defs
        push!(get!(() -> Dict[], file_defs, d[:file]), d)
    end

    modules = build_module_tree(file_defs, entry_path, package_name)

    write_envelope(io, "res", id, "project.scan", Dict(
        :project_root => state.project_root,
        :package_name => package_name,
        :entry_file => entry_path,
        :modules => modules,
    ))
end

"""
    handle_markdown_tokenize

Backend-side syntax tokenizer for fenced code blocks. Today only handles
Julia (`lang = "julia"`); other languages get an empty span list so the
frontend's tree-sitter fallback wins.

Wire shape (via `kernel.request` envelope):
```
req payload:  { lang: "julia", source: "..." }
res payload:  { lang: "julia", spans: [{ start, end, kind }] }
```

Uses `JuliaSyntax.parseall` instead of `tokenize` so we walk the parse
tree, not just the lexical token stream — this is the precision lift
over tree-sitter-julia, since the AST tells us about function definition
vs call-site, parameter names, field access, type annotations, etc.
that the lexer can only guess at with heuristics. Per Codex's industry-
standard recommendation, this is the "semantic layer" that overlays
the synchronous tree-sitter base on the frontend.

`start` and `end` are byte offsets in `source` (0-indexed, exclusive
end — matches Rust slice semantics). `kind` is a tree-sitter standard
capture name (`keyword`, `function.call`, `type`, `string`, etc.) so
the frontend can reuse `color_for_scope` without a separate mapping.

Tolerant on parse errors: walks whatever subtree did parse and skips
sections that didn't. Returns `nothing` from the inner walk for nodes
that don't map to a known kind; those bytes fall through to the
frontend's default-fg rendering.
"""
function handle_markdown_tokenize(io::IO, _state::KernelState, id, payload)
    lang = string(get(payload, "lang", ""))
    source = string(get(payload, "source", ""))
    if lang != "julia" && lang != "jl"
        write_envelope(io, "res", id, "markdown.tokenize", Dict(
            :lang => lang,
            :spans => Dict[],
        ))
        return
    end
    spans = try
        tokenize_julia_source(source)
    catch err
        @warn "markdown.tokenize Julia walk failed" exception = err
        Dict[]
    end
    write_envelope(io, "res", id, "markdown.tokenize", Dict(
        :lang => "julia",
        :spans => spans,
    ))
end

"""
    tokenize_julia_source(source) -> Vec<Dict>

Parse `source` with JuliaSyntax and walk the resulting syntax tree,
emitting `(start, end, kind)` spans in source order. Byte offsets are
0-indexed (Rust convention); `kind` is a tree-sitter standard capture
name (`keyword` / `function.call` / `function` / `type` / `string` /
`number` / `comment` / `variable.parameter`).

Implementation note: JuliaSyntax represents source as `GreenNode`s with
explicit trivia (whitespace, comments). We walk the `SyntaxNode` tree
which already excludes trivia for structure, but consult the underlying
green tree's spans for comment ranges.

Heuristic mapping from `JuliaSyntax.kind` symbols to tree-sitter
captures is deliberately conservative — emit only spans we're
confident about, leave anything ambiguous for the frontend's default
rendering. The point isn't to colour everything; it's to colour
things tree-sitter-julia can't (param names, field access, function
def vs call, etc.) where JuliaSyntax has unambiguous answers.
"""
function tokenize_julia_source(source::AbstractString)
    out = Dict[]
    tree = try
        JuliaSyntax.parseall(JuliaSyntax.SyntaxNode, source; filename = "<fence>",
                             ignore_warnings = true)
    catch
        return out
    end
    walk_for_tokens!(out, tree, source, false)
    sort!(out, by = d -> d[:start])
    return out
end

function walk_for_tokens!(out::Vector{Dict}, node, source::AbstractString,
                          in_def_head::Bool)
    # Backend's role here is purely the *semantic overlay* — what
    # tree-sitter-julia can't tell from a lexical walk. Tree-sitter
    # already handles keywords / strings / comments / numbers /
    # operators correctly; we don't re-emit those. We emit:
    #
    #   - function-definition names  → "function"
    #   - call-site names            → "function.call"
    #   - type annotation RHS        → "type"
    #   - supertype RHS              → "type"
    #
    # Each captures the IDENTIFIER's byte range, not the whole
    # composite expression. Children recurse so nested forms (e.g. a
    # call inside a function body, a type annotation inside a struct
    # field) also get coloured.
    k = JuliaSyntax.kind(node)
    if k == K"function"
        # Function-def head is the first child; if it's a K"call",
        # the function name is its first identifier.
        kids = JuliaSyntax.children(node)
        if !isnothing(kids) && !isempty(kids)
            head = kids[1]
            if JuliaSyntax.kind(head) == K"call"
                hkids = JuliaSyntax.children(head)
                if !isnothing(hkids) && !isempty(hkids) &&
                   JuliaSyntax.kind(hkids[1]) == K"Identifier"
                    nrng = JuliaSyntax.byte_range(hkids[1])
                    push!(out, Dict(
                        :start => first(nrng) - 1,
                        :end => last(nrng),
                        :kind => "function",
                    ))
                end
            end
        end
    elseif k == K"call" && !in_def_head
        kids = JuliaSyntax.children(node)
        if !isnothing(kids) && !isempty(kids) &&
           JuliaSyntax.kind(kids[1]) == K"Identifier"
            nrng = JuliaSyntax.byte_range(kids[1])
            push!(out, Dict(
                :start => first(nrng) - 1,
                :end => last(nrng),
                :kind => "function.call",
            ))
        end
    elseif k == K"::"
        kids = JuliaSyntax.children(node)
        if !isnothing(kids) && length(kids) >= 2
            type_node = kids[end]
            nrng = JuliaSyntax.byte_range(type_node)
            push!(out, Dict(
                :start => first(nrng) - 1,
                :end => last(nrng),
                :kind => "type",
            ))
        end
    elseif k == K"<:" || k == K">:"
        kids = JuliaSyntax.children(node)
        if !isnothing(kids) && length(kids) >= 2
            type_node = kids[end]
            nrng = JuliaSyntax.byte_range(type_node)
            push!(out, Dict(
                :start => first(nrng) - 1,
                :end => last(nrng),
                :kind => "type",
            ))
        end
    end
    kids = JuliaSyntax.children(node)
    if !isnothing(kids)
        # Children of K"function" — the head (first child) is a K"call"
        # that's the def-signature, not a real call. Recurse with
        # in_def_head=true so the inner K"call" walker skips emitting
        # `function.call`; the body still recurses normally so calls
        # inside it pick up `function.call`.
        for (i, c) in enumerate(kids)
            child_in_head = (k == K"function") && (i == 1)
            walk_for_tokens!(out, c, source, child_in_head)
        end
    end
end

"""
    read_package_name(project_root) -> String | nothing

Minimal Project.toml parser. Returns the value of the top-level
`name = "..."` key (the standard `[deps]`-free first section), or
`nothing` if the file doesn't exist or has no `name`. We don't pull in
TOML.jl here — Project.toml's package-name surface is consistently
quoted and lives before any `[section]` header, so a hand-roll keeps
the dep graph minimal.
"""
function read_package_name(project_root::AbstractString)
    p = joinpath(project_root, "Project.toml")
    isfile(p) || return nothing
    for raw in eachline(p)
        line = strip(split(raw, '#'; limit = 2)[1])
        isempty(line) && continue
        startswith(line, '[') && break  # entered a section; package name lives above
        if startswith(line, "name")
            kv = split(line, '='; limit = 2)
            length(kv) == 2 || continue
            val = strip(kv[2])
            val = strip(val, ['"', '\''])
            isempty(val) || return val
        end
    end
    return nothing
end

"""
    include_target(node) -> String | nothing

If `node` is an `include("literal")` call, return the literal path string;
otherwise `nothing`. Only the single-string-argument form is matched — the
static, statically-resolvable case the module nav cares about.
"""
function include_target(node)
    JuliaSyntax.kind(node) == K"call" || return nothing
    kids = JuliaSyntax.children(node)
    (isnothing(kids) || length(kids) != 2) && return nothing
    head, arg = kids[1], kids[2]
    (JuliaSyntax.kind(head) == K"Identifier" &&
     string(JuliaSyntax.sourcetext(head)) == "include" &&
     JuliaSyntax.kind(arg) == K"string") || return nothing
    return string_literal_text(arg)
end

"""
    scan_project_defs(entry_path) -> Vector{Dict}

Single recursive AST walk for `project.scan`. Starting at `entry_path`, it
descends into `module` bodies AND follows `include("…")` calls, carrying the
*enclosing module* name so a definition attributes to the module it runs in at
runtime — regardless of which physical file it sits in.

This replaces the old two-phase scan (flatten the include graph into a flat
file set → parse each file independently), which lost the enclosing-module
context: a file include()d inside `module Zernike` was parsed standalone, its
top-level defs got no `:parent`, and `build_module_tree` then defaulted them to
the package — so `Zernike` came up empty and its members listed flat under the
package. Carrying the module context across the include boundary fixes that.

Each entry has the `collect_definitions` shape (`:name`/`:kind`/`:line`/
`:ast_hash`, struct `:fields`/`:supertype`) plus a `:file` (the physical file)
and, for non-top-level defs, a `:parent` (the enclosing module name).
`build_module_tree` nests by `:parent`; it is correct as long as `:parent` is.
"""
function scan_project_defs(entry_path::AbstractString)
    defs = Dict[]
    visited = Set{String}()   # cycle guard: a file include-ing back into its includer
    scan_file_defs!(defs, abspath(entry_path), nothing, visited)
    return defs
end

# Parse one file and walk its top level in the scope of `current_module` — the
# module the `include` that pulled this file in was sitting in, or `nothing` for
# the entry file's true top level. `include` paths resolve relative to the
# current file (the standard Julia rule).
function scan_file_defs!(defs, path::AbstractString, current_module, visited::Set{String})
    abs_path = abspath(path)
    abs_path in visited && return
    isfile(abs_path) || return
    push!(visited, abs_path)
    src = try
        read(abs_path, String)
    catch
        return
    end
    tree = try
        JuliaSyntax.parseall(JuliaSyntax.SyntaxNode, src; filename = abs_path)
    catch
        return
    end
    scan_block_defs!(defs, tree, current_module, dirname(abs_path), abs_path, visited)
end

# Walk the statements of `node` (a file root or a module/begin block),
# attributing each definition to `current_module`. Recurses into nested
# `module` bodies (with that module as the new scope) and follows `include`
# calls in the current scope; `base_dir`/`file` track the current file for
# include resolution and `:file` stamping.
function scan_block_defs!(defs, node, current_module, base_dir::AbstractString,
                          file::AbstractString, visited::Set{String})
    children = JuliaSyntax.children(node)
    isnothing(children) && return
    for child in children
        inc = include_target(child)
        if !isnothing(inc)
            scan_file_defs!(defs, abspath(joinpath(base_dir, inc)), current_module, visited)
            continue
        end
        entry = def_entry(child, JuliaSyntax.kind(child), current_module)
        if !isnothing(entry)
            entry[:file] = file
            push!(defs, entry)
            if entry[:kind] == "module"
                # Descend into the module body with this module as the new
                # enclosing scope — to any depth, across include()s.
                # `module_body_node` unwraps a docstring wrapper so a documented
                # `module M … end` (K"doc"[string, module]) is descended too.
                mnode = module_body_node(child, JuliaSyntax.kind(child))
                if !isnothing(mnode)
                    for mk in JuliaSyntax.children(mnode)
                        if JuliaSyntax.kind(mk) == K"block"
                            scan_block_defs!(defs, mk, entry[:name], base_dir, file, visited)
                        end
                    end
                end
            end
            continue
        end
        # Scope-transparent grouping (`begin … end`, parse toplevel): descend
        # with the SAME module so grouped includes/defs still attribute right.
        # (Conditional includes — `@static if … include … end` — are not
        # followed; a documented limitation, rare in package entry points.)
        k = JuliaSyntax.kind(child)
        if k == K"block" || k == K"toplevel"
            scan_block_defs!(defs, child, current_module, base_dir, file, visited)
        end
    end
end

function string_literal_text(node)
    kids = JuliaSyntax.children(node)
    isnothing(kids) && return nothing
    isempty(kids) && return nothing
    for c in kids
        if JuliaSyntax.kind(c) == K"String"
            return string(JuliaSyntax.sourcetext(c))
        end
    end
    return nothing
end

"""
    build_module_tree(file_defs, entry_path) -> Vector{Dict}

Aggregate per-file definitions into the nested `modules` shape the
frontend renders. Each entry in `file_defs[file]` carries `:kind` and
optional `:parent` (the enclosing module's name, set by
`collect_definitions` when recursing into a module block). Modules
themselves appear with `:kind == "module"` and no `:parent` for
top-level modules.

Today every entry's `:parent` field is the *immediate* enclosing
module name (or nothing for top-level modules in the entry file).
Multiple module names with the same string across files are *merged*
into a single module entry — sufficient while the v1 doesn't handle
collisions (a project that re-uses a name across unrelated submodules
is pathological). Future revisions can disambiguate by parent chain.
"""
function build_module_tree(file_defs::Dict{String, Vector{Dict}},
                           entry_path::AbstractString,
                           package_name::AbstractString)
    # Flat list of (entry, file) tuples for easier indexing.
    all_entries = Tuple{Dict, String}[]
    for (f, defs) in file_defs
        for d in defs
            push!(all_entries, (d, f))
        end
    end

    # Top-level entries in `include`d files have no `:parent` — they
    # run in the *includer*'s module scope at runtime, not at file
    # scope. Default them to the package module so `src/inner.jl`'s
    # `struct Inner` lands under `MyPkg`. Skip the package's own
    # module declaration (it really is top-level).
    for (d, f) in all_entries
        haskey(d, :parent) && continue
        if d[:kind] == "module" && d[:name] == package_name
            continue
        end
        d[:parent] = package_name
    end

    # Collect every module declaration as a candidate node. Top-level
    # modules are those with no :parent; submodules have :parent pointing
    # to their enclosing module's name.
    module_decls = Dict{String, Dict}()
    for (d, f) in all_entries
        if d[:kind] == "module"
            name = d[:name]
            # First-wins on collision so the entry file's module beats
            # an accidental duplicate in an included file.
            if !haskey(module_decls, name)
                module_decls[name] = Dict(
                    :name => name,
                    :file => f,
                    :line => d[:line],
                    :ast_hash => d[:ast_hash],
                    :types => Dict[],
                    :functions => Dict[],
                    :submodules => Vector{Dict}(),
                )
            end
        end
    end

    # Drop non-module entries into the correct module bucket.
    for (d, f) in all_entries
        d[:kind] == "module" && continue
        parent = get(d, :parent, nothing)
        isnothing(parent) && continue
        mod = get(module_decls, parent, nothing)
        isnothing(mod) && continue
        entry_dict = Dict(
            :name => d[:name],
            :kind => d[:kind],
            :file => f,
            :line => d[:line],
            :ast_hash => d[:ast_hash],
        )
        if d[:kind] == "struct" || d[:kind] == "abstract"
            entry_dict[:constructors] = Dict[]
            if haskey(d, :fields)
                entry_dict[:fields] = d[:fields]
            end
            if haskey(d, :supertype)
                entry_dict[:supertype] = d[:supertype]
            end
            push!(mod[:types], entry_dict)
        else
            push!(mod[:functions], entry_dict)
        end
    end

    # Move constructors (functions whose name matches a sibling type's
    # name in the same module) from :functions into the type's
    # :constructors list. Outer + inner constructors land here uniformly
    # because both are exposed as functions named `Foo` in the same
    # module's definition set.
    for (_, mod) in module_decls
        type_names = Set(t[:name] for t in mod[:types])
        kept_fns = Dict[]
        for fn in mod[:functions]
            if fn[:name] in type_names
                # Find the matching type and push.
                for t in mod[:types]
                    if t[:name] == fn[:name]
                        push!(t[:constructors], fn)
                        break
                    end
                end
            else
                push!(kept_fns, fn)
            end
        end
        mod[:functions] = kept_fns
        # Sort siblings alphabetically for predictable display.
        sort!(mod[:types]; by = t -> t[:name])
        sort!(mod[:functions]; by = f -> f[:name])
    end

    # Nest submodules under their parent. We do this *after* the flat
    # population so a submodule's own types/functions are already
    # attached to its node when we move it into its parent's :submodules.
    nested_names = Set{String}()
    for (name, mod) in module_decls
        # Find the module's own parent (look up its entry in all_entries).
        parent = nothing
        for (d, _) in all_entries
            if d[:kind] == "module" && d[:name] == name
                parent = get(d, :parent, nothing)
                break
            end
        end
        if !isnothing(parent) && haskey(module_decls, parent)
            push!(module_decls[parent][:submodules], mod)
            push!(nested_names, name)
        end
    end

    # Top-level modules = anything not nested under another.
    top = Dict[]
    for (name, mod) in module_decls
        name in nested_names && continue
        push!(top, mod)
    end
    sort!(top; by = m -> m[:name])
    top
end

# ---- wire helpers ----

function write_envelope(io::IO, kind, id, op, payload)
    env = Dict(:v => PROTOCOL_VERSION, :id => id, :kind => kind, :op => op, :payload => payload)
    JSON3.write(io, env)
    write(io, '\n')
    flush(io)
end

end # module
