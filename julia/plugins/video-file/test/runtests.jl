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

    @testset "preview poster" begin
        # The real ffmpeg is the JLL's: nothing is skipped for a missing system ffmpeg.
        sample = joinpath(@__DIR__, "..", "..", "..", "..", "examples", "preview", "sample.mp4")
        @test isfile(sample)
        pp = ConceptExplorerCore.preview(V, sample)
        @test pp.mime == "image/png"
        # PNG magic number.
        @test length(pp.data) >= 8
        @test pp.data[1:8] == UInt8[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
    end

    @testset "the JLL's ffmpeg runs, never one on the search path" begin
        sample = joinpath(@__DIR__, "..", "..", "..", "..", "examples", "preview", "sample.mp4")
        mktempdir() do dir
            marker = joinpath(dir, "consulted")
            loud = joinpath(dir, "loud")
            mkdir(loud)
            write(joinpath(loud, "ffmpeg"), "#!/bin/sh\necho consulted >> '$marker'\nexit 7\n")
            chmod(joinpath(loud, "ffmpeg"), 0o755)
            sep = Sys.iswindows() ? ";" : ":"
            withenv("PATH" => loud * sep * ENV["PATH"]) do
                @test ConceptExplorerCore.preview(V, sample).mime == "image/png"
            end
            @test !ispath(marker)
            # No ffmpeg on the search path at all.
            withenv("PATH" => joinpath(dir, "empty")) do
                @test ConceptExplorerCore.preview(V, sample).mime == "image/png"
            end
        end
    end

    @testset "a stuck ffmpeg ends inside the bound with the plugin's own note" begin
        # The daemon waits 10 s for a kernel answer; the tool's bound stays under it.
        @test ShipToolsVideoFile.TOOL_BOUND < 10
        mktempdir() do dir
            hung = joinpath(dir, "ffmpeg")
            write(hung, "#!/bin/sh\nexec sleep 600\n")
            chmod(hung, 0o755)
            old = ShipToolsVideoFile.FFMPEG[]
            ShipToolsVideoFile.FFMPEG[] = () -> `$hung`
            try
                elapsed = @elapsed pp = ShipToolsVideoFile._poster("clip.mp4", 1)
                note = String(copy(pp.data))
                @test pp.mime == "text/markdown"
                @test occursin("Couldn't extract a poster frame", note)
                @test occursin("did not finish within", note)
                @test elapsed < 1.6
            finally
                ShipToolsVideoFile.FFMPEG[] = old
            end
        end
    end

    @testset "no note carries a command, an environment or text taken from one" begin
        # The JLL's command form holds the kernel's whole environment, and a process error shows its command. The
        # stand-in commands carry a small fake environment with a marker, so a leak shows without exposing a real one.
        marker = "marker-4c1e8a"
        fake(path) = setenv(`$path`, Dict("SOT_TEST_MARKER" => marker, "PATH" => "/usr/bin:/bin"))
        mktempdir() do dir
            function tool(name, body)
                path = joinpath(dir, name)
                write(path, "#!/bin/sh\n$body\n")
                chmod(path, 0o755)
                path
            end
            old = ShipToolsVideoFile.FFMPEG[]
            try
                for (cmd, words) in ((tool("exit", "exit 3"), "ffmpeg exited with code 3"),
                                     (joinpath(dir, "absent"), "ffmpeg could not start"),
                                     (tool("hang", "exec sleep 600"), "ffmpeg did not finish within"))
                    # The note names the tool in fixed words, whatever the stand-in is called.
                    ShipToolsVideoFile.FFMPEG[] = () -> fake(cmd)
                    note = String(copy(ShipToolsVideoFile._poster("clip.mp4", 1).data))
                    # Booleans first: a failed @test prints its operands, and a leaking note must not be one of them.
                    leaked = occursin(marker, note) || occursin("setenv", note) || occursin("Process(", note)
                    said = occursin(replace(words, "ffmpeg" => basename(cmd)), note)
                    @test !leaked
                    @test said
                end
            finally
                ShipToolsVideoFile.FFMPEG[] = old
            end
        end
    end
end
