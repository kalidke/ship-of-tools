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

# Stand-ins for the two tools reach the plugin as Julia's copy: no system lookup finds anything, and a scratch tool
# folder holds the copy record that names them. `libpath` is the copy's library path, which the plugin puts in the
# environment of each command it runs.
function stand_in_tools(f, pdfinfo, pdftoppm, envs...; libpath = "")
    old = (ConceptExplorerCore.WHICH[], ConceptExplorerCore.TOOL_HOME[])
    home = mktempdir()
    ConceptExplorerCore.WHICH[] = _ -> nothing
    ConceptExplorerCore.TOOL_HOME[] = home
    dir = ConceptExplorerCore.tool_dir(ShipToolsPDFFile.POPPLER)
    mkpath(dir)
    write(joinpath(dir, "copy.txt"),
          "libpath_env=SOT_TEST_LIBS\nlibpath=$libpath\npdfinfo=$pdfinfo\npdftoppm=$pdftoppm\n")
    try
        withenv(f, envs...)
    finally
        ConceptExplorerCore.WHICH[], ConceptExplorerCore.TOOL_HOME[] = old
        rm(home; recursive = true, force = true)
    end
end

# The two real tools on this computer's search path, for the tests that run them; the plugin fetches Julia's copy
# only where there are none (the hosted run in real_fetch.jl proves that path).
const SYSTEM_POPPLER = Sys.which("pdftoppm") !== nothing && Sys.which("pdfinfo") !== nothing

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

    # The real tools, where the computer has them.
    sample = joinpath(mktempdir(), "sample.pdf")
    write_pdf(sample, 2)
    SYSTEM_POPPLER || @info "skipping the real-poppler tests (none on the search path)"

    SYSTEM_POPPLER && @testset "preview page 1 (2-arg)" begin
        pp = ConceptExplorerCore.preview(P, sample)
        @test pp.mime == "image/png"
        @test pp.data[1:8] == PNG_MAGIC
        @test pp.extras["page"] == 1
        @test pp.extras["page_count"] == 2
    end

    SYSTEM_POPPLER && @testset "preview page 2 via params (3-arg)" begin
        pp = ConceptExplorerCore.preview(P, sample, Dict{String,Any}("page" => 2))
        @test pp.mime == "image/png"
        @test pp.extras["page"] == 2
    end

    SYSTEM_POPPLER && @testset "page clamps to document" begin
        pp = ConceptExplorerCore.preview(P, sample, Dict{String,Any}("page" => 99))
        @test pp.extras["page"] == 2
        lo = ConceptExplorerCore.preview(P, sample, Dict{String,Any}("page" => 0))
        @test lo.extras["page"] == 1
    end

    SYSTEM_POPPLER && @testset "fit hint sizes the raster to the pane" begin
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

    Sys.isunix() && @testset "the computer's own poppler is used and nothing is fetched" begin
        mktempdir() do dir
            marker = joinpath(dir, "consulted")
            own = joinpath(dir, "own")
            mkdir(own)
            write(joinpath(own, "pdfinfo"), "#!/bin/sh\necho info >> '$marker'\nprintf 'Pages: 3\\nPage size: 612 x 792 pts\\n'\n")
            write(joinpath(own, "pdftoppm"), "#!/bin/sh\necho render >> '$marker'\nprintf 'png'\n")
            chmod(joinpath(own, "pdfinfo"), 0o755)
            chmod(joinpath(own, "pdftoppm"), 0o755)
            old = (ConceptExplorerCore.TOOL_HOME[], ConceptExplorerCore.TOOL_FETCH[])
            starts = Ref(0)
            ConceptExplorerCore.TOOL_HOME[] = joinpath(dir, "home")
            ConceptExplorerCore.TOOL_FETCH[] = (d, s) -> (starts[] += 1; `true`)
            try
                doc = joinpath(dir, "own.pdf")
                write(doc, "own")
                withenv("PATH" => own * PATH_SEP * ENV["PATH"]) do
                    pp = ConceptExplorerCore.preview(P, doc)
                    @test pp.mime == "image/png" && pp.extras["page_count"] == 3
                end
                @test readlines(marker) == ["info", "render"]
                @test starts[] == 0 && !ispath(ConceptExplorerCore.TOOL_HOME[])
            finally
                ConceptExplorerCore.TOOL_HOME[], ConceptExplorerCore.TOOL_FETCH[] = old
            end
        end
    end

    Sys.isunix() && @testset "no poppler on the computer: a fetched copy is used, else one fetch starts and the note says so" begin
        mktempdir() do dir
            info = joinpath(dir, "pdfinfo")
            write(info, "#!/bin/sh\nprintf 'Pages: 2\\nPage size: 612 x 792 pts\\n'\n")
            render = joinpath(dir, "pdftoppm")
            write(render, "#!/bin/sh\nprintf 'png'\n")
            chmod(info, 0o755)
            chmod(render, 0o755)
            doc = joinpath(dir, "copy.pdf")
            write(doc, "x")
            stand_in_tools(info, render) do
                pp = ConceptExplorerCore.preview(P, doc)
                @test pp.mime == "image/png" && String(copy(pp.data)) == "png" && pp.extras["page_count"] == 2
            end
            old = (ConceptExplorerCore.WHICH[], ConceptExplorerCore.TOOL_HOME[], ConceptExplorerCore.TOOL_FETCH[])
            starts = Ref(0)
            ConceptExplorerCore.WHICH[] = _ -> nothing
            ConceptExplorerCore.TOOL_HOME[] = joinpath(dir, "empty-home")
            ConceptExplorerCore.TOOL_FETCH[] = (d, s) -> (starts[] += 1; `true`)
            try
                pp = ConceptExplorerCore.preview(P, doc)
                note = String(copy(pp.data))
                @test pp.mime == "text/markdown"
                @test occursin("being downloaded", note) && occursin("poppler", note) && occursin("160 MB", note)
                @test occursin("once for this computer", note) && occursin("Select the file again", note)
                @test starts[] == 1
                # A failed fetch's fixed words are the next note's, and no new fetch starts inside the pace.
                write(joinpath(ConceptExplorerCore.tool_dir(ShipToolsPDFFile.POPPLER), "failed.txt"), "the download failed\n")
                note = String(copy(ConceptExplorerCore.preview(P, doc).data))
                @test occursin("could not be downloaded: the download failed", note)
                @test starts[] == 1
            finally
                ConceptExplorerCore.WHICH[], ConceptExplorerCore.TOOL_HOME[], ConceptExplorerCore.TOOL_FETCH[] = old
            end
        end
    end

    @testset "the plugin's environment holds no poppler JLL" begin
        # Nothing is downloaded at instantiate: the copy is fetched at the first preview that needs it.
        # Boolean first: a failed @test prints its operands, and the manifest is long.
        held = occursin("[[deps.Poppler_jll]]", read(joinpath(dirname(Base.active_project()), "Manifest.toml"), String))
        @test !held
    end

    @testset "a slow tool ends inside the bound with the plugin's own note" begin
        # A page's two tools share the core's bound for a request's tools.
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

    Sys.isunix() && @testset "no note carries a command, an environment or text taken from one" begin
        # A copy's commands carry the whole environment, and a process error shows its command. The copy's library
        # path, which the plugin puts in each command's environment, holds a marker: a leak shows without exposing
        # a real environment.
        marker = "marker-4c1e8a"
        mktempdir() do dir
            # One stand-in folder per behavior, so each is named pdfinfo or pdftoppm as the tool is.
            function tool(folder, name, body; executable = true)
                mkpath(joinpath(dir, folder))
                path = joinpath(dir, folder, name)
                write(path, "#!/bin/sh\n$body\n")
                chmod(path, executable ? 0o755 : 0o644)
                path
            end
            info_ok = tool("ok", "pdfinfo", "printf 'Pages: 2\\nPage size: 612 x 792 pts\\n'")
            info_exit = tool("exit", "pdfinfo", "exit 3")
            info_hang = tool("hang", "pdfinfo", "exec sleep 600")
            info_noexec = tool("noexec", "pdfinfo", "exit 0"; executable = false)
            render_exit = tool("exit", "pdftoppm", "exit 5")
            render_hang = tool("hang", "pdftoppm", "exec sleep 600")
            render_noexec = tool("noexec", "pdftoppm", "exit 0"; executable = false)
            n = Ref(0)
            function note_for(info, render; bound=2)
                doc = joinpath(dir, "doc$(n[] += 1).pdf")
                write(doc, "x")
                stand_in_tools(info, render; libpath = "/" * marker) do
                    pp = ShipToolsPDFFile._preview_page(doc, 1; bound)
                    pp.mime == "text/markdown" ? String(copy(pp.data)) : ""
                end
            end
            for (note, words) in (
                    (note_for(info_exit, render_exit), "couldn't read this file"),
                    (note_for(info_noexec, render_exit), "pdfinfo could not start"),
                    (note_for(info_hang, render_exit; bound=1), "pdfinfo did not finish within"),
                    (note_for(info_ok, render_exit), "pdftoppm exited with code 5"),
                    (note_for(info_ok, render_noexec), "pdftoppm could not start"),
                    (note_for(info_ok, render_hang), "pdftoppm did not finish within"))
                # Booleans first: a failed @test prints its operands, and a leaking note must not be one of them.
                leaked = occursin(marker, note) || occursin("setenv", note) || occursin("Process(", note)
                said = occursin(words, note)
                @test !leaked
                @test said
            end
        end
    end

    @testset "3-arg fallback exists for non-paged plugins" begin
        # Core's fallback drops params for plugins that only define 2-arg.
        @test hasmethod(ConceptExplorerCore.preview,
                        Tuple{Type{P}, AbstractString, AbstractDict})
    end
end
