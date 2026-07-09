# ShipToolsKernel test suite.
#
# Covers the two pieces the kernel is uniquely responsible for:
#   1. the per-entity AST hash (ADR 0005) — the provenance key the whole
#      concept-annotation staleness design rests on, so its invariants get
#      property-style tests here;
#   2. the project-scan module walker (nesting regression, via
#      scan_nesting.jl);
# plus a serve()-loop smoke test proving the NDJSON envelope path answers
# hello and contains unknown ops without dying.

using Test
using JSON3
using JuliaSyntax
using ShipToolsKernel

const SK = ShipToolsKernel

# Parse a source string and return name => per-entity ast_hash for its
# top-level definitions (the same collect_definitions path file.parse uses).
function entity_hashes(src::AbstractString)
    tree = JuliaSyntax.parseall(JuliaSyntax.SyntaxNode, src; filename = "<test>")
    defs = SK.collect_definitions(tree)
    Dict(d[:name] => d[:ast_hash] for d in defs)
end

@testset "ShipToolsKernel" begin
    @testset "per-entity AST hash invariants (ADR 0005)" begin
        base = entity_hashes("f(x) = x + 1")

        # Reformat-invariant: whitespace and comments are trivia, not code.
        reformat = entity_hashes("f(x)  =  x + 1   # a comment\n")
        @test base["f"] == reformat["f"]

        # Any value/structural change flips the hash.
        @test base["f"] != entity_hashes("f(x) = x + 2")["f"]

        # The name participates (same body, different name ⇒ different hash).
        @test base["f"] != entity_hashes("g(x) = x + 1")["g"]

        # Short-form and long-form are different textual realisations —
        # the hash is a fingerprint of the definition as written.
        @test base["f"] != entity_hashes("function f(x)\n    x + 1\nend")["f"]

        # Docstring edits do NOT flip the hash: the K"doc" wrapper is
        # stripped so the docstring is its own annotation surface.
        doc1 = entity_hashes("\"\"\"doc v1\"\"\"\nf(x) = x + 1")
        doc2 = entity_hashes("\"\"\"doc v2 — different words\"\"\"\nf(x) = x + 1")
        @test doc1["f"] == doc2["f"]
        @test doc1["f"] == base["f"]   # …and matches the undocumented form

        # Struct field-type changes flip the struct's hash.
        s1 = entity_hashes("struct S\n    a::Int\nend")
        s2 = entity_hashes("struct S\n    a::Float64\nend")
        @test s1["S"] != s2["S"]

        # Format: full SHA-256 hex (64 chars). Pinned here so any future
        # change to the hash format is a deliberate, test-visible decision
        # (it invalidates every synced_against in .concept/ sidecars).
        @test occursin(r"^[0-9a-f]{64}$", base["f"])
    end

    @testset "definition collection shapes" begin
        defs = SK.collect_definitions(JuliaSyntax.parseall(
            JuliaSyntax.SyntaxNode,
            """
            module M
            struct T <: Integer
                a::Int
            end
            h(x) = 2x
            end
            """; filename = "<test>"))
        byname = Dict(d[:name] => d for d in defs)
        @test byname["M"][:kind] == "module"
        @test byname["T"][:kind] == "struct"
        @test byname["T"][:parent] == "M"
        @test byname["T"][:supertype] == "Integer"
        @test [f[:name] for f in byname["T"][:fields]] == ["a"]
        @test byname["h"][:parent] == "M"
    end

    @testset "serve() answers hello and contains bad input" begin
        reqs = [
            Dict(:v => 1, :id => 1, :kind => "req", :op => "kernel.hello",
                 :payload => Dict()),
            Dict(:v => 1, :id => 2, :kind => "req", :op => "no.such_op",
                 :payload => Dict()),
        ]
        input = IOBuffer(join(JSON3.write.(reqs), "\n") * "\n")
        output = IOBuffer()
        SK.serve(input, output; project_root = mktempdir())

        lines = split(String(take!(output)), "\n"; keepempty = false)
        @test length(lines) == 2

        hello = JSON3.read(lines[1])
        @test hello.op == "kernel.hello"
        @test hello.payload.protocol == SK.PROTOCOL_VERSION
        @test "file.preview" in hello.payload.features

        unk = JSON3.read(lines[2])
        @test unk.id == 2
        @test unk.payload.code == "unknown_op"
    end
end

# project.scan nesting regression (asserts internally; a failure throws and
# fails the suite). Kept as a standalone script so the documented manual
# invocation — julia --project=julia/kernel julia/kernel/test/scan_nesting.jl —
# still works.
include("scan_nesting.jl")
