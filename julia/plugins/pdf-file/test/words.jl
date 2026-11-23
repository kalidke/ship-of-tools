# A page's words (`params["words"] === true`): included by runtests.jl inside its one testset.

const WORDS_MIME = "application/vnd.sot.page-words+json"
# The core's own JSON reader: the document must be JSON as any reader takes it.
const JSON3 = ConceptExplorerCore.JSON3

# The document one page's words make, pinned byte for byte: the window and the daemon read this form.
const WORDS_XHTML = """
<html><body><doc>
  <page width="200.000000" height="100.000000">
    <flow><block xMin="40.000000" yMin="20.000000" xMax="160.000000" yMax="80.000000">
        <line xMin="40.000000" yMin="20.000000" xMax="160.000000" yMax="40.000000">
          <word xMin="40.000000" yMin="20.000000" xMax="80.000000" yMax="40.000000">alpha</word>
          <word xMin="120.000000" yMin="20.000000" xMax="160.000000" yMax="40.000000">beta</word>
        </line>
        <line xMin="40.000000" yMin="60.000000" xMax="80.000000" yMax="80.000000">
          <word xMin="40.000000" yMin="60.000000" xMax="80.000000" yMax="80.000000">gamma</word>
        </line>
    </block></flow>
  </page>
</doc></body></html>
"""
const WORDS_DOCUMENT = """{"page":2,"cut":false,"lines":[[[13107,13107,26214,26214,"alpha"],[39321,13107,52428,26214,"beta"]],[[13107,39321,26214,52428,"gamma"]]]}"""

# A folder of stand-in poppler programs, as Julia's copy holds them side by side: `pdfinfo` answers a page turned by
# `rot`, `pdftoppm` a picture, and `pdftotext` prints the file `xhtml`. `text` replaces the text tool's script, and
# `nothing` leaves the folder without one. Each run of a tool adds its arguments to `<dir>/<tool>.log`.
function words_tools(dir; rot = 0, xhtml = WORDS_XHTML, text = "cat '$(joinpath(dir, "page.xhtml"))'", info = nothing)
    mkpath(dir)
    write(joinpath(dir, "page.xhtml"), xhtml)
    scripts = Dict("pdfinfo" => something(info, "printf 'Pages: 2\\nPage    2 size: 200 x 100 pts\\nPage    2 rot:  $rot\\nPage size: 200 x 100 pts\\n'"),
                   "pdftoppm" => "printf 'png'", "pdftotext" => text)
    for (name, body) in scripts
        body === nothing && continue
        write(joinpath(dir, name), "#!/bin/sh\nprintf '%s\\n' \"\$*\" >> '$(joinpath(dir, name)).log'\n$body\n")
        chmod(joinpath(dir, name), 0o755)
    end
    joinpath(dir, "pdfinfo"), joinpath(dir, "pdftoppm")
end

words_of(path, page; more...) = ConceptExplorerCore.preview(P, path, Dict{String,Any}("page" => page, "words" => true, more...))
runs(dir, tool) = isfile(joinpath(dir, "$tool.log")) ? readlines(joinpath(dir, "$tool.log")) : String[]
# The document's lines as the words' texts, and its boxes in order.
texts(doc) = [[String(w[5]) for w in line] for line in doc.lines]
boxes(doc) = [Int.(w[1:4]) for line in doc.lines for w in line]

