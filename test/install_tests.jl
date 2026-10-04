# Tests for src/install.jl and src/comm_bin.jl: update_comm reporting and pruning of retired comm scripts.

@testset "update_comm reports an INCOMPLETE install honestly" begin
    mktempdir() do home
        adapters = first(ShipTools.CLAUDE_SKILL_SRCS)
        skill = first(sort([n for n in readdir(adapters)
                            if isfile(joinpath(adapters, n, "SKILL.md"))]))
        stuckdir = joinpath(home, ".claude", "skills", skill, "SKILL.md")
        mkpath(stuckdir)
        write(joinpath(stuckdir, "marker.txt"), "keepme")
        withenv("HOME" => home, "CLAUDE_CONFIG_DIR" => nothing, "CODEX_HOME" => nothing,
                "SOT_COMM_HOME" => joinpath(home, ".sot-comm")) do
            err = try
                ShipTools.update_comm(clis = [:claude]); nothing
            catch e
                e
            end
            @test err isa ErrorException
            @test occursin("INCOMPLETE", err.msg)
            @test occursin(skill, err.msg)
            # No stamp on a partial run, and later stages were not stranded.
            @test !isfile(joinpath(home, ".sot-comm", "VERSION"))
            @test isfile(joinpath(home, ".sot-comm", "bin", "comm-relay.sh"))
        end
    end
end

@testset "install prunes the comm scripts the release no longer ships" begin
    # The source bytes of one shipped bin file, from whichever listed folder holds it.
    srcbytes(name) = read(joinpath(first(d for (d, n) in ShipTools._comm_bin_files() if n == name), name))
    manifest = ".sot-comm-installed"
    withhome(f) = mktempdir() do home
        bin = joinpath(home, ".sot-comm", "bin")
        mkpath(bin)
        withenv("HOME" => home, "CLAUDE_CONFIG_DIR" => nothing, "CODEX_HOME" => nothing,
                "SOT_COMM_HOME" => joinpath(home, ".sot-comm")) do
            f(home, bin)
        end
    end

    @testset "(a) no manifest yet: the frozen list prunes, nothing else" begin
        withhome() do home, bin
            write(joinpath(bin, "comm-listen.sh"), "stale")
            write(joinpath(bin, "bus.sh"), "stale")
            write(joinpath(bin, "comm-wake.sh"), "stale")
            write(joinpath(bin, "comm-poll.sh"), "OLD")
            write(joinpath(bin, "my-own-tool.sh"), "mine")
            ShipTools.update_comm(clis = [:claude])
            @test !isfile(joinpath(bin, "comm-listen.sh"))
            @test !isfile(joinpath(bin, "bus.sh"))
            @test !isfile(joinpath(bin, "comm-wake.sh"))
            @test read(joinpath(bin, "comm-poll.sh")) == srcbytes("comm-poll.sh")
            @test read(joinpath(bin, "my-own-tool.sh"), String) == "mine"
            @test isfile(joinpath(bin, "comm-status-idle.sh"))
            @test isfile(joinpath(bin, manifest))
            @test "comm-poll.sh" in split(read(joinpath(bin, manifest), String), '\n')
        end
    end

    @testset "(b) manifest-driven, no list entry" begin
        withhome() do home, bin
            write(joinpath(bin, "comm-gone.sh"), "stale")
            write(joinpath(bin, manifest), "comm-gone.sh\ncomm-poll.sh\n")
            ShipTools.update_comm(clis = [:claude])
            @test !isfile(joinpath(bin, "comm-gone.sh"))
            @test isfile(joinpath(bin, "comm-poll.sh"))
            @test !("comm-gone.sh" in split(read(joinpath(bin, manifest), String), '\n'))
        end
    end

    @testset "(c) a failed install prunes nothing and keeps the old manifest" begin
        withhome() do home, bin
            adapters = first(ShipTools.CLAUDE_SKILL_SRCS)
            skill = first(sort([n for n in readdir(adapters)
                                if isfile(joinpath(adapters, n, "SKILL.md"))]))
            stuckdir = joinpath(home, ".claude", "skills", skill, "SKILL.md")
            mkpath(stuckdir)
            write(joinpath(bin, "comm-listen.sh"), "stale")
            write(joinpath(bin, "comm-gone.sh"), "stale")
            old = "comm-gone.sh\n"
            write(joinpath(bin, manifest), old)
            @test_throws ErrorException ShipTools.update_comm(clis = [:claude])
            @test read(joinpath(bin, "comm-listen.sh"), String) == "stale"
            @test read(joinpath(bin, "comm-gone.sh"), String) == "stale"
            @test read(joinpath(bin, manifest), String) == old
        end
    end

    @testset "(d) empty record prunes the seed only; a symlink is never removed" begin
        withhome() do home, bin
            write(joinpath(bin, "comm-gone.sh"), "stale")
            write(joinpath(bin, "bus.sh"), "stale")
            write(joinpath(bin, manifest), "")
            target = joinpath(home, "target.txt")
            write(target, "precious")
            symlink(target, joinpath(bin, "comm-listen.sh"))
            ShipTools.update_comm(clis = [:claude])
            @test isfile(joinpath(bin, "comm-gone.sh"))
            @test !isfile(joinpath(bin, "bus.sh"))
            @test islink(joinpath(bin, "comm-listen.sh"))
            @test read(target, String) == "precious"
        end
    end

    @testset "(e) the script the retired bridge loops re-exec is never pruned" begin
        withhome() do home, bin
            write(joinpath(bin, "comm-relay.sh"), "OLD")
            write(joinpath(bin, manifest), "comm-relay.sh\n")
            @test_logs (:info, r"still shipped") match_mode = :any min_level = Base.CoreLogging.Info begin
                ShipTools.update_comm(clis = [:claude])
            end
            @test read(joinpath(bin, "comm-relay.sh")) == srcbytes("comm-relay.sh")
        end
    end
end

@testset "bin folders" begin
    mktempdir() do root
        mkpath(joinpath(root, "a", "sub"))
        mkpath(joinpath(root, "b"))
        write(joinpath(root, "a", "x.sh"), "x")
        write(joinpath(root, "b", "y.sh"), "y")
        list = joinpath(root, "list.txt")
        # A CRLF list with a blank line: CR is stripped, blanks skipped.
        write(list, "a\r\n\r\nb\r\n")
        folders = ShipTools._comm_bin_folders(list, root)
        @test folders == [joinpath(root, "a"), joinpath(root, "b")]
        files = ShipTools._comm_bin_files(folders)
        @test [n for (_, n) in files] == ["x.sh", "y.sh"]
        @test ShipTools._comm_bin_shipped(files) == ["x.sh", "y.sh"]

        # A listed folder that is not there fails, naming it.
        write(list, "a\nmissing\n")
        err = try ShipTools._comm_bin_folders(list, root); nothing catch e; e end
        @test err isa ErrorException
        @test occursin("missing", err.msg)

        # One name in two folders fails, naming both.
        write(joinpath(root, "b", "x.sh"), "dup")
        err = try ShipTools._comm_bin_files([joinpath(root, "a"), joinpath(root, "b")]); nothing catch e; e end
        @test err isa ErrorException
        @test occursin("x.sh", err.msg)
        @test occursin(joinpath(root, "a"), err.msg) && occursin(joinpath(root, "b"), err.msg)
    end
end
