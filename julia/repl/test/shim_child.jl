# The shim as the daemon starts it: a child process running `run_shim`, with a scratch depot first on
# `JULIA_DEPOT_PATH` whose config/startup.jl is the user's startup file.
using Test
using ShipToolsRepl

# Start the shim with `startup` as the user's startup file, send each of `codes` as an eval (`pauses[i]` seconds after
# eval i, for a task it started to run; `files` are written under the depot, and `@DEPOT@` in the startup file is its
# path), and return its envelopes per eval, every wire line, and its stderr.
function shim_child(startup, codes; pauses = Dict{Int,Float64}(), files = Dict{String,String}())
    depot = mktempdir()
    mkpath(joinpath(depot, "config"))
    for (rel, text) in files
        mkpath(dirname(joinpath(depot, rel)))
        write(joinpath(depot, rel), text)
    end
    write(joinpath(depot, "config", "startup.jl"), replace(startup, "@DEPOT@" => depot))
    script = "using ShipToolsRepl; ShipToolsRepl.run_shim(stdin, stdout)"
    cmd = addenv(`$(Base.julia_cmd()) --startup-file=no --project=$(pkgdir(ShipToolsRepl)) -e $script`,
        "JULIA_DEPOT_PATH" => join([depot, ""], Sys.iswindows() ? ';' : ':'))
    errpath = tempname()
    proc = open(pipeline(cmd; stderr = errpath), "r+")
    watchdog = @async (sleep(180); kill(proc))
    lines = String[]
    got = Bool[]
    for (i, code) in enumerate(codes)
        write(proc, eval_line(i, code))
        flush(proc)
        got_res = false
        while !got_res && !eof(proc)
            line = readline(proc)
            push!(lines, line)
            got_res = occursin("\"kind\":\"res\"", line)
        end
        push!(got, got_res)
        sleep(get(pauses, i, 0.0))
    end
    close(proc.in)
    wait(proc)
    stderr_text = read(errpath, String)
    rm(errpath; force = true)
    rm(depot; recursive = true, force = true)
    envs = Any[try ShipToolsRepl.json_read(l) catch; l end for l in lines]
    ok = all(e -> e isa AbstractDict, envs) || (println(stderr, "child stderr:\n", stderr_text); false)
    filter!(e -> e isa AbstractDict, envs)
    frames(id) = [e[:payload][:frame] for e in envs if get(e, :op, "") == "repl.frame" && e[:id] == id]
    return (; got, ok, envs, frames, stderr_text)
end

text_of(frames, kind) = join([f[:text] for f in frames if f[:kind] == kind])

@testset "run_shim: a child process, its startup file, stdout and the display stack" begin
    startup = """
        println("startup-line"); display(7)
        struct StartupErr end
        Base.showerror(io::IO, ::StartupErr) = print(io, "custom-startup-showerror")
        """
    codes = [
        "println(\"before\"); display(42); println(\"after\")",
        # A task's display and println land after the eval that started it has ended.
        "@async (sleep(0.3); display(42); println(\"late\")); nothing",
        "println(\"x\"); 2",
        "throw(StartupErr())",
        "struct EvalErr end\nBase.showerror(io::IO, ::EvalErr) = print(io, \"custom-eval-showerror\")",
        "throw(EvalErr())",
        "(Base.Multimedia.displays[1] isa TextDisplay, Base.Multimedia.displays[1].io isa ShipToolsRepl.EvalStdout," *
            " findfirst(d -> d isa ShipToolsRepl.WGLDisplay, Base.Multimedia.displays) > 1)",
    ]
    c = shim_child(startup, codes; pauses = Dict(2 => 1.0))
    @test all(c.got)
    # every line the child wrote is one envelope (a raw `42` parses as a number), and the ready line comes first
    @test c.ok
    @test c.envs[1][:op] == "repl.ready"
    # the startup file ran with stdout on stderr: its print and its display are lines of their own
    err_lines = split(c.stderr_text, '\n')
    @test "startup-line" in err_lines
    @test "7" in err_lines
    @test text_of(c.frames(1), "stdout") == "before\n42\nafter\n"
    @test text_of(c.frames(3), "stdout") == "x\n"
    # an error frame uses the `showerror` the startup file or an earlier eval defined
    for id in (4, 6)
        errs = [f for f in c.frames(id) if f[:kind] == "error"]
        @test length(errs) == 1
        @test errs[1][:message] == (id == 4 ? "custom-startup-showerror" : "custom-eval-showerror")
    end
    # the stack's bottom entry is a real TextDisplay (Plots, Makie and Bonito look for it by type), on the eval's
    # stdout, and the shim's own display sits above it
    vals = [f[:text] for f in c.frames(7) if f[:kind] == "value"]
    @test vals == ["(true, true, true)"]
end

@testset "run_shim: a startup file that throws is said once at the first eval, and the shim serves" begin
    c = shim_child("println(\"before-boom\"); error(\"boom\")", ["1 + 1", "2 + 2"])
    @test all(c.got)
    @test c.ok
    first_frame = first(c.frames(1))
    @test first_frame[:kind] == "stderr"
    @test occursin("the startup file", first_frame[:text])
    @test occursin("startup.jl", first_frame[:text])
    @test occursin("boom", first_frame[:text])
    @test [f[:text] for f in c.frames(1) if f[:kind] == "value"] == ["2"]
    @test !any(f -> f[:kind] == "stderr", c.frames(2))
    @test "before-boom" in split(c.stderr_text, '\n')
end

@testset "run_shim: a Revise the startup file loaded revises before each eval, and a failed revision is a stderr frame" begin
    # A stdlib-only stand-in package named Revise, found through LOAD_PATH like the real one.
    standin = """
        module Revise
        const calls = Ref(0)
        function revise()
            calls[] += 1
            calls[] == 2 && @error "revise-logged-failure"
            calls[] == 3 && error("revise-threw")
            return nothing
        end
        end
        """
    c = shim_child("push!(LOAD_PATH, raw\"@DEPOT@/stand\")\nusing Revise\n", fill("Revise.calls[]", 3);
        files = Dict("stand/Revise/src/Revise.jl" => standin))
    @test all(c.got)
    @test c.ok
    for id in 1:3
        @test [f[:text] for f in c.frames(id) if f[:kind] == "value"] == [string(id)]
    end
    @test !any(f -> f[:kind] == "stderr", c.frames(1))
    @test any(f -> f[:kind] == "stderr" && occursin("revise-logged-failure", f[:text]), c.frames(2))
    @test any(f -> f[:kind] == "stderr" && occursin("revise-threw", f[:text]), c.frames(3))
end