# A PDF of one US-letter page, turned by `rotate`, with "alpha beta" and under it "gamma" in 24-point Helvetica (a
# standard font, so none is embedded): "alpha" starts 72 points from the left with its baseline 700 points up.
function write_text_pdf(path; rotate = 0)
    stream = "BT /F1 24 Tf 1 0 0 1 72 700 Tm (alpha beta) Tj 1 0 0 1 72 600 Tm (gamma) Tj ET"
    objs = ["<< /Type /Catalog /Pages 2 0 R >>", "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Rotate $rotate /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
            "<< /Length $(sizeof(stream)) >>\nstream\n$stream\nendstream",
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"]
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

Sys.isunix() && @testset "a words request answers the page's lines and boxes" begin
    mktempdir() do dir
        info, render = words_tools(joinpath(dir, "bin"))
        doc = joinpath(dir, "doc.pdf")
        write(doc, "x")
        stand_in_tools(info, render) do
            pp = words_of(doc, 2)
            @test pp.mime == WORDS_MIME
            @test String(copy(pp.data)) == WORDS_DOCUMENT
            @test texts(JSON3.read(pp.data)) == [["alpha", "beta"], ["gamma"]]
            # One run of each tool, for that page alone, and no picture drawn.
            @test runs(joinpath(dir, "bin"), "pdfinfo") == ["-f 2 -l 2 $doc"]
            @test runs(joinpath(dir, "bin"), "pdftotext") == ["-bbox-layout -f 2 -l 2 $doc -"]
            @test isempty(runs(joinpath(dir, "bin"), "pdftoppm"))
        end
    end
end

Sys.isunix() && @testset "a quarter turn swaps the page's sides" begin
    # The tool gives a turned page's boxes in the turned page's space and its size unturned.
    one = replace(WORDS_XHTML, r"<word.*</word>\n"s => "<word xMin=\"40.000000\" yMin=\"40.000000\" xMax=\"80.000000\" yMax=\"80.000000\">w</word>\n")
    for (rot, box) in ((0, [13107, 26214, 26214, 52428]), (180, [13107, 26214, 26214, 52428]),
                       (90, [26214, 13107, 52428, 26214]), (270, [26214, 13107, 52428, 26214]))
        mktempdir() do dir
            info, render = words_tools(joinpath(dir, "bin"); rot, xhtml = one)
            doc = joinpath(dir, "doc.pdf")
            write(doc, "x")
            stand_in_tools(info, render) do
                @test boxes(JSON3.read(words_of(doc, 2).data)) == [box]
            end
        end
    end
end

Sys.isunix() && @testset "a word's text is decoded and cleaned" begin
    word(x0, x1, text) = "<word xMin=\"$x0\" yMin=\"20.000000\" xMax=\"$x1\" yMax=\"40.000000\">$text</word>\n"
    xhtml = "<page width=\"200.000000\" height=\"100.000000\">\n<line>\n" *
            word("-40.000000", "40.000000", "a&amp;b") * word("40.000000", "80.000000", "&lt;c&gt;") *
            word("80.000000", "120.000000", "&quot;q&quot;") * word("160.000000", "120.000000", "&apos;x&apos;") *
            word("160.000000", "240.000000", "&amp;lt;") *
            "</line>\n<line>\n" * word("40.000000", "80.000000", "\x01\u2028\u0085") * "</line>\n<line>\n" *
            word("40.000000", "80.000000", "back\\slash") * word("80.000000", "120.000000", "\x1b[31mred\u2029") *
            word("120.000000", "160.000000", "\x07") * word("160.000000", "200.000000", "by\xffte") * "</line>\n"
    mktempdir() do dir
        info, render = words_tools(joinpath(dir, "bin"); xhtml)
        doc = joinpath(dir, "doc.pdf")
        write(doc, "x")
        stand_in_tools(info, render) do
            read_doc = JSON3.read(words_of(doc, 2).data)
            # A word and a line left empty are left out; no control character, line separator or byte that is no
            # character stays in a word.
            @test texts(read_doc) == [["a&b", "<c>", "\"q\"", "'x'", "&lt;"], ["back\\slash", "[31mred", "byte"]]
            # A box past the page's edge is clamped to it.
            @test boxes(read_doc)[1] == [0, 13107, 13107, 26214]
            @test boxes(read_doc)[5] == [52428, 13107, 65535, 26214]
            # A box whose sides came the other way round is put in order.
            @test boxes(read_doc)[4] == [39321, 13107, 52428, 26214]
        end
    end
end

Sys.isunix() && @testset "the document is cut at a whole word under its bound" begin
    words = join(("<word xMin=\"40.000000\" yMin=\"20.000000\" xMax=\"80.000000\" yMax=\"40.000000\">w$i</word>\n" for i in 1:200))
    xhtml = "<page width=\"200.000000\" height=\"100.000000\">\n<line>\n$words</line>\n"
    mktempdir() do dir
        info, render = words_tools(joinpath(dir, "bin"); xhtml)
        doc = joinpath(dir, "doc.pdf")
        write(doc, "x")
        stand_in_tools(info, render) do
            whole = ShipToolsPDFFile._page_words(doc, 2)
            @test only(texts(JSON3.read(whole.data))) == ["w$i" for i in 1:200]
            @test JSON3.read(whole.data).cut == false
            for limit in (100, 600, 601, 2000)
                cut = ShipToolsPDFFile._page_words(doc, 2; limit)
                read_doc = JSON3.read(cut.data)
                kept = isempty(read_doc.lines) ? String[] : only(texts(read_doc))
                # Under the bound, and so near it that the next word (at most 36 bytes here) would have passed it.
                @test limit - 36 < length(cut.data) <= limit
                @test read_doc.cut == true
                @test kept == ["w$i" for i in 1:length(kept)]
            end
            @test ShipToolsPDFFile.MAX_WORDS_BYTES <= 512 * 1024
        end
    end
end

Sys.isunix() && @testset "a page with no text answers no lines" begin
    mktempdir() do dir
        info, render = words_tools(joinpath(dir, "bin"); xhtml = "<doc>\n  <page width=\"612.000000\" height=\"792.000000\">\n  </page>\n</doc>\n")
        doc = joinpath(dir, "doc.pdf")
        write(doc, "x")
        stand_in_tools(info, render) do
            pp = words_of(doc, 2)
            @test pp.mime == WORDS_MIME
            @test String(copy(pp.data)) == """{"page":2,"cut":false,"lines":[]}"""
        end
    end
end

Sys.isunix() && @testset "the text tool is the one beside pdftoppm, and no text tool is a note" begin
    mktempdir() do dir
        lib = joinpath(dir, "lib")
        mkpath(lib)
        seen = joinpath(dir, "seen")
        # pdfinfo is in another folder: the text tool is found beside pdftoppm alone, and runs with the copy's library path.
        info, _ = words_tools(joinpath(dir, "info"); text = nothing)
        _, render = words_tools(joinpath(dir, "bin"); text = "printf '%s' \"\$SOT_TEST_LIBS\" > '$seen'\ncat '$(joinpath(dir, "bin", "page.xhtml"))'")
        doc = joinpath(dir, "doc.pdf")
        write(doc, "x")
        stand_in_tools(info, render, "SOT_TEST_LIBS" => nothing; libpath = lib) do
            @test String(copy(words_of(doc, 2).data)) == WORDS_DOCUMENT
            @test read(seen, String) == lib
        end
        stand_in_tools(info, joinpath(dir, "info", "pdftoppm")) do
            pp = words_of(doc, 2)
            @test pp.mime == "text/markdown"
            @test occursin("no `pdftotext`", String(copy(pp.data)))
        end
    end
end

Sys.isunix() && @testset "a words request that fails is a note of fixed words, inside the one bound" begin
    marker = "marker-7d2f1b"
    mktempdir() do dir
        lib = joinpath(dir, marker)
        mkpath(lib)
        n = Ref(0)
        function note_for(; bound = 2, tools...)
            info, render = words_tools(joinpath(dir, "bin$(n[] += 1)"); tools...)
            doc = joinpath(dir, "doc$(n[]).pdf")
            write(doc, "x")
            stand_in_tools(info, render; libpath = lib) do
                elapsed = @elapsed pp = ShipToolsPDFFile._page_words(doc, 2; bound)
                @test elapsed < bound + 0.6
                pp.mime == "text/markdown" ? String(copy(pp.data)) : ""
            end
        end
        for (note, words) in (
                (note_for(text = "exit 3"), "pdftotext exited with code 3"),
                (note_for(bound = 1, text = "exec sleep 600"), "pdftotext did not finish within"),
                (note_for(info = "exit 99"), "pdfinfo exited with code 99"),
                # The two tools share the bound: pdfinfo takes 1 s of 2 s, and the text tool gets the rest.
                (note_for(info = "sleep 1\nprintf 'Page    2 rot:  0\\n'", text = "exec sleep 600"), "pdftotext did not finish within"),
                (note_for(xhtml = "<html>no page element</html>\n"), "was not understood"),
                # A page with no width has no fractions to give.
                (note_for(xhtml = replace(WORDS_XHTML, "width=\"200.000000\"" => "width=\"0.000000\"")), "was not understood"))
            leaked = occursin(marker, note) || occursin("setenv", note) || occursin("Process(", note)
            said = occursin(words, note) && occursin("page 2", note)
            @test !leaked
            @test said
        end
    end
end

Sys.isunix() && @testset "a page request runs no text tool" begin
    mktempdir() do dir
        info, render = words_tools(joinpath(dir, "bin"))
        doc = joinpath(dir, "doc.pdf")
        write(doc, "x")
        stand_in_tools(info, render) do
            for pp in (ConceptExplorerCore.preview(P, doc),
                       ConceptExplorerCore.preview(P, doc, Dict{String,Any}("page" => 2, "fit_w" => 400, "fit_h" => 800)),
                       ConceptExplorerCore.preview(P, doc, Dict{String,Any}("page" => 2, "words" => false)),
                       ConceptExplorerCore.preview(P, doc, Dict{String,Any}("page" => 2, "words" => "true")))
                @test pp.mime == "image/png" && String(copy(pp.data)) == "png"
            end
            @test isempty(runs(joinpath(dir, "bin"), "pdftotext"))
            @test length(runs(joinpath(dir, "bin"), "pdftoppm")) == 3   # the fourth request repeats the third and is the kept page
        end
    end
end

SYSTEM_POPPLER && @testset "the real tools give a generated page's words where they were drawn" begin
    dir = mktempdir()
    # A point 5 points right of a word's start and 5 above its baseline, as fractions of the drawn page times 65535.
    inside(box, fx, fy) = box[1] <= fx * 65535 <= box[3] && box[2] <= fy * 65535 <= box[4]
    upright = joinpath(dir, "upright.pdf")
    write_text_pdf(upright)
    doc = JSON3.read(words_of(upright, 1).data)
    @test texts(doc) == [["alpha", "beta"], ["gamma"]]
    @test doc.page == 1 && doc.cut == false
    alpha, beta, gamma = boxes(doc)
    @test inside(alpha, 77 / 612, (792 - 705) / 792)
    @test inside(gamma, 77 / 612, (792 - 605) / 792)
    @test alpha[3] < beta[1] && alpha[4] < gamma[2]
    # A page turned a quarter is drawn 792 wide and 612 high, and a point (x, y up) of it lands at (y, x) from the top left.
    turned = joinpath(dir, "turned.pdf")
    write_text_pdf(turned; rotate = 90)
    doc = JSON3.read(words_of(turned, 1).data)
    @test texts(doc) == [["alpha", "beta"], ["gamma"]]
    alpha, _, gamma = boxes(doc)
    @test inside(alpha, 705 / 792, 77 / 612)
    @test inside(gamma, 605 / 792, 77 / 612)
    # A page past the document's end is a note, as every failure is.
    @test words_of(upright, 9).mime == "text/markdown"
end
