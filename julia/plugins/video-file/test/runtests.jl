using ShipToolsVideoFile
using ConceptExplorerCore
using Test

const V = ShipToolsVideoFile.VideoFile

# A stand-in ffmpeg reaches the plugin as Julia's copy: no system lookup finds anything, and a scratch tool folder holds
# the copy record that names it. `libpath` is the copy's library path, which the plugin puts in the command's
# environment.
function stand_in_ffmpeg(f, ffmpeg; libpath = "")
    old = (ConceptExplorerCore.WHICH[], ConceptExplorerCore.TOOL_HOME[])
    home = mktempdir()
    ConceptExplorerCore.WHICH[] = _ -> nothing
    ConceptExplorerCore.TOOL_HOME[] = home
    dir = ConceptExplorerCore.tool_dir(ShipToolsVideoFile.FFMPEG_TOOLS)
    mkpath(dir)
    write(joinpath(dir, "copy.txt"), "libpath_env=SOT_TEST_LIBS\nlibpath=$libpath\nffmpeg=$ffmpeg\n")
    try
        f()
    finally
        ConceptExplorerCore.WHICH[], ConceptExplorerCore.TOOL_HOME[] = old
        rm(home; recursive = true, force = true)
    end
end

const SAMPLE = joinpath(@__DIR__, "..", "..", "..", "..", "examples", "preview", "sample.mp4")
const PNG_MAGIC = UInt8[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]

