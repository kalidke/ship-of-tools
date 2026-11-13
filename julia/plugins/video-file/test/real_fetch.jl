# The real fetch, run by the hosted CI on Linux, macOS and Windows (the `preview-tools` job): a computer with no
# ffmpeg, Julia's copy downloaded by the real `fetch_tool.jl` into a scratch folder, and a real poster frame taken
# through it. Not part of `Pkg.test()`, which would download the copy on every run.
#   julia --project=julia/plugins/video-file julia/plugins/video-file/test/real_fetch.jl

using ConceptExplorerCore
using ShipToolsVideoFile
using Test

const CE = ConceptExplorerCore
sample = joinpath(@__DIR__, "..", "..", "..", "..", "examples", "preview", "sample.mp4")

@testset "Julia's copy of ffmpeg, fetched and run" begin
    CE.WHICH[] = _ -> nothing                   # as on a computer with no ffmpeg
    CE.TOOL_HOME[] = mktempdir()
    dir = CE.tool_dir(ShipToolsVideoFile.FFMPEG_TOOLS)
    poster() = CE.preview(ShipToolsVideoFile.VideoFile, sample)
    note = String(poster().data)
    @test occursin("being downloaded", note)
    # Each preview while the fetch runs says so; the poster comes when it ends, within the fetch's own bound.
    pp = poster()
    failure = Ref("")
    began = time()
    while pp.mime != "image/png" && time() - began < CE.FETCH_BOUND + 60
        text = String(copy(pp.data))
        occursin("could not be downloaded", text) && (failure[] = text; break)
        sleep(5)
        pp = poster()
    end
    isempty(failure[]) || (println(failure[]); println(read(joinpath(dir, "fetch.log"), String)))
    @test isempty(failure[])
    @test pp.mime == "image/png"
    @test pp.data[1:8] == UInt8[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
    @test isfile(joinpath(dir, "copy.txt"))
end
