# The record of Julia's copy of a tool, `copy.txt` in the tool's folder: lines `key=value`, the library path variable
# (`libpath_env`) and its value (`libpath`), then each program's file. Included by ConceptExplorerCore, which reads it
# for a preview, and by fetch_tool.jl, which checks what it wrote with this same function, so a copy the preview would
# refuse is a failure of the fetch and never a refetch at every preview.

const COPY_PATHSEP = Sys.iswindows() ? ';' : ':'

# The record's entries as a `Dict`, if it names an existing file for every program and an existing folder for every
# entry of its library path (a deleted artifact, or the library folder of a Julia since replaced); else `nothing`.
function read_copy(dir, programs)
    try
        pairs = (split(l, "="; limit = 2) for l in eachline(joinpath(dir, "copy.txt")) if occursin('=', l))
        kv = Dict(String(k) => String(v) for (k, v) in pairs)
        all(p -> isfile(get(kv, p, "")), programs) || return nothing
        all(isdir, filter(!isempty, split(kv["libpath"], COPY_PATHSEP))) || return nothing
        haskey(kv, "libpath_env") ? kv : nothing
    catch
        nothing
    end
end
