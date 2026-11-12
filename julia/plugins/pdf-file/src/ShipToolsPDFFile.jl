module ShipToolsPDFFile

# Built-in plugin for PDF files (ADR 0021).
#
# The preview pane shows one rasterized page at a time; `n`/`p` in the
# frontend re-request `preview.get` with a `page` param that arrives here
# via the 3-arg `preview` form. Rasterization shells out to poppler
# (`pdftoppm` for the page bitmap, `pdfinfo` for the page count) on the
# host where the file lives — same backend-side-decode model as the
# ffmpeg poster path (ADR 0018). Both tools are `Poppler_jll`'s own build,
# run through its command form; nothing is looked up on the search path.

using ConceptExplorerCore
using Poppler_jll

export PDFFile

"""
    PDFFile <: FileType

Built-in plugin for `.pdf`. `preview` returns one page as `image/png` with
`extras = {"page", "page_count"}` so the frontend can drive page turns.
"""
struct PDFFile <: ConceptExplorerCore.FileType end

# 144 DPI: a US-letter page ≈ 1224×1584 px — crisp at fit-to-pane and
# moderate zoom on the frontend's nearest-sampler quad, far under GPU
# texture caps. A `dpi` param can ride the same params channel later
# (zoom-triggered re-rasterize, deferred in ADR 0021).
const RASTER_DPI = 144

# The longest one page request may spend in pdfinfo and pdftoppm together, in seconds; the kernel serves one request at
# a time, so a stuck tool must end, and the daemon gives up on a kernel answer at 10 s (`KERNEL_REQUEST_TIMEOUT`).
const PREVIEW_BOUND = 8

# The tools as the JLL's command form (a `Cmd` with its library path set). A function, so a platform with no ready-built
# poppler still loads this module; a test stands other commands in here.
const PDFTOPPM = Ref{Function}(() -> Poppler_jll.pdftoppm())
const PDFINFO = Ref{Function}(() -> Poppler_jll.pdfinfo())

ConceptExplorerCore.matches(::Type{PDFFile}, path::AbstractString) =
    endswith(lowercase(path), ".pdf")

# 2-arg form (plain `preview.get`, no params): page 1.
ConceptExplorerCore.preview(::Type{PDFFile}, path::AbstractString) =
    _preview_page(path, 1)

# 3-arg form: honor a `page` param (1-based; clamped to the document) and an
# optional `fit_w`/`fit_h` pixel hint (the frontend's preview-pane size) so
# the page rasterizes at exactly the displayed resolution — the GPU then
# samples ~1:1 and text stays crisp instead of aliasing through a resample.
function ConceptExplorerCore.preview(::Type{PDFFile}, path::AbstractString,
                                     params::AbstractDict)
    page = try
        Int(get(params, "page", 1))
    catch
        1
    end
    asint(x) = try
        v = Int(x)
        v > 0 ? v : nothing
    catch
        nothing
    end
    fit_w = asint(get(params, "fit_w", nothing))
    fit_h = asint(get(params, "fit_h", nothing))
    _preview_page(path, max(page, 1); fit_w, fit_h)
end

# Safety ceiling on the rasterized long side: a maximised 4K pane at 2×
# zoom-rerasterize (future) stays under GPU texture caps and wire bloat.
const MAX_RASTER_PX = 4096

# The kernel serves one request at a time, so these caches need no lock or background task.
const PAGE_CACHE_LIMIT = 64 * 1024^2
const PageKey = Tuple{String,Float64,Int,Tuple}
const PAGE_CACHE = Dict{PageKey,Vector{UInt8}}()
const PAGE_ORDER = PageKey[] # Least recently used first.
const PAGE_BYTES = Ref(0)
const PDF_INFO = Dict{String,Tuple{Float64,Union{Nothing,Tuple{Int,Float64,Float64}}}}()

function _cached_page(key::PageKey)
    png = get(PAGE_CACHE, key, nothing)
    png === nothing && return nothing
    deleteat!(PAGE_ORDER, findfirst(==(key), PAGE_ORDER))
    push!(PAGE_ORDER, key)
    png
end

function _cache_page(key::PageKey, png::Vector{UInt8})
    length(png) > PAGE_CACHE_LIMIT && return
    while PAGE_BYTES[] + length(png) > PAGE_CACHE_LIMIT
        oldest = popfirst!(PAGE_ORDER)
        PAGE_BYTES[] -= length(pop!(PAGE_CACHE, oldest))
    end
    PAGE_CACHE[key] = png
    push!(PAGE_ORDER, key)
    PAGE_BYTES[] += length(png)
end

