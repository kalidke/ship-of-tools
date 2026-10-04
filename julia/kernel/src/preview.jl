# Preview op: file.preview, with lazy loading of built-in plugins that are not loaded at startup.

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
