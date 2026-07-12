using Test
using JSON3
using ShipToolsRepl

# Reach the non-exported streaming internals under test.
const DR = ShipToolsRepl

@testset "ShipToolsRepl streaming" begin

    @testset "utf8_prefix splits on char boundary" begin
        # "é" is 0xC3 0xA9. A buffer ending mid-char must hold the partial byte
        # back so we never emit invalid UTF-8 (JSON3 would reject it).
        full = Vector{UInt8}("aé")              # [0x61, 0xC3, 0xA9]
        s, rest = DR.utf8_prefix(full)
        @test s == "aé"
        @test isempty(rest)

        partial = full[1:2]                     # "a" + leading byte of é
        s2, rest2 = DR.utf8_prefix(partial)
        @test s2 == "a"
        @test rest2 == UInt8[0xC3]
        # Completing the char emits it.
        s3, rest3 = DR.utf8_prefix(vcat(rest2, UInt8[0xA9]))
        @test s3 == "é"
        @test isempty(rest3)
    end

    @testset "value_frames_for: text/plain for a plain value" begin
        frames = DR.value_frames_for(42)
        @test length(frames) == 1
        @test frames[1][:kind] == "value"
        @test frames[1][:mime] == "text/plain"
        @test strip(frames[1][:text]) == "42"
    end

    @testset "value_frames_for: BrowserView emits a browser frame (ADR 0032)" begin
        url = "http://127.0.0.1:1237/browser-display/abcd"
        frames = DR.value_frames_for(DR.BrowserView(url))
        @test length(frames) == 1
        @test frames[1][:kind] == "browser"
        @test frames[1][:url] == url
        # browserview() is the exported constructor and round-trips identically.
        @test DR.value_frames_for(DR.browserview(url)) == frames
    end

    @testset "stream_eval_frames: stdout then value, in order" begin
        frames = Dict[]
        DR.stream_eval_frames(f -> push!(frames, f)) do
            print("hello")
            21 + 21
        end
        kinds = [f[:kind] for f in frames]
        @test "stdout" in kinds
        @test last(kinds) == "value"          # value comes after stdout
        sout = join(f[:text] for f in frames if f[:kind] == "stdout")
        @test occursin("hello", sout)
        valf = frames[findlast(f -> f[:kind] == "value", frames)]
        @test strip(valf[:text]) == "42"
    end

    @testset "stream_eval_frames: error frame on throw" begin
        frames = Dict[]
        DR.stream_eval_frames(f -> push!(frames, f)) do
            error("boom")
        end
        errs = filter(f -> f[:kind] == "error", frames)
        @test length(errs) == 1
        @test occursin("boom", errs[1][:message])
        @test !isempty(errs[1][:stacktrace])    # captured a backtrace
    end

    @testset "discover_project walks up to Project.toml" begin
        dir, toml, source = DR.discover_project(@__FILE__)
        @test source == :discovered
        @test isfile(joinpath(dir, "Project.toml"))
    end

    # ---- end-to-end: drive serve() over in-memory streams --------------------
    # serve runs the eval on an @async task and streams repl.frame evts, then a
    # terminal res. We feed NDJSON requests and read the envelopes back.
    function drive(requests::Vector{String}; idle_timeout = 20.0)
        bs_in = Base.BufferStream()
        bs_out = Base.BufferStream()
        srv = @async DR.serve(bs_in, bs_out)
        # Watchdog: if serve wedges, unblock the reader so the test fails loud
        # instead of hanging.
        @async begin
            sleep(idle_timeout)
            close(bs_out)
            close(bs_in)
        end
        return bs_in, bs_out, srv
    end

    function read_until_res(bs_out)
        envs = Any[]
        while true
            line = readline(bs_out)            # "" on EOF (watchdog close)
            isempty(line) && (eof(bs_out) ? break : continue)
            env = JSON3.read(line)
            push!(envs, env)
            get(env, :kind, "") == "res" && break
        end
        return envs
    end

    @testset "serve: repl.eval streams frames + terminal res" begin
        bs_in, bs_out, _ = drive(String[])
        req = JSON3.write(Dict(
            :v => 1, :id => 7, :op => "repl.eval",
            :payload => Dict(:eval_id => 99, :code => "print(\"hi\"); 1+2"),
        ))
        write(bs_in, req * "\n")
        flush(bs_in)
        envs = read_until_res(bs_out)
        close(bs_in)

        evts = [e for e in envs if get(e, :kind, "") == "evt" && e.op == "repl.frame"]
        @test !isempty(evts)
        # every evt is correlated to the request + eval
        @test all(e -> e.id == 7, evts)
        @test all(e -> e.payload.eval_id == 99, evts)
        framekinds = [e.payload.frame.kind for e in evts]
        @test "stdout" in framekinds
        @test "value" in framekinds
        @test last(framekinds) == "done"       # done is the terminal frame
        # terminal res ack
        res = envs[end]
        @test res.kind == "res"
        @test res.op == "repl.eval"
        @test res.payload.eval_id == 99
    end

    @testset "serve: repl.interrupt cancels a running eval" begin
        bs_in, bs_out, _ = drive(String[])
        # A long, yielding eval so the dispatch loop stays responsive to interrupt.
        evalreq = JSON3.write(Dict(
            :v => 1, :id => 1, :op => "repl.eval",
            :payload => Dict(:eval_id => 1, :code => "sleep(60)"),
        ))
        write(bs_in, evalreq * "\n"); flush(bs_in)
        # Let the eval task actually start before interrupting.
        sleep(1.0)
        intreq = JSON3.write(Dict(
            :v => 1, :id => 2, :op => "repl.interrupt", :payload => Dict(),
        ))
        write(bs_in, intreq * "\n"); flush(bs_in)

        # Collect envelopes until we see the eval's done frame (id 1) AND the
        # interrupt res (id 2).
        saw_interrupt_res = false
        saw_eval_error = false
        saw_eval_done = false
        deadline = time() + 15
        while time() < deadline && !(saw_eval_done && saw_interrupt_res)
            line = readline(bs_out)
            isempty(line) && (eof(bs_out) ? break : continue)
            env = JSON3.read(line)
            if get(env, :kind, "") == "res" && env.op == "repl.interrupt"
                saw_interrupt_res = true
                @test env.payload.interrupted == true
            elseif get(env, :kind, "") == "evt" && env.op == "repl.frame" && env.id == 1
                k = env.payload.frame.kind
                k == "error" && (saw_eval_error = true)
                k == "done" && (saw_eval_done = true)
            end
        end
        close(bs_in)
        @test saw_interrupt_res
        @test saw_eval_error        # InterruptException surfaced as an error frame
        @test saw_eval_done         # eval terminated with a done frame
    end
end
