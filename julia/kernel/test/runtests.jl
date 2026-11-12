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
using ConceptExplorerCore
using ShipToolsKernel

const SK = ShipToolsKernel

# Parse a source string and return name => per-entity ast_hash for its
# top-level definitions (the same collect_definitions path file.parse uses).
function entity_hashes(src::AbstractString)
    tree = JuliaSyntax.parseall(JuliaSyntax.SyntaxNode, src; filename = "<test>")
    defs = SK.collect_definitions(tree)
    Dict(d[:name] => d[:ast_hash] for d in defs)
end

# Stand-in plugins that throw whatever `THROWN` holds, from `preview` or from `matches`.
const THROWN = Ref{Any}()
struct BoomToolFile <: ConceptExplorerCore.FileType end
ConceptExplorerCore.matches(::Type{BoomToolFile}, path::AbstractString) = endswith(path, ".boomtool")
ConceptExplorerCore.preview(::Type{BoomToolFile}, path::AbstractString) = throw(THROWN[])
struct BoomMatchFile <: ConceptExplorerCore.FileType end
ConceptExplorerCore.matches(::Type{BoomMatchFile}, path::AbstractString) =
    endswith(path, ".boommatch") ? throw(THROWN[]) : false

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

        unk = JSON3.read(lines[2])
        @test unk.id == 2
        @test unk.payload.code == "unknown_op"
    end

    @testset "the PDF and video plugins load at their first preview" begin
        # Their tools are JLLs (about 37 MiB), so a kernel that never previews one does not load them.
        loaded(name) = any(m -> nameof(m) === name, values(Base.loaded_modules))
        @test !loaded(:ShipToolsPDFFile) && !loaded(:ShipToolsVideoFile)
        examples = joinpath(@__DIR__, "..", "..", "..", "examples", "preview")
        function preview_mime(file)
            req = JSON3.write(Dict(:v => 1, :id => 3, :kind => "req", :op => "file.preview",
                                   :payload => Dict(:path => joinpath(examples, file))))
            output = IOBuffer()
            SK.serve(IOBuffer(req * "\n"), output; project_root = examples)
            JSON3.read(String(take!(output))).payload.mime
        end
        @test preview_mime("sample.pdf") == "image/png"
        @test loaded(:ShipToolsPDFFile) && !loaded(:ShipToolsVideoFile)
        @test preview_mime("sample.mp4") == "image/png"
        @test loaded(:ShipToolsVideoFile)
        # Every extension a lazy plugin claims has its row.
        for ext in Main.ShipToolsVideoFile.VIDEO_EXTENSIONS
            @test get(SK.LAZY_PLUGIN_FOR_EXT, ext, nothing) == "ShipToolsVideoFile"
        end
        @test SK.LAZY_PLUGIN_FOR_EXT[".pdf"] == "ShipToolsPDFFile"
    end

    @testset "no reply or stderr line holds a command's environment" begin
        # A JLL's command form carries the whole environment, and a process error shows its command. A stand-in plugin
        # throws such errors; the stand-in commands carry a small fake environment with a marker.
        marker = "marker-4c1e8a"
        env = Dict("SOT_TEST_MARKER" => marker, "PATH" => "/usr/bin:/bin")
        thrown(cmd) = try run(setenv(cmd, env)); nothing catch e e end
        errors = [thrown(`false`), thrown(`/no/such/tool`)]
        # The class is real: Julia's own text for each holds the marker.
        shown = [occursin(marker, sprint(showerror, e)) for e in errors]
        @test all(shown)
        mktempdir() do dir
            for ext in (".boomtool", ".boommatch"), err in errors
                path = joinpath(dir, "file" * ext)
                write(path, "x")
                THROWN[] = err
                req = JSON3.write(Dict(:v => 1, :id => 7, :kind => "req", :op => "file.preview",
                                       :payload => Dict(:path => path)))
                output = IOBuffer()
                errfile = joinpath(dir, "stderr")
                open(errfile, "w") do io
                    redirect_stderr(io) do
                        SK.serve(IOBuffer(req * "\n"), output; project_root = dir)
                    end
                end
                reply = String(take!(output))
                logged = read(errfile, String)
                # Booleans first: a failed @test prints its operands, and a leaking reply must not be one of them.
                leaked = occursin(marker, reply) || occursin(marker, logged)
                answered = occursin("\"id\":7", reply)
                @test !leaked
                @test answered
            end
        end
    end
end

# project.scan nesting regression (asserts internally; a failure throws and
# fails the suite). Kept as a standalone script so the documented manual
# invocation — julia --project=julia/kernel julia/kernel/test/scan_nesting.jl —
# still works.
include("scan_nesting.jl")
