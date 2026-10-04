# Tests for src/install.jl and src/comm_bin.jl: update_comm reporting, pruning, the one-file library and sot-fe, and an install replayed step by step.

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

@testset "an install shows a script one whole library at every step" begin
    files = ShipTools._comm_bin_files()      # what the installer publishes, in its order
    folder_of = Dict(n => f for (f, n) in files)
    lib = folder_of["comm-lib.sh"]
    # Every file of the listed folders, as the repo lays them out (parts included).
    repo = [(d, n) for d in ShipTools._comm_bin_folders() for n in readdir(d)
            if isfile(joinpath(d, n)) && n != ShipTools.NEVER_INSTALLED]
    # A `source` or `.` line whose quoted path ends in a name `known` holds.
    source_line = r"^\s*(?:source|\.)\s.*/([A-Za-z0-9._-]+)\""
    sourced(path, known) = [String(m[1]) for m in (match(source_line, l) for l in eachline(path))
                            if m !== nothing && m[1] in known]
    # Files a file of their own folder sources: the release before the split shipped them inside it.
    repo_names = Set(last.(repo))
    repo_folder = Dict(n => d for (d, n) in repo)
    split_parts = Set(s for (d, n) in repo for s in sourced(joinpath(d, n), repo_names) if repo_folder[s] == d)
    marker(n) = "_sot_pin_old_" * replace(n, r"[^A-Za-z0-9]" => "_")
    # The release before the split: its names, and comm-lib.sh one whole file.
    function previous_release!(bin)
        for (d, n) in repo
            n in split_parts && continue
            write(joinpath(bin, n), n == "comm-lib.sh" ? "$(marker(n))=1\n" : "# previous release\n")
        end
        return 1
    end
    # This repo's layout one release older, as an install that copied each file would leave it.
    function previous_layout!(bin)
        for (d, n) in repo
            tail = d == lib ? "\n$(marker(n))=1\n" : "\n# previous release\n"
            write(joinpath(bin, n), read(joinpath(d, n), String) * tail)
        end
        return count(p -> first(p) == lib, repo)
    end
    # Publish what the installer publishes, in its order, and before the first file and after each one
    # check what a script started at that moment sees. One line per file that is ever ahead of what it
    # sources, and one per step at which comm-lib.sh does not source whole.
    function replay(old!)
        bad = String[]
        mktempdir() do dir
            stage = mkpath(joinpath(dir, "stage"))
            bin = mkpath(joinpath(dir, "bin"))
            home = mkpath(joinpath(dir, "home"))
            foreach(((f, n),) -> ShipTools._comm_bin_stage(f, n, stage), files)
            names = Set(last.(files))
            graph = Dict(n => sourced(joinpath(stage, n), names) for (f, n) in files)
            # The scan finds what the scripts source, so the checks below are not vacuous.
            "comm-lib.sh" in graph["comm-despawn.sh"] && "comm-lib.sh" in graph["sot-fe"] ||
                push!(bad, "the source scan found nothing")
            function closure(n)
                seen = Set{String}()
                todo = copy(graph[n])
                while !isempty(todo)
                    s = pop!(todo)
                    s in seen && continue
                    push!(seen, s)
                    append!(todo, graph[s])
                end
                return seen
            end
            nold = old!(bin)
            unset = [k => nothing for k in keys(ENV) if startswith(k, "SOT_")]
            ahead = Dict{String,Tuple{Int,Int,Vector{String}}}()   # file => (first step, last step, missing then)
            withenv(unset..., "HOME" => home, "SOT_COMM_HOME" => joinpath(home, ".sot-comm")) do
                done = Set{String}()
                for k in 0:length(files)
                    if k > 0
                        n = last(files[k])
                        ShipTools.install_file(joinpath(stage, n), joinpath(bin, n))
                        push!(done, n)
                    end
                    # A script that sources comm-lib.sh now gets one whole library: all old or all new.
                    if Sys.isunix()
                        out = IOBuffer()
                        probe = `bash -c 'source "$1/comm-lib.sh" || exit 9; compgen -v _sot_pin_old_ || true'
                                 _ $bin`
                        p = run(pipeline(ignorestatus(probe); stdout = out, stderr = devnull))
                        marks = split(String(take!(out)), '\n'; keepempty = false)
                        p.exitcode == 0 && length(marks) in (0, nold) ||
                            push!(bad, "step $k: comm-lib.sh sources with exit $(p.exitcode), " *
                                       "$(length(marks)) of $nold files old")
                    end
                    # A file published by now finds everything it sources published too.
                    for p in done
                        missing = sort([s for s in closure(p) if !(s in done)])
                        isempty(missing) && continue
                        ahead[p] = haskey(ahead, p) ? (ahead[p][1], k, ahead[p][3]) : (k, k, missing)
                    end
                end
            end
            for (p, (a, z, m)) in sort(collect(ahead); by = x -> x[2][1])
                push!(bad, "steps $a-$z: $p is new before $(join(m, " "))")
            end
        end
        return bad
    end
    @test replay(previous_release!) == String[]
    @test replay(previous_layout!) == String[]
end

@testset "comm-lib.sh and sot-fe install as one file each, defining what the repo's files define" begin
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

@testset "a source line the installer cannot install whole stops the install" begin
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
                return e isa ErrorException && (occursin("cannot install", e.msg) || occursin("sourced by both", e.msg))
            finally
                write(path, old)
            end
        end
        # A part sourced in a form the installer does not inline, from its own folder or another.
        @test refused(joinpath(lib, "lib.sh"), loader * ". \"\$(dirname \"\$0\")/lib-a.sh\"\n")
        @test refused(joinpath(app, "app.sh"), "source \"\$SCRIPT_DIR/lib-a.sh\"\n")
        # A part that sources a file, and a part two files source.
        @test refused(joinpath(lib, "lib-a.sh"), "source \"\$SCRIPT_DIR/app.sh\"\n")
        write(joinpath(lib, "lib2.sh"), loader)
        @test refused(joinpath(lib, "lib2.sh"), loader)
    end
end
