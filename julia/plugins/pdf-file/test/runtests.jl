using ShipToolsPDFFile
using ConceptExplorerCore
using Test

const P = ShipToolsPDFFile.PDFFile
const PNG_MAGIC = UInt8[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]

@testset "ShipToolsPDFFile" begin
    @testset "matches" begin
        for p in ("a.pdf", "A.PDF", "/x/y/paper.pdf")
            @test ConceptExplorerCore.matches(P, p)
        end
        for p in ("a.png", "a.md", "pdf", "a.pdf.bak", "noext")
            @test !ConceptExplorerCore.matches(P, p)
        end
    end

    # A real rasterize needs poppler + a sample; generate a 2-page PDF with
    # ghostscript if available, else skip (CI hosts vary).
    sample = joinpath(mktempdir(), "sample.pdf")
    gs = Sys.which("gs")
    havepoppler = Sys.which("pdftoppm") !== nothing && Sys.which("pdfinfo") !== nothing
    if gs !== nothing && havepoppler
        run(pipeline(`$gs -q -dBATCH -dNOPAUSE -sDEVICE=pdfwrite -o $sample
                      -c "/Helvetica findfont 24 scalefont setfont
                          100 700 moveto (page one) show showpage
                          100 700 moveto (page two) show showpage"`,
                     devnull))

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
            # Letterbox fit: one axis lands exactly on the bound (gs default
            # page is taller-than-wide relative to 400x800 → width-bound or
            # height-bound depending on aspect; either way both fit inside).
            @test w <= 400 && h <= 800
            @test w == 400 || h == 800
        end
    else
        @info "skipping rasterize tests (gs or poppler absent)"
    end

    @testset "3-arg fallback exists for non-paged plugins" begin
        # Core's fallback drops params for plugins that only define 2-arg.
        @test hasmethod(ConceptExplorerCore.preview,
                        Tuple{Type{P}, AbstractString, AbstractDict})
    end
end
