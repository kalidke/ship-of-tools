# Tests for src/install.jl and src/comm_bin.jl: update_comm reporting, pruning, the one-file library and sot-fe, the refusal, and install_comm's publish order and failures.

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
        write(joinpath(root, "a", "CLAUDE.md"), "page")
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

@testset "update_comm installs no CLAUDE.md" begin
    mktempdir() do home
        withenv("HOME" => home, "CLAUDE_CONFIG_DIR" => nothing, "CODEX_HOME" => nothing,
                "SOT_COMM_HOME" => joinpath(home, ".sot-comm")) do
            ShipTools.update_comm(clis = [:claude, :codex])
        end
        @test isfile(joinpath(home, ".sot-comm", "bin", "comm-poll.sh"))
        found = [joinpath(r, f) for (r, _, fs) in walkdir(home) for f in fs if f == ShipTools.NEVER_INSTALLED]
        @test isempty(found)
    end
end

@testset "the library publishes first, and a script started after any rename sources it whole" begin
    files = ShipTools._comm_bin_files()
    lib = first(f for (f, n) in files if n == "comm-lib.sh")
    # With every part inside the file that sources it, comm-lib.sh is the one file a script sources from
    # another, so publishing it first is the whole order rule: no new script runs against the old library.
    @test first(files) == (lib, "comm-lib.sh")
    mktempdir() do dir
        bin = mkpath(joinpath(dir, "bin"))
        home = mkpath(joinpath(dir, "home"))
        # The previous release's bin: the same names, every one older than this tree's.
        foreach(((f, n),) -> write(joinpath(bin, n), n == "comm-lib.sh" ? "old=1\n" : "# previous release\n"), files)
        published = String[]
        bad = String[]
        unset = [k => nothing for k in keys(ENV) if startswith(k, "SOT_")]
        withenv(unset..., "HOME" => home, "SOT_COMM_HOME" => joinpath(home, ".sot-comm")) do
            # install_comm's own loop; after each rename it makes, a script starts and sources the library.
            function rename(src, dst)
                Base.Filesystem.rename(src, dst)
                push!(published, basename(dst))
                Sys.isunix() && !success(`bash -c 'source "$1/comm-lib.sh"' _ $bin`) &&
                    push!(bad, "after $(basename(dst)): comm-lib.sh does not source")
            end
            problems = String[]
            ShipTools._publish_comm_bin!(problems, bin; rename = rename)
            @test isempty(problems)
        end
        @test first(published) == "comm-lib.sh"
        @test sort(published) == sort(last.(files))
        @test bad == String[]
    end
end

@testset "comm-lib.sh and sot-fe install as one file each, and comm-lib.sh defines what the loader defines" begin
    files = ShipTools._comm_bin_files()
    folder_of = Dict(n => f for (f, n) in files)
    parts_of = Dict{String,Vector{String}}()
    for entry in ("comm-lib.sh", "sot-fe")
        f = folder_of[entry]
        parts = [p for p in (ShipTools._comm_part_of(f, l) for l in eachline(joinpath(f, entry))) if p !== nothing]
        parts_of[entry] = parts
        @test !isempty(parts)
        @test !any(p -> haskey(folder_of, p), parts)        # a part does not install on its own
        text = ShipTools._comm_bin_text(f, entry)
        @test all(p -> occursin(read(joinpath(f, p), String), text), parts)
        @test all(l -> ShipTools._comm_part_of(f, l) === nothing, eachline(IOBuffer(text)))
    end
    if Sys.isunix()
        mktempdir() do home
            unset = [k => nothing for k in keys(ENV) if startswith(k, "SOT_")]
            withenv(unset..., "HOME" => home, "SOT_COMM_HOME" => joinpath(home, ".sot-comm"),
                    "CLAUDE_CONFIG_DIR" => nothing, "CODEX_HOME" => nothing) do
                # A bin an install from this repo's split layout left: the parts as files, and recorded.
                bin = mkpath(joinpath(home, ".sot-comm", "bin"))
                parts = vcat(values(parts_of)...)
                foreach(p -> write(joinpath(bin, p), "# previous release\n"), parts)
                write(joinpath(bin, ShipTools.COMM_MANIFEST), join(parts, "\n") * "\n")
                ShipTools.update_comm(clis = [:claude])
                @test !any(p -> ispath(joinpath(bin, p)), parts)
                # The functions and globals a script gets from the installed file are the repo loader's.
                probe = raw"""b0="$(compgen -v | sort)"; source "$1" >/dev/null || exit 9; declare -f
                    for v in $(compgen -v | sort | comm -13 <(printf "%s\n" "$b0") -); do declare -p "$v"; done"""
                defined(lib) = read(`bash -c $probe _ $lib`, String)
                @test defined(joinpath(bin, "comm-lib.sh")) == defined(joinpath(folder_of["comm-lib.sh"], "comm-lib.sh"))
                # sot-fe reaches its dispatch: every part it sourced is in the one file.
                @test occursin("sot-fe", read(Cmd(`bash $(joinpath(bin, "sot-fe")) help`; dir = home), String))
                # A script runs on the installed library.
                withenv("SOT_COMM_TEST_HOST" => "pin-host") do
                    out = read(Cmd(`bash $(joinpath(bin, "comm-context.sh"))`; dir = home), String)
                    @test occursin(r"(?m)^HOST=pin-host$", out)
                end
            end
        end
    end
end

