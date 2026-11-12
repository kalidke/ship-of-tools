using ShipToolsPDFFile
using ConceptExplorerCore
using Test

const P = ShipToolsPDFFile.PDFFile
const PNG_MAGIC = UInt8[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
const PATH_SEP = Sys.iswindows() ? ";" : ":"

# A valid PDF of `npages` blank US-letter pages, so the real-tool tests need no ghostscript.
function write_pdf(path, npages)
    objs = ["<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [$(join(("$(2 + i) 0 R" for i in 1:npages), " "))] /Count $npages >>"]
    append!(objs, fill("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>", npages))
    io = IOBuffer()
    print(io, "%PDF-1.4\n")
    offsets = Int[]
    for (i, body) in enumerate(objs)
        push!(offsets, position(io))
        print(io, "$i 0 obj\n$body\nendobj\n")
    end
    xref = position(io)
    print(io, "xref\n0 $(length(objs) + 1)\n0000000000 65535 f \n")
    for o in offsets
        print(io, lpad(o, 10, '0'), " 00000 n \n")
    end
    print(io, "trailer\n<< /Size $(length(objs) + 1) /Root 1 0 R >>\nstartxref\n$xref\n%%EOF\n")
    write(path, take!(io))
end

# Stand-ins for the two tools: the plugin's seam takes any function that returns a Cmd.
function stand_in_tools(f, pdfinfo, pdftoppm, envs...)
    old = (ShipToolsPDFFile.PDFINFO[], ShipToolsPDFFile.PDFTOPPM[])
    ShipToolsPDFFile.PDFINFO[], ShipToolsPDFFile.PDFTOPPM[] = () -> `$pdfinfo`, () -> `$pdftoppm`
    try
        withenv(f, envs...)
    finally
        ShipToolsPDFFile.PDFINFO[], ShipToolsPDFFile.PDFTOPPM[] = old
    end
end

@testset "ShipToolsPDFFile" begin
    @testset "matches" begin
        for p in ("a.pdf", "A.PDF", "/x/y/paper.pdf")
            @test ConceptExplorerCore.matches(P, p)
        end
        for p in ("a.png", "a.md", "pdf", "a.pdf.bak", "noext")
            @test !ConceptExplorerCore.matches(P, p)
        end
    end

    @testset "page cache through preview" begin
        mktempdir() do dir
            tools = joinpath(dir, "bin")
            mkdir(tools)
            info_log, render_log = joinpath(dir, "info"), joinpath(dir, "render")
            write(info_log, "")
            write(render_log, "")
            write(joinpath(tools, "pdfinfo"), raw"""#!/bin/sh
                printf '%s\n' "$*" >> "$PDF_TEST_INFO"
                printf 'Pages: 4\nPage size: 600 x 800 pts\n'
                """)
            write(joinpath(tools, "pdftoppm"), raw"""#!/bin/sh
                printf '%s\n' "$*" >> "$PDF_TEST_RENDER"
                printf '%s' "$*"
                if [ "${PDF_TEST_BYTES:-0}" -gt 0 ]; then
                    dd if=/dev/zero bs="$PDF_TEST_BYTES" count=1 2>/dev/null
                fi
                """)
            chmod(joinpath(tools, "pdfinfo"), 0o755)
            chmod(joinpath(tools, "pdftoppm"), 0o755)
            sample = joinpath(dir, "cache.pdf")
            other = joinpath(dir, "other.pdf")
            write(sample, "first")
            write(other, "other")
            turns(path, page; w=400, h=800) = ConceptExplorerCore.preview(P, path,
                Dict{String,Any}("page" => page, "fit_w" => w, "fit_h" => h))
            infos() = length(readlines(info_log))
            renders() = length(readlines(render_log))
            stand_in_tools(joinpath(tools, "pdfinfo"), joinpath(tools, "pdftoppm"),
                           "PDF_TEST_INFO" => info_log, "PDF_TEST_RENDER" => render_log) do
                one = turns(sample, 1)
                @test one.mime == "image/png"
                @test renders() == 1 # No neighboring page is prefetched.
                @test turns(sample, 2).extras["page"] == 2
                again = turns(sample, 1)
                @test again.data == one.data
                @test again.extras == one.extras
                @test (infos(), renders()) == (1, 2)
                turns(sample, 1; w=200)
                @test (infos(), renders()) == (1, 3)
                turns(other, 1)
                @test (infos(), renders()) == (2, 4)
                run(`touch -m -t 200001010000 $sample`)
                turns(sample, 1)
                @test (infos(), renders()) == (3, 5)

                # Three pages exceed the byte bound; a hit makes page 1 newer than page 2.
                large = joinpath(dir, "large.pdf")
                write(large, "large")
                withenv("PDF_TEST_BYTES" => string(div(ShipToolsPDFFile.PAGE_CACHE_LIMIT, 3) + 1)) do
                    turns(large, 1)
                    turns(large, 2)
                    turns(large, 1)
                    turns(large, 3)
                    @test renders() == 8
                    turns(large, 1)
                    @test renders() == 8
                    turns(large, 2)
                    @test renders() == 9
                end
                oversized = joinpath(dir, "oversized.pdf")
                write(oversized, "oversized")
                withenv("PDF_TEST_BYTES" => string(ShipToolsPDFFile.PAGE_CACHE_LIMIT + 1)) do
                    turns(oversized, 1)
                    turns(oversized, 1)
                    @test renders() == 11
                end
            end
        end
    end

    # The real tools are the JLL's: nothing is skipped for a missing system poppler or ghostscript.
    sample = joinpath(mktempdir(), "sample.pdf")
    write_pdf(sample, 2)

    @testset "preview page 1 (2-arg)" begin
        pp = ConceptExplorerCore.preview(P, sample)
        @test pp.mime == "image/png"
        @test pp.data[1:8] == PNG_MAGIC
        @test pp.extras["page"] == 1
        @test pp.extras["page_count"] == 2
    end

    @testset "preview page 2 via params (3-arg)" begin
        pp = ConceptExplorerCore.preview(P, sample, Dict{String,Any}("page" => 2))
        @test pp.mime == "image/png"
        @test pp.extras["page"] == 2
    end

    @testset "page clamps to document" begin
        pp = ConceptExplorerCore.preview(P, sample, Dict{String,Any}("page" => 99))
        @test pp.extras["page"] == 2
        lo = ConceptExplorerCore.preview(P, sample, Dict{String,Any}("page" => 0))
        @test lo.extras["page"] == 1
    end

    @testset "fit hint sizes the raster to the pane" begin
        # PNG IHDR: width/height as big-endian u32 at bytes 17:20 / 21:24.
        dims(d) = (Int(d[17])<<24 | Int(d[18])<<16 | Int(d[19])<<8 | Int(d[20]),
                   Int(d[21])<<24 | Int(d[22])<<16 | Int(d[23])<<8 | Int(d[24]))
        pp = ConceptExplorerCore.preview(P, sample,
            Dict{String,Any}("page" => 1, "fit_w" => 400, "fit_h" => 800))
        w, h = dims(pp.data)
        # Letterbox fit: one axis lands exactly on the bound (the letter page is
        # width-bound against 400x800); either way both fit inside.
        @test w <= 400 && h <= 800
        @test w == 400 || h == 800
    end

    @testset "the JLL's poppler runs, never one on the search path" begin
        mktempdir() do dir
            marker = joinpath(dir, "consulted")
            loud = joinpath(dir, "loud")
            mkdir(loud)
            for tool in ("pdftoppm", "pdfinfo")
                write(joinpath(loud, tool), "#!/bin/sh\necho consulted >> '$marker'\nexit 7\n")
                chmod(joinpath(loud, tool), 0o755)
            end
            # A fresh path and mtime: nothing cached from the tests above.
            fresh = joinpath(dir, "fresh.pdf")
            write_pdf(fresh, 3)
            withenv("PATH" => loud * PATH_SEP * ENV["PATH"]) do
                pp = ConceptExplorerCore.preview(P, fresh)
                @test pp.mime == "image/png"
                @test pp.extras["page_count"] == 3
            end
            @test !ispath(marker)
            # No poppler on the search path at all.
            bare = joinpath(dir, "bare.pdf")
            write_pdf(bare, 2)
            withenv("PATH" => joinpath(dir, "empty")) do
                pp = ConceptExplorerCore.preview(P, bare)
                @test pp.mime == "image/png"
                @test pp.extras["page_count"] == 2
            end
        end
    end

    @testset "a slow tool ends inside the bound with the plugin's own note" begin
        # The daemon waits 10 s for a kernel answer; the two tools together stay under it.
        @test ShipToolsPDFFile.PREVIEW_BOUND < 10
        mktempdir() do dir
            slow_info = joinpath(dir, "pdfinfo")
            write(slow_info, "#!/bin/sh\nsleep 1\nprintf 'Pages: 2\\nPage size: 612 x 792 pts\\n'\n")
            hung_render = joinpath(dir, "pdftoppm")
            write(hung_render, "#!/bin/sh\nexec sleep 600\n")
            chmod(slow_info, 0o755)
            chmod(hung_render, 0o755)
            doc = joinpath(dir, "slow.pdf")
            write(doc, "slow")
            stand_in_tools(slow_info, hung_render) do
                # pdfinfo takes 1 s of the 2 s bound; pdftoppm gets the rest, not a bound of its own.
                elapsed = @elapsed pp = ShipToolsPDFFile._preview_page(doc, 1; bound=2)
                note = String(copy(pp.data))
                @test pp.mime == "text/markdown"
                @test occursin("Couldn't rasterize page 1", note)
                @test occursin("did not finish within", note)
                @test elapsed < 2.6
                # pdfinfo itself running out is its own note, not a corrupt file.
                stand_in_tools(hung_render, hung_render) do
                    gone = joinpath(dir, "gone.pdf")
                    write(gone, "gone")
                    elapsed = @elapsed pp = ShipToolsPDFFile._preview_page(gone, 1; bound=1)
                    note = String(copy(pp.data))
                    @test occursin("page count", note)
                    @test occursin("did not finish within", note)
                    @test !occursin("corrupt", note)
                    @test elapsed < 1.6
                end
            end
        end
    end

    @testset "3-arg fallback exists for non-paged plugins" begin
        # Core's fallback drops params for plugins that only define 2-arg.
        @test hasmethod(ConceptExplorerCore.preview,
                        Tuple{Type{P}, AbstractString, AbstractDict})
    end
end
