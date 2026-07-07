module ShipToolsPDFFile

# Built-in plugin for PDF files (ADR 0021).
#
# The preview pane shows one rasterized page at a time; `n`/`p` in the
# frontend re-request `preview.get` with a `page` param that arrives here
# via the 3-arg `preview` form. Rasterization shells out to poppler
# (`pdftoppm` for the page bitmap, `pdfinfo` for the page count) on the
# host where the file lives — same backend-side-decode model as the
# ffmpeg poster path (ADR 0018), and poppler is already present on the
# backend hosts.

using ConceptExplorerCore

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

function _preview_page(path::AbstractString, page::Int;
                       fit_w::Union{Int,Nothing}=nothing,
                       fit_h::Union{Int,Nothing}=nothing)
    pdftoppm = Sys.which("pdftoppm")
    pdfinfo = Sys.which("pdfinfo")
    if pdftoppm === nothing || pdfinfo === nothing
        # No quiet fallback: name the gap and the fix.
        return _note("poppler (`pdftoppm`/`pdfinfo`) not found on the kernel host — install poppler-utils to render PDF pages.")
    end
    info = _pdf_info(pdfinfo, path)
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
    png = try
        # -f/-l bound rasterization to the one page. Stdout streaming is
        # poppler-quirky (caught on first deploy): `-singlefile` with NO
        # output-root argument writes the PNG to stdout; a trailing `-`
        # root yields 0 bytes silently, and without `-singlefile` the `-`
        # is treated as a filename prefix (`./--1.png` in the cwd). `-png`
        # keeps the frontend's existing image path.
        read(`$pdftoppm -png $scale_args -f $p -l $p -singlefile $path`)
    catch e
        return _note("Couldn't rasterize page $p: $(sprint(showerror, e))")
    end
    isempty(png) && return _note("Rasterizing page $p produced no output.")
    ConceptExplorerCore.PreviewPayload(
        "image/png", png,
        Dict{String,Any}("page" => p, "page_count" => page_count),
    )
end

"""
    _pdf_info(pdfinfo, path) -> Union{Tuple{Int,Float64,Float64}, Nothing}

`(page_count, page_w_pts, page_h_pts)` from one `pdfinfo` run, or `nothing`
when the tool fails or the Pages field is absent (corrupt/encrypted file).
Page size falls back to 0×0 when unparsable — callers treat that as "no
geometry" and use the fixed-DPI path.
"""
function _pdf_info(pdfinfo::AbstractString, path::AbstractString)
    out = try
        read(`$pdfinfo $path`, String)
    catch
        return nothing
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
