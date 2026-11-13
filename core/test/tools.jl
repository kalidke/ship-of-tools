# Julia's copy of an external tool: fetched only where the computer has none, one fetch at a time across processes.
# Stand-ins all the way: a fake search path, a scratch tool folder, and a fetch command that runs the real
# `fetch_tool.jl` with the download replaced, so nothing is downloaded and the lock, the bound and the words are the
# real ones. Unix only: the kill and the file times are Unix's.

using Test
using ConceptExplorerCore

const CE = ConceptExplorerCore

Sys.isunix() && @testset "Julia's copy of an external tool" begin
    set = CE.ToolSet("widget", "Widget_jll", "00000000-0000-0000-0000-000000000001", "1", ["wid", "get"], "9 MB")
    script = joinpath(pkgdir(CE), "src", "fetch_tool.jl")
    julia = `$(Base.julia_cmd()) --startup-file=no --project=$(dirname(Base.active_project()))`
    old = (CE.WHICH[], CE.TOOL_HOME[], CE.TOOL_FETCH[], CE.FETCH_PACE[])
    words(r) = r isa AbstractString ? r : ""
    until(f) = timedwait(f, 60; pollint = 0.05) == :ok

    # A scratch tool folder, a search path holding `found`, and a fetch command whose download is `work(release)`:
    # Julia source for the body of the script's `install`, which gets the fetch's folder as `dir`. `body` gets what it
    # needs to look at; the scenario's end releases a fetch still waiting, so none outlives its test.
    function scenario(body, work; found = String[], bound = 600)
        mktempdir() do home
            dir = joinpath(home, set.jll)
            release = joinpath(home, "release")
            CE.TOOL_HOME[] = home
            CE.WHICH[] = p -> p in found ? "/sys/bin/$p" : nothing
            CE.TOOL_FETCH[] = (d, s) -> begin
                open(io -> println(io, "x"), joinpath(home, "starts"), "a")
                code = "include(ARGS[1]); FetchTool.main(ARGS[2:end]; install = (dir, args...) -> ($(work(release))))"
                addenv(`$julia -e $code $script $d $(s.jll) $(s.uuid) $(s.compat) $bound $(s.programs)`, "JULIA_LOAD_PATH" => "@:@stdlib")
            end
            starts() = isfile(joinpath(home, "starts")) ? countlines(joinpath(home, "starts")) : 0
            ended() = until(() -> !CE.fetching(dir) && (isfile(joinpath(dir, "failed.txt")) || isfile(joinpath(dir, "copy.txt"))))
            try
                body((; home, dir, release, starts, ended))
            finally
                write(release, "")
                isdir(dir) && until(() -> !CE.fetching(dir))
                CE.WHICH[], CE.TOOL_HOME[], CE.TOOL_FETCH[], CE.FETCH_PACE[] = old
            end
        end
    end
    # A download that waits for the scenario's release, then records a copy of both programs as the real one does.
    waits(release) = """begin
        for _ in 1:600; isfile($(repr(release))) && break; sleep(0.1); end
        for p in ("wid", "get"); write(joinpath(dir, p), ""); end
        write(joinpath(dir, "copy.txt"), "libpath_env=SOT_TEST_LIBS\\nlibpath=/copy/lib\\nwid=\$(joinpath(dir, "wid"))\\nget=\$(joinpath(dir, "get"))\\n")
        nothing
    end"""
    # A download that ends at once with `outcome`, Julia source for the words of a failure, nothing, or a throw.
    ends(outcome) = release -> outcome

    @testset "a system tool on the search path is used and nothing is fetched" begin
        scenario(ends("nothing"); found = ["wid", "get"]) do s
            cmds = CE.find_tools(set)
            @test cmds["wid"] == `/sys/bin/wid` && cmds["get"] == `/sys/bin/get`
            @test s.starts() == 0 && !ispath(s.dir)
        end
    end

    @testset "neither tool: one fetch starts; during it a preview says so and starts no other, in any process" begin
        # One of the pair on the path is no pair: the two come from one build, so Julia's copy holds both.
        scenario(waits; found = ["wid"]) do s
            note = words(CE.find_tools(set))
            @test occursin("not found on this computer", note) && occursin("being downloaded", note)
            @test occursin("9 MB", note) && occursin("once", note) && occursin("Select the file again", note)
            @test s.starts() == 1
            @test until(() -> CE.fetching(s.dir))
            @test occursin("still being downloaded", words(CE.find_tools(set)))
            # A second kernel is a second process with its own lookup, in the same folder.
            code = """
                using ConceptExplorerCore
                CE = ConceptExplorerCore
                set = CE.ToolSet("widget", "Widget_jll", "00000000-0000-0000-0000-000000000001", "1", ["wid", "get"], "9 MB")
                CE.TOOL_HOME[] = ARGS[1]
                CE.WHICH[] = p -> nothing
                CE.TOOL_FETCH[] = (d, s) -> (open(io -> println(io, "x"), joinpath(ARGS[1], "starts"), "a"); `true`)
                print(CE.find_tools(set))
                """
            @test occursin("still being downloaded", read(addenv(`$julia -e $code $(s.home)`, "JULIA_LOAD_PATH" => "@:@stdlib"), String))
            @test s.starts() == 1
            # The fetch ends and its copy is used, the process's library path behind the copy's; a system tool wins.
            write(s.release, "")
            @test s.ended()
            withenv("SOT_TEST_LIBS" => "/own") do
                cmds = CE.find_tools(set)
                @test cmds["wid"].exec == [joinpath(s.dir, "wid")]
                @test "SOT_TEST_LIBS=/copy/lib:/own" in cmds["wid"].env
            end
            @test s.starts() == 1
        end
    end

    @testset "a failed fetch's words reach the next note, and the retry waits for the pace" begin
        scenario(ends("\"the download failed\"")) do s
            @test occursin("being downloaded", words(CE.find_tools(set)))
            @test s.ended()
            @test readline(joinpath(s.dir, "failed.txt")) == "the download failed"
            CE.FETCH_PACE[] = 3600
            for _ in 1:2
                note = words(CE.find_tools(set))
                @test occursin("could not be downloaded: the download failed", note) && occursin("next try is in", note)
            end
            @test s.starts() == 1
            # Past the pace the next preview tries again.
            run(`touch -m -t 200001010000 $(joinpath(s.dir, "failed.txt"))`)
            @test occursin("being downloaded", words(CE.find_tools(set)))
            @test s.starts() == 2
        end
        # An exception inside the download is fixed words; its text stays in the log.
        scenario(ends("error(\"secret-4c1e8a\")")) do s
            CE.find_tools(set)
            @test s.ended()
            @test readline(joinpath(s.dir, "failed.txt")) == "the fetch failed"
            @test occursin("secret-4c1e8a", read(joinpath(s.dir, "fetch.log"), String))
            @test !occursin("secret-4c1e8a", words(CE.find_tools(set)))
        end
    end

    @testset "a fetch is bounded" begin
        scenario(ends("sleep(60)"); bound = 2) do s
            CE.find_tools(set)
            @test s.ended()
            @test readline(joinpath(s.dir, "failed.txt")) == "it did not finish within 2 s"
        end
    end

    @testset "a fetch whose process was killed blocks nothing" begin
        scenario(r -> "begin; write(joinpath(dir, \"pid\"), string(getpid())); $(waits(r)); end") do s
            CE.find_tools(set)
            @test until(() -> isfile(joinpath(s.dir, "pid")) && !isempty(read(joinpath(s.dir, "pid"), String)))
            @test CE.fetching(s.dir)
            run(`kill -9 $(read(joinpath(s.dir, "pid"), String))`)
            # No failure was written, so nothing paces the next try; the dead holder's lock is reclaimed within seconds.
            @test until(() -> !CE.fetching(s.dir))
            @test !isfile(joinpath(s.dir, "failed.txt"))
            @test occursin("being downloaded", words(CE.find_tools(set)))
            @test s.starts() == 2
        end
    end
end
