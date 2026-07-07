using ShipToolsVideoFile
using ConceptExplorerCore
using Test

const V = ShipToolsVideoFile.VideoFile

@testset "ShipToolsVideoFile" begin
    @testset "matches" begin
        for p in ("a.mp4", "A.MP4", "/x/y/clip.webm", "movie.mov", "x.mkv", "y.m4v")
            @test ConceptExplorerCore.matches(V, p)
        end
        for p in ("a.png", "a.md", "a.mp3", "a.txt", "noext")
            @test !ConceptExplorerCore.matches(V, p)
        end
    end

    @testset "preview poster" begin
        sample = joinpath(@__DIR__, "..", "..", "..", "..", "examples", "preview", "sample.mp4")
        if Sys.which("ffmpeg") !== nothing && isfile(sample)
            pp = ConceptExplorerCore.preview(V, sample)
            @test pp.mime == "image/png"
            # PNG magic number.
            @test length(pp.data) >= 8
            @test pp.data[1:8] == UInt8[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
        else
            @info "skipping poster test (ffmpeg or sample.mp4 absent)"
        end
    end
end
