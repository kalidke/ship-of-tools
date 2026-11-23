module ShipToolsPDFFile

# Built-in plugin for PDF files (ADR 0021).
#
# The preview pane shows one rasterized page at a time; `n`/`p` in the
# frontend re-request `preview.get` with a `page` param that arrives here
# via the 3-arg `preview` form. Rasterization shells out to poppler
# (`pdftoppm` for the page bitmap, `pdfinfo` for the page count) on the
# host where the file lives — same backend-side-decode model as the
# ffmpeg poster path (ADR 0018). The tools are the computer's own where its
# search path has both, else Julia's copy (`Poppler_jll`), fetched at the first
# preview that needs it (`ConceptExplorerCore.find_tools`).

using ConceptExplorerCore

export PDFFile

"""
    PDFFile <: FileType

Built-in plugin for `.pdf`. `preview` returns one page as `image/png` with
`extras = {"page", "page_count"}` so the frontend can drive page turns. With
`params["words"] === true` it returns that page's words and their boxes instead
(`WORDS_MIME`), and draws nothing.
"""
struct PDFFile <: ConceptExplorerCore.FileType end

# 144 DPI: a US-letter page ≈ 1224×1584 px — crisp at fit-to-pane and
# moderate zoom on the frontend's nearest-sampler quad, far under GPU
# texture caps. A `dpi` param can ride the same params channel later
# (zoom-triggered re-rasterize, deferred in ADR 0021).
const RASTER_DPI = 144

# The two programs are one build, so Julia's copy holds both.
const POPPLER = ConceptExplorerCore.ToolSet("poppler", "Poppler_jll", "9c32591e-4766-534b-9725-b71a8799265b", "25",
                                            ["pdftoppm", "pdfinfo"], "160 MB")

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
    # A request for the page's words draws nothing; every other request draws the page as before.
    get(params, "words", false) === true && return _page_words(path, max(page, 1))
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
                       bound::Real=ConceptExplorerCore.TOOL_WAIT)
    tools = ConceptExplorerCore.find_tools(POPPLER)
    tools isa AbstractString && return _note(tools)
    deadline = time() + bound
    left() = max(deadline - time(), 0.1)
    path = abspath(path)
    modified = mtime(path)
    cached_info = get(PDF_INFO, path, nothing)
    info = if cached_info !== nothing && first(cached_info) == modified
        last(cached_info)
    else
        read_info = try
            _pdf_info(tools["pdfinfo"], path, left())
        catch e
            # A tool that ran out of time says nothing about the file, so it is not remembered.
            return _note("Couldn't read this file's page count: $(ConceptExplorerCore.error_words(e))")
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
            ConceptExplorerCore.read_within(`$(tools["pdftoppm"]) -png $scale_args -f $p -l $p -singlefile $path`, left())
        catch e
            return _note("Couldn't rasterize page $p: $(ConceptExplorerCore.error_words(e))")
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
    _pdf_info(pdfinfo, path, bound) -> Union{Tuple{Int,Float64,Float64}, Nothing}

