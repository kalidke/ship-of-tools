module HDF5Preview

# Real `.h5` / `.hdf5` preview for Ship of Tools. Per the design locked with the
# user (bus 2026-05-28): HDF5.jl, metadata-only, lazy-loaded.
#
# - **HDF5.jl, not a CLI shell-out:** `h5ls`/`h5dump`/`h5py` are not present on
#   every backend host and there's no mature pure-Julia reader, so a shell-out
#   would be a fresh-box-breaks failure mode. `HDF5_jll` is self-contained and
#   cross-platform — the only no-system-install path.
# - **Lazy:** the kernel does NOT eagerly `using HDF5Preview`; it's loaded
#   on-demand (auto-load on first `.h5` preview, or `plugins.load`), so
#   `using HDF5` here pulls `HDF5_jll` only when a user actually opens an HDF5
#   file — never at kernel startup.
# - **Metadata-only:** we walk groups/datasets and report name · shape · eltype
#   (+ chunking + attributes) but NEVER read dataset contents, so the preview is
#   instant regardless of file size.

using ConceptExplorerCore
using HDF5

export HDF5File

"""
    HDF5File <: FileType

Ship of Tools plugin for `.h5` / `.hdf5` / `.hdf` files — adds a `FileType` subtype
and method extensions picked up via `ConceptExplorerCore.file_types()` once the
plugin loads. No kernel/backend changes needed (the dispatch wiring is the ABI).
"""
struct HDF5File <: ConceptExplorerCore.FileType end

const HDF5_EXTENSIONS = (".h5", ".hdf5", ".hdf")

ConceptExplorerCore.matches(::Type{HDF5File}, path::AbstractString) =
    any(endswith(lowercase(path), ext) for ext in HDF5_EXTENSIONS)

# Output guardrails. Cap the tree so a pathological file (millions of objects)
# can't produce a multi-MB preview; surface what was dropped rather than
# silently truncating (per the project's no-quiet-fallbacks rule).
const MAX_LINES = 500
const MAX_ATTR_VALUE_CHARS = 60

mutable struct Walk
    lines::Vector{String}
    groups::Int
    datasets::Int
    omitted::Int
end
Walk() = Walk(String[], 0, 0, 0)

# Append a line unless we've hit the cap; past the cap, count it as omitted so
# the footer can report "+N more".
function emit!(w::Walk, s::AbstractString)
    if length(w.lines) < MAX_LINES
        push!(w.lines, s)
    else
        w.omitted += 1
    end
    nothing
end

# `name  {100×200}  Float64  chunk {32×32}` — all from metadata, no data read.
function dataset_label(name::AbstractString, d::HDF5.Dataset)
    shape = try
        dims = size(d)
        isempty(dims) ? "scalar" : join(string.(dims), "×")
    catch
        "?"
    end
    et = try
        string(eltype(d))
    catch
        "?"
    end
    chunk = ""
    try
        c = HDF5.get_chunk(d)
        if c !== nothing && !isempty(c)
            chunk = "  chunk {" * join(string.(c), "×") * "}"
        end
    catch
        # not chunked (contiguous) or API mismatch — omit silently, it's optional
    end
    "$name  {$shape}  $et$chunk"
end

# Render an object's attributes at `at_prefix`, marked `@`. Values shown only
# for small scalars/strings (never reads large attribute arrays).
function attr_lines!(w::Walk, obj, at_prefix::AbstractString)
    names = try
        sort!(collect(keys(HDF5.attributes(obj))))
    catch
        return
    end
    for an in names
        val = ""
        try
            r = read(HDF5.attributes(obj)[an])
            if r isa AbstractString || r isa Number || r isa Bool
                s = string(r)
                s = length(s) > MAX_ATTR_VALUE_CHARS ? s[1:MAX_ATTR_VALUE_CHARS] * "…" : s
                val = " = " * s
            end
        catch
            # unreadable / non-scalar attribute → name only
        end
        emit!(w, at_prefix * "@" * an * val)
    end
end

# Depth-first walk. `prefix` is the accumulated tree-drawing prefix for THIS
# group's children; attributes and child connectors share it.
function walk_group!(w::Walk, g, prefix::AbstractString)
    attr_lines!(w, g, prefix)
    ks = try
        sort!(collect(keys(g)))
    catch
        String[]
    end
    for (i, k) in enumerate(ks)
        last = i == length(ks)
        connector = last ? "└─ " : "├─ "
        childprefix = prefix * (last ? "   " : "│  ")
        child = try
            g[k]
        catch e
            emit!(w, prefix * connector * k * "  (open failed: " * sprint(showerror, e) * ")")
            continue
        end
        try
            if child isa HDF5.Dataset
                w.datasets += 1
                emit!(w, prefix * connector * dataset_label(k, child))
                attr_lines!(w, child, childprefix)
            elseif child isa HDF5.Group
                w.groups += 1
                emit!(w, prefix * connector * k * "/")
                walk_group!(w, child, childprefix)
            else
                emit!(w, prefix * connector * k * "  (" * string(nameof(typeof(child))) * ")")
            end
        finally
            close(child)
        end
    end
    nothing
end

"""
    preview(::Type{HDF5File}, path) -> PreviewPayload

Metadata-only HDF5 tree. Returns `text/markdown`: a header (path · size ·
group/dataset counts) plus a fenced tree of groups, datasets (name · shape ·
eltype · chunk) and attributes. Never reads dataset contents, so it's instant
on huge files. Output capped at `MAX_LINES` with an explicit "+N more" footer.
On open/parse failure returns a `text/plain` error (no panic, no faked success).
"""
function ConceptExplorerCore.preview(::Type{HDF5File}, path::AbstractString)
    sz = try
        filesize(path)
    catch
        -1
    end
    file = nothing
    try
        file = HDF5.h5open(path, "r")
    catch e
        msg = "HDF5 open failed for $(basename(path)):\n\n$(sprint(showerror, e))"
        return ConceptExplorerCore.PreviewPayload("text/plain", Vector{UInt8}(msg))
    end
    try
        w = Walk()
        emit!(w, basename(path) * "/")
        walk_group!(w, file, "")
        szstr = sz < 0 ? "?" : Base.format_bytes(sz)
        header = "# HDF5 · $(basename(path))\n\n" *
                 "`$(path)` · $(szstr) · $(w.groups) groups · $(w.datasets) datasets\n\n"
        footer = w.omitted > 0 ?
            "\n… +$(w.omitted) more objects (capped at $(MAX_LINES) lines)\n" : ""
        body = header * "```text\n" * join(w.lines, "\n") * "\n" * footer * "```\n"
        return ConceptExplorerCore.PreviewPayload("text/markdown", Vector{UInt8}(body))
    catch e
        msg = "HDF5 read failed for $(basename(path)):\n\n$(sprint(showerror, e))"
        return ConceptExplorerCore.PreviewPayload("text/plain", Vector{UInt8}(msg))
    finally
        file !== nothing && close(file)
    end
end

"""
    parse_entities(::Type{HDF5File}, path) -> Vector{ConceptEntity}

No concept entities for HDF5 files (phase-1). A future revision could yield one
entity per group/dataset for annotation in concept nav.
"""
ConceptExplorerCore.parse_entities(::Type{HDF5File}, path::AbstractString) =
    ConceptExplorerCore.ConceptEntity[]

end # module