@testset "ShipToolsVideoFile" begin
    @testset "matches" begin
        for p in ("a.mp4", "A.MP4", "/x/y/clip.webm", "movie.mov", "x.mkv", "y.m4v")
            @test ConceptExplorerCore.matches(V, p)
        end
        for p in ("a.png", "a.md", "a.mp3", "a.txt", "noext")
            @test !ConceptExplorerCore.matches(V, p)
        end
    end

    @testset "matches suffix corpus" begin
        for suffix in ("mp4", "webm", "mov", "mkv", "m4v")
            for spelling in (suffix, uppercase(suffix), uppercasefirst(suffix))
                for path in ("clip.$spelling", ".$spelling",
                             "many.dots.clip.$spelling", joinpath("folder", ".$spelling"))
                    @test ConceptExplorerCore.matches(ShipToolsVideoFile.VideoFile, path)
                end
            end
        end
        for path in ("", "noext", ".", ".txt", "clip.mp4.txt",
                     "dir.mp4/clip.txt", "clip.mp4/")
            @test !ConceptExplorerCore.matches(ShipToolsVideoFile.VideoFile, path)
        end
    end

    # The real ffmpeg, where the computer has one on its search path (the hosted run in real_fetch.jl proves the fetch).
    if Sys.which("ffmpeg") !== nothing && isfile(SAMPLE)
        @testset "preview poster" begin
            pp = ConceptExplorerCore.preview(V, SAMPLE)
            @test pp.mime == "image/png"
            @test length(pp.data) >= 8 && pp.data[1:8] == PNG_MAGIC
        end
    else
        @info "skipping the real-ffmpeg poster test (none on the search path)"
    end

    Sys.isunix() && @testset "the computer's own ffmpeg is used and nothing is fetched" begin
        mktempdir() do dir
            marker = joinpath(dir, "consulted")
            own = joinpath(dir, "own")
            mkdir(own)
            write(joinpath(own, "ffmpeg"), "#!/bin/sh\necho consulted >> '$marker'\nprintf 'png'\n")
            chmod(joinpath(own, "ffmpeg"), 0o755)
            old = (ConceptExplorerCore.TOOL_HOME[], ConceptExplorerCore.TOOL_FETCH[])
            starts = Ref(0)
            ConceptExplorerCore.TOOL_HOME[] = joinpath(dir, "home")
            ConceptExplorerCore.TOOL_FETCH[] = (d, s) -> (starts[] += 1; `true`)
            try
                withenv("PATH" => own * ":" * ENV["PATH"]) do
                    pp = ConceptExplorerCore.preview(V, "clip.mp4")
                    @test pp.mime == "image/png" && String(copy(pp.data)) == "png"
                end
                @test isfile(marker)
                @test starts[] == 0 && !ispath(ConceptExplorerCore.TOOL_HOME[])
            finally
                ConceptExplorerCore.TOOL_HOME[], ConceptExplorerCore.TOOL_FETCH[] = old
            end
        end
    end

    Sys.isunix() && @testset "no ffmpeg on the computer: a fetched copy is used, else one fetch starts and the note says so" begin
        mktempdir() do dir
            ffmpeg = joinpath(dir, "ffmpeg")
            write(ffmpeg, "#!/bin/sh\nprintf 'png'\n")
            chmod(ffmpeg, 0o755)
            stand_in_ffmpeg(ffmpeg) do
                pp = ConceptExplorerCore.preview(V, "clip.mp4")
                @test pp.mime == "image/png" && String(copy(pp.data)) == "png"
            end
            old = (ConceptExplorerCore.WHICH[], ConceptExplorerCore.TOOL_HOME[], ConceptExplorerCore.TOOL_FETCH[])
            starts = Ref(0)
            ConceptExplorerCore.WHICH[] = _ -> nothing
            ConceptExplorerCore.TOOL_HOME[] = joinpath(dir, "empty-home")
            ConceptExplorerCore.TOOL_FETCH[] = (d, s) -> (starts[] += 1; `true`)
            try
                pp = ConceptExplorerCore.preview(V, "clip.mp4")
                note = String(copy(pp.data))
                @test pp.mime == "text/markdown"
                @test occursin("being downloaded", note) && occursin("ffmpeg", note) && occursin("210 MB", note)
                @test occursin("once for this computer", note) && occursin("Select the file again", note)
                @test occursin("Playback opens in the browser with `o`", note)
                @test starts[] == 1
                # A failed fetch's fixed words are the next note's, and no new fetch starts inside the pace.
                write(joinpath(ConceptExplorerCore.tool_dir(ShipToolsVideoFile.FFMPEG_TOOLS), "failed.txt"), "the download failed\n")
                note = String(copy(ConceptExplorerCore.preview(V, "clip.mp4").data))
                @test occursin("could not be downloaded: the download failed", note)
                @test starts[] == 1
            finally
                ConceptExplorerCore.WHICH[], ConceptExplorerCore.TOOL_HOME[], ConceptExplorerCore.TOOL_FETCH[] = old
            end
        end
    end

    @testset "the plugin's environment holds no ffmpeg JLL" begin
        # Nothing is downloaded at instantiate: the copy is fetched at the first preview that needs it.
        # Boolean first: a failed @test prints its operands, and the manifest is long.
        held = occursin("[[deps.FFMPEG_jll]]", read(joinpath(dirname(Base.active_project()), "Manifest.toml"), String))
        @test !held
    end

    Sys.isunix() && @testset "a stuck ffmpeg ends inside the bound with the plugin's own note" begin
        mktempdir() do dir
            hung = joinpath(dir, "ffmpeg")
            write(hung, "#!/bin/sh\nexec sleep 600\n")
            chmod(hung, 0o755)
            stand_in_ffmpeg(hung) do
                elapsed = @elapsed pp = ShipToolsVideoFile._poster("clip.mp4", 1)
                note = String(copy(pp.data))
                @test pp.mime == "text/markdown"
                @test occursin("Couldn't extract a poster frame", note)
                @test occursin("did not finish within", note)
                @test elapsed < 1.6
            end
        end
    end

    Sys.isunix() && @testset "no note carries a command, an environment or text taken from one" begin
        # A copy's command carries the whole environment, and a process error shows its command. The copy's library
        # path, which the plugin puts in the command's environment, holds a marker: a leak shows without exposing a
        # real environment.
        marker = "marker-4c1e8a"
        mktempdir() do dir
            # One stand-in folder per behavior, so each is named ffmpeg as the tool is.
            function tool(folder, body; executable = true)
                mkpath(joinpath(dir, folder))
                path = joinpath(dir, folder, "ffmpeg")
                write(path, "#!/bin/sh\n$body\n")
                chmod(path, executable ? 0o755 : 0o644)
                path
            end
            for (cmd, words) in ((tool("exit", "exit 3"), "exited with code 3"),
                                 (tool("noexec", "exit 0"; executable = false), "could not start"),
                                 (tool("hang", "exec sleep 600"), "did not finish within"))
                stand_in_ffmpeg(cmd; libpath = "/" * marker) do
                    note = String(copy(ShipToolsVideoFile._poster("clip.mp4", 1).data))
                    # Booleans first: a failed @test prints its operands, and a leaking note must not be one of them.
                    leaked = occursin(marker, note) || occursin("setenv", note) || occursin("Process(", note)
                    said = occursin("ffmpeg " * words, note)
                    @test !leaked
                    @test said
                end
            end
        end
    end
end