`(page_count, page_w_pts, page_h_pts)` from one `pdfinfo` run, or `nothing`
when the tool fails or the Pages field is absent (corrupt/encrypted file).
A run that outlasts `bound` seconds throws.
Page size falls back to 0×0 when unparsable — callers treat that as "no
geometry" and use the fixed-DPI path.
"""
function _pdf_info(pdfinfo::Cmd, path::AbstractString, bound::Real)
    out = try
        String(ConceptExplorerCore.read_within(`$pdfinfo $path`, bound))
    catch e
        e isa ConceptExplorerCore.ToolFailure && e.exited && return nothing
        rethrow()
    end
    pages = match(r"^Pages:\s+(\d+)"m, out)
    pages === nothing && return nothing
    size = match(r"^Page size:\s+([\d.]+)\s+x\s+([\d.]+)\s+pts"m, out)
    w = size === nothing ? 0.0 : something(tryparse(Float64, size.captures[1]), 0.0)
    h = size === nothing ? 0.0 : something(tryparse(Float64, size.captures[2]), 0.0)
    (parse(Int, pages.captures[1]), w, h)
end

# A page's words, for selecting text on the picture of it: asked for with `params["words"] === true`, answered as one
# JSON document, `{"page":P,"cut":false,"lines":[[[x0,y0,x1,y1,"text"],...],...]}`. A line is one line of
# `pdftotext -bbox-layout`, in the reading order that tool guesses; a box is the word's rectangle as fractions of the
# page as `pdftoppm` draws it, each times 65535 and rounded (0 is the left or the top edge), clamped to the page.
const WORDS_MIME = "application/vnd.sot.page-words+json"
# The most bytes of the document: the words are cut before the one that would pass it, and `cut` says so.
const MAX_WORDS_BYTES = 512 * 1024
const WORD_OR_LINE = r"<line[ >]|<word xMin=\"([-\d.]+)\" yMin=\"([-\d.]+)\" xMax=\"([-\d.]+)\" yMax=\"([-\d.]+)\">([^<]*)</word>"

function _page_words(path::AbstractString, page::Int;
                     bound::Real=ConceptExplorerCore.TOOL_WAIT, limit::Int=MAX_WORDS_BYTES)
    tools = ConceptExplorerCore.find_tools(POPPLER)
    tools isa AbstractString && return _note(tools)
    # The text tool is the file beside `pdftoppm` (one build holds both, on a computer and in Julia's copy), run with
    # its environment. The tool set is not grown by it: a copy fetched before would then count as no copy.
    render = tools["pdftoppm"].exec[1]
    file = joinpath(dirname(render), replace(basename(render), "pdftoppm" => "pdftotext"))
    isfile(file) || return _note("This computer's poppler has no `pdftotext`, so the text of page $page cannot be read.")
    pdftotext = Cmd(Cmd([file]); env = tools["pdftoppm"].env)
    deadline = time() + bound
    left() = max(deadline - time(), 0.1)
    path = abspath(path)
    doc = try
        # The tool gives a page turned a quarter its boxes in the turned page's space and its size unturned.
        info = String(ConceptExplorerCore.read_within(`$(tools["pdfinfo"]) -f $page -l $page $path`, left()))
        turn = match(r"^Page\s+\d+\s+rot:\s+(\d+)"m, info)
        turned = turn !== nothing && turn.captures[1] in ("90", "270")
        xhtml = String(ConceptExplorerCore.read_within(`$pdftotext -bbox-layout -f $page -l $page $path -`, left()))
        _words_document(xhtml, page, turned, limit)
    catch e
        return _note("Couldn't read the text of page $page: $(ConceptExplorerCore.error_words(e))")
    end
    doc === nothing && return _note("Couldn't read the text of page $page: the answer of `pdftotext` was not understood.")
    ConceptExplorerCore.PreviewPayload(WORDS_MIME, Vector{UInt8}(codeunits(doc)))
end

# The document for one page of `pdftotext -bbox-layout`'s output, or `nothing` when it names no page size. A word's
# text has the tool's five entities decoded and every control character, U+2028 and U+2029 dropped, so no word breaks
# a line or carries an escape; a word left empty and a line left empty are left out.
function _words_document(xhtml::AbstractString, page::Int, turned::Bool, limit::Int)
    isvalid(xhtml) || (xhtml = filter(isvalid, xhtml))   # bytes that are no character are dropped before anything is matched
    size = match(r"<page width=\"([\d.]+)\" height=\"([\d.]+)\"", xhtml)
    size === nothing && return nothing
    w, h = something.(tryparse.(Float64, size.captures), 0.0)
    turned && ((w, h) = (h, w))
    (w > 0 && h > 0) || return nothing
    at(v, of) = round(Int, clamp(something(tryparse(Float64, v), 0.0) / of, 0, 1) * 65535)
    lines, line, cut = String[], String[], false
    used = sizeof("{\"page\":$page,\"cut\":false,\"lines\":[]}")
    for m in eachmatch(WORD_OR_LINE, xhtml)
        if m.captures[1] === nothing   # a line begins
            isempty(line) || push!(lines, "[" * join(line, ",") * "]")
            line = String[]
            continue
        end
        text = replace(m.captures[5], "&amp;" => "&", "&lt;" => "<", "&gt;" => ">", "&quot;" => "\"", "&apos;" => "'")
        text = filter(c -> !iscntrl(c) && c != '\u2028' && c != '\u2029', text)
        isempty(text) && continue
        x0, x1 = minmax(at(m.captures[1], w), at(m.captures[3], w))
        y0, y1 = minmax(at(m.captures[2], h), at(m.captures[4], h))
        word = "[$x0,$y0,$x1,$y1,\"" * replace(text, "\\" => "\\\\", "\"" => "\\\"") * "\"]"
        # A word after another costs a comma; a line's first costs the line's brackets, and a comma after another line.
        cost = sizeof(word) + (isempty(line) ? 2 + !isempty(lines) : 1)
        if used + cost > limit
            cut = true
            break
        end
        used += cost
        push!(line, word)
    end
    isempty(line) || push!(lines, "[" * join(line, ",") * "]")
    "{\"page\":$page,\"cut\":$cut,\"lines\":[" * join(lines, ",") * "]}"
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