function _preview_page(path::AbstractString, page::Int;
                       fit_w::Union{Int,Nothing}=nothing,
                       fit_h::Union{Int,Nothing}=nothing,
                       bound::Real=PREVIEW_BOUND)
    if !Poppler_jll.is_available()
        return _note("no ready-built poppler exists for this platform.")
    end
    deadline = time() + bound
    left() = max(deadline - time(), 0.1)
    path = abspath(path)
    modified = mtime(path)
    cached_info = get(PDF_INFO, path, nothing)
    info = if cached_info !== nothing && first(cached_info) == modified
        last(cached_info)
    else
        read_info = try
            _pdf_info(path, left())
        catch e
            # A tool that ran out of time says nothing about the file, so it is not remembered.
            return _note("Couldn't read this file's page count: $(sprint(showerror, e))")
        end
        PDF_INFO[path] = (modified, read_info)
        read_info
    end
    if info === nothing
        return _note("`pdfinfo` couldn't read this file — it may be corrupt or encrypted.")
    end
    page_count, page_w_pts, page_h_pts = info
    p = clamp(page, 1, page_count)
    # Letterbox-fit sizing: scale the page to the pane like the frontend
    # letterboxes the texture — bound by whichever pane edge the page hits
    # first. pdftoppm takes one bounded axis (`-scale-to-x`/`-scale-to-y`,
    # the other -1 = keep aspect); pick the axis from the aspect comparison.
    # No fit hint (old frontend, capture harnesses) → fixed-DPI fallback.
    scale_args = if fit_w !== nothing && fit_h !== nothing &&
                    page_w_pts > 0 && page_h_pts > 0
        height_bound = fit_w / fit_h > page_w_pts / page_h_pts
        if height_bound
            `-scale-to-y $(min(fit_h, MAX_RASTER_PX)) -scale-to-x -1`
        else
            `-scale-to-x $(min(fit_w, MAX_RASTER_PX)) -scale-to-y -1`
        end
    else
        `-r $RASTER_DPI`
    end
    key = (path, modified, p, Tuple(scale_args))
    png = _cached_page(key)
    if png === nothing
        png = try
            # -f/-l bound rasterization to the one page. Stdout streaming is
            # poppler-quirky (caught on first deploy): `-singlefile` with NO
            # output-root argument writes the PNG to stdout; a trailing `-`
            # root yields 0 bytes silently, and without `-singlefile` the `-`
            # is treated as a filename prefix (`./--1.png` in the cwd). `-png`
            # keeps the frontend's existing image path.
            ConceptExplorerCore.read_within(`$(PDFTOPPM[]()) -png $scale_args -f $p -l $p -singlefile $path`, left())
        catch e
            return _note("Couldn't rasterize page $p: $(sprint(showerror, e))")
        end
        isempty(png) && return _note("Rasterizing page $p produced no output.")
        _cache_page(key, png)
    end
    ConceptExplorerCore.PreviewPayload(
        "image/png", png,
        Dict{String,Any}("page" => p, "page_count" => page_count),
    )
end

"""
    _pdf_info(path, bound) -> Union{Tuple{Int,Float64,Float64}, Nothing}

`(page_count, page_w_pts, page_h_pts)` from one `pdfinfo` run, or `nothing`
when the tool fails or the Pages field is absent (corrupt/encrypted file).
A run that outlasts `bound` seconds throws.
Page size falls back to 0×0 when unparsable — callers treat that as "no
geometry" and use the fixed-DPI path.
"""
function _pdf_info(path::AbstractString, bound::Real)
    out = try
        String(ConceptExplorerCore.read_within(`$(PDFINFO[]()) $path`, bound))
    catch e
        e isa ProcessFailedException && return nothing
        rethrow()
    end
    pages = match(r"^Pages:\s+(\d+)"m, out)
    pages === nothing && return nothing
    size = match(r"^Page size:\s+([\d.]+)\s+x\s+([\d.]+)\s+pts"m, out)
    w = size === nothing ? 0.0 : something(tryparse(Float64, size.captures[1]), 0.0)
    h = size === nothing ? 0.0 : something(tryparse(Float64, size.captures[2]), 0.0)
    (parse(Int, pages.captures[1]), w, h)
end

_note(msg) = ConceptExplorerCore.PreviewPayload(
    "text/markdown",
    Vector{UInt8}(codeunits("# pdf\n\n$msg\n")),
)

"""
    parse_entities(::Type{PDFFile}, path) -> Vector{ConceptEntity}

Phase-1 stub — PDFs contribute no concept entities.
"""
ConceptExplorerCore.parse_entities(::Type{PDFFile}, path::AbstractString) =
    ConceptExplorerCore.ConceptEntity[]

end # module
