# ConceptExplorerCore test suite — exercises the dispatch surface the way a
# plugin package does: define FileType subtypes, extend matches/preview through
# the public ABI, and drive discovery/resolution/fallback through the exported
# functions. No kernel, no I/O.

using Test
using ConceptExplorerCore

# -- test-only plugins ------------------------------------------------------

# Claims *.testtxt; defines ONLY the 2-arg preview — the 3-arg call must reach
# it through core's params-dropping fallback.
struct TestTxt <: FileType end
ConceptExplorerCore.matches(::Type{TestTxt}, path::AbstractString) =
    endswith(lowercase(path), ".testtxt")
ConceptExplorerCore.preview(::Type{TestTxt}, path) =
    PreviewPayload("text/plain", Vector{UInt8}(codeunits("two-arg")))

# Claims *.testpaged; defines ONLY the 3-arg paginated preview (ADR 0021 shape)
# and reports position via extras.
struct TestPaged <: FileType end
ConceptExplorerCore.matches(::Type{TestPaged}, path::AbstractString) =
    endswith(lowercase(path), ".testpaged")
function ConceptExplorerCore.preview(::Type{TestPaged}, path, params::AbstractDict)
    page = get(params, "page", 1)
    PreviewPayload("text/plain", Vector{UInt8}(codeunits("page=$page")),
                   Dict{String,Any}("page" => page, "page_count" => 3))
end

# A FileType subtype that never defined `matches` — file_type_for's hasmethod
# guard must skip it instead of erroring.
struct TestNoMatches <: FileType end

# -- suite -------------------------------------------------------------------

@testset "ConceptExplorerCore" begin
    @testset "file_types() discovers loaded subtypes" begin
        ts = file_types()
        @test TestTxt in ts
        @test TestPaged in ts
        @test TestNoMatches in ts
    end

    @testset "file_type_for resolution" begin
        @test file_type_for("a/b/report.testtxt") == TestTxt
        @test file_type_for("REPORT.TESTTXT") == TestTxt          # matches lowercases
        @test file_type_for("doc.testpaged") == TestPaged
        # nothing claims it — and TestNoMatches (no matches method) is skipped,
        # not an error
        @test file_type_for("mystery.zzz-unclaimed") === nothing
    end

    @testset "preview dispatch and the 3-arg fallback" begin
        # 3-arg call on a 2-arg-only plugin: fallback drops the params
        pp = preview(TestTxt, "f.testtxt", Dict{String,Any}("page" => 7))
        @test pp.mime == "text/plain"
        @test String(copy(pp.data)) == "two-arg"
        @test isempty(pp.extras)

        # 3-arg call on a paginated plugin: params arrive, extras come back
        pp2 = preview(TestPaged, "f.testpaged", Dict{String,Any}("page" => 2))
        @test String(copy(pp2.data)) == "page=2"
        @test pp2.extras["page"] == 2
        @test pp2.extras["page_count"] == 3

        # absent param uses the plugin's default
        pp3 = preview(TestPaged, "f.testpaged", Dict{String,Any}())
        @test pp3.extras["page"] == 1
    end

    @testset "TreeNode / PreviewPayload constructors" begin
        n = TreeNode("id1", "label", :module)
        @test n.has_children == false
        @test isempty(n.badges)
        @test isempty(n.payload)

        n2 = TreeNode("id2", "l", :file;
                      has_children = true,
                      badges = [:stale],
                      payload = Dict{String,Any}("k" => 1))
        @test n2.has_children
        @test n2.badges == [:stale]
        @test n2.payload["k"] == 1

        pp = PreviewPayload("application/octet-stream", UInt8[1, 2, 3])
        @test pp.mime == "application/octet-stream"
        @test isempty(pp.extras)
        @test pp.data == UInt8[1, 2, 3]
    end

    @testset "declared-but-unimplemented ABI stubs exist" begin
        # These are design seams (see the module docstring): the generic
        # functions must exist for plugins to extend, with no methods yet.
        @test ast_hash isa Function
        @test parse_entities isa Function
        @test applicable_annotations isa Function
        @test isempty(methods(ast_hash))
        @test isempty(methods(applicable_annotations))
    end
end