@testset "a source command the installer cannot install whole stops the install" begin
    mktempdir() do root
        lib = mkpath(joinpath(root, "lib"))
        app = mkpath(joinpath(root, "app"))
        loader = "x=1\nsource \"\$(dirname \"\${BASH_SOURCE[0]}\")/lib-a.sh\" || return 1\n"
        write(joinpath(lib, "lib.sh"), loader)
        write(joinpath(lib, "lib-a.sh"), "a() { :; }")       # no final newline: the installed text adds one
        write(joinpath(app, "app.sh"), "source \"\$SCRIPT_DIR/lib.sh\"\n")
        @test ShipTools._comm_bin_files([lib, app]) == [(lib, "lib.sh"), (app, "app.sh")]
        @test ShipTools._comm_bin_text(lib, "lib.sh") == "x=1\na() { :; }\n"
        # A CRLF checkout: the same lines match, and the bytes are kept.
        write(joinpath(lib, "lib.sh"), replace(loader, "\n" => "\r\n"))
        @test ShipTools._comm_bin_files([lib, app]) == [(lib, "lib.sh"), (app, "app.sh")]
        @test ShipTools._comm_bin_text(lib, "lib.sh") == "x=1\r\na() { :; }\n"
        write(joinpath(lib, "lib.sh"), loader)
        # Write `text` into `path`, and say whether `_comm_bin_files` then refuses the folders.
        function refused(path, text)
            old = read(path, String)
            write(path, text)
            try
                ShipTools._comm_bin_files([lib, app])
                return false
            catch e
                return e isa ErrorException && occursin("cannot install", e.msg)
            finally
                write(path, old)
            end
        end
        # A part sourced by any command but its own file's part line, wherever on the line it stands.
        for text in ("source \"\$SCRIPT_DIR/lib-a.sh\"\n",
                     "x=\"\$( . \"\$SCRIPT_DIR/lib-a.sh\"; a )\"\n",
                     "row=\"\$( ( . \"\$(dirname \"\${BASH_SOURCE[0]}\")/lib-a.sh\" || exit 2; a ) )\"\n",
                     "if source \"\$SCRIPT_DIR/lib-a.sh\"; then a; fi\n",
                     "[ -r \"\$D/lib-a.sh\" ] && . \"\$D/lib-a.sh\"\n")
            @test refused(joinpath(app, "app.sh"), text)
        end
        @test refused(joinpath(lib, "lib.sh"), loader * ". \"\$(dirname \"\$0\")/lib-a.sh\"\n")
        @test refused(joinpath(lib, "lib.sh"), loader * "true && . \"\$(dirname \"\${BASH_SOURCE[0]}\")/lib-a.sh\"\n")
        # A part that sources a file.
        @test refused(joinpath(lib, "lib-a.sh"), "source \"\$SCRIPT_DIR/app.sh\"\n")
        # Only the command's path is read: a comment naming a part is no refusal.
        @test !refused(joinpath(app, "app.sh"), "source \"\$SCRIPT_DIR/lib.sh\"   # a() is in lib-a.sh\n")
        # A comment line holds no command, whatever its prose looks like.
        @test !refused(joinpath(app, "app.sh"), "source \"\$SCRIPT_DIR/lib.sh\"\n    # reads `x`. lib-a.sh has it; (. lib-a.sh)\n")
        # Two files may source one part: each holds its text.
        write(joinpath(lib, "lib2.sh"), loader)
        @test ShipTools._comm_bin_files([lib, app]) == [(lib, "lib.sh"), (lib, "lib2.sh"), (app, "app.sh")]
        @test ShipTools._comm_bin_text(lib, "lib2.sh") == "x=1\na() { :; }\n"
    end
end

@testset "a file the scan cannot read stays listed, with every other file of its folder" begin
    mktempdir() do root
        lib = mkpath(joinpath(root, "lib"))
        app = mkpath(joinpath(root, "app"))
        write(joinpath(lib, "lib.sh"), "x=1\n")
        write(joinpath(app, "app.sh"), "y=1\n")
        files = [(lib, "lib.sh"), (app, "app.sh")]
        # The scan lists a file it cannot read (where mode bits are enforced), so only that file's folder fails.
        chmod(joinpath(app, "app.sh"), 0o000)
        unreadable = try read(joinpath(app, "app.sh")); false catch; true end
        unreadable && @test ShipTools._comm_bin_files([lib, app]) == files
        # A folder with two files, one unreadable at the scan: both names stay listed. A shorter list would
        # let the install's prune delete the folder's other scripts from the bin and record the shorter list,
        # when the file reads again at its folder's publish and the install succeeds.
        write(joinpath(app, "app2.sh"), "z=1\n")
        unreadable && @test ShipTools._comm_bin_files([lib, app]) == [(lib, "lib.sh"), (app, "app.sh"), (app, "app2.sh")]
        chmod(joinpath(app, "app.sh"), 0o644)
        @test ShipTools._comm_bin_files([lib, app]) == [(lib, "lib.sh"), (app, "app.sh"), (app, "app2.sh")]
    end
end

@testset "after the library's folder records a problem, no other folder publishes" begin
    mktempdir() do root
        bin = mkpath(joinpath(root, "bin"))
        names = last.(ShipTools._comm_bin_files())
        refuse(name) = (src, dst) -> basename(dst) == name ? error("refused") : Base.Filesystem.rename(src, dst)
        problems = String[]
        files = ShipTools._publish_comm_bin!(problems, bin; rename = refuse("comm-lib.sh"))
        @test length(problems) == 2 && occursin("(not published: the library's folder recorded a problem)", problems[2])
        @test sort(last.(files)) == sort(names)
        @test isempty(readdir(bin))
        # A later folder's problem is its own: the others publish.
        problems = String[]
        ShipTools._publish_comm_bin!(problems, bin; rename = refuse("comm-send.sh"))
        @test length(problems) == 1
        @test sort(readdir(bin)) == sort(filter(!=("comm-send.sh"), names))
    end
end
