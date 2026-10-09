using Test
using Pkg
using Sockets
using ShipToolsRepl

# Reach the non-exported streaming internals under test.
const DR = ShipToolsRepl

include("answer_then_next.jl")

@testset "ShipToolsRepl streaming" begin

    @testset "utf8_prefix splits on char boundary" begin
        # "é" is 0xC3 0xA9. A buffer ending mid-char must hold the partial byte
        # back so we never emit invalid UTF-8 (json_write would serialize it
        # as garbage on the other side of the pipe).
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

    @testset "a BrowserView shows its origin, never its secret path (decision 0031)" begin
        # A wrapped BrowserView is rendered as text like any value; that text reaches logs and sot-fe output.
        bv = DR.BrowserView("http://127.0.0.1:41234/0123456789abcdef0123456789abcdef", false)
        for s in (repr(bv), string(bv), sprint(show, MIME"text/plain"(), bv), repr((bv,)),
                  sprint(show, MIME"text/plain"(), [bv]), sprint(show, MIME"text/plain"(), Dict(:fig => bv)))
            @test !occursin("0123456789abcdef", s)
            @test occursin("127.0.0.1:41234", s)
        end
        @test all(f -> !occursin("0123456789abcdef", string(get(f, :text, ""))), DR.value_frames_for((bv,)))
        # A userinfo is dropped whole, even one with a raw `@` in it.
        @test repr(DR.BrowserView("https://alice@password@127.0.0.1:41234/x", false)) ==
              "BrowserView(https://127.0.0.1:41234/…, open = false)"
    end

    @testset "value_frames_for: BrowserView emits a browser frame (ADR 0032)" begin
        url = "http://127.0.0.1:1237/browser-display/abcd"
        frames = DR.value_frames_for(DR.BrowserView(url))
        @test length(frames) == 1
        @test frames[1][:kind] == "browser"
        @test frames[1][:url] == url
        # browserview() is the exported constructor and round-trips identically.
        @test DR.value_frames_for(DR.browserview(url)) == frames
        # Auto-open is on by default; `open = false` (serve-only, or opened on one named
        # frontend with `open = "<name>"`) rides the frame so front-ends can
        # skip the broadcast browser-open.
        @test frames[1][:open] === true
        no_open = DR.value_frames_for(DR.browserview(url; open = false))
        @test length(no_open) == 1
        @test no_open[1][:open] === false
        @test no_open[1][:url] == url
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
            env = DR.json_read(line)
            push!(envs, env)
            get(env, :kind, "") == "res" && break
        end
        return envs
    end

    @testset "serve: repl.eval streams frames + terminal res" begin
        bs_in, bs_out, _ = drive(String[])
        req = sprint(DR.json_write, Dict(
            :v => 1, :id => 7, :op => "repl.eval",
            :payload => Dict(:eval_id => 99, :code => "print(\"hi\"); 1+2"),
        ))
        write(bs_in, req * "\n")
        flush(bs_in)
        envs = read_until_res(bs_out)
        close(bs_in)

        evts = [e for e in envs if get(e, :kind, "") == "evt" && e[:op] == "repl.frame"]
        @test !isempty(evts)
        # every evt is correlated to the request + eval
        @test all(e -> e[:id] == 7, evts)
        @test all(e -> e[:payload][:eval_id] == 99, evts)
        framekinds = [e[:payload][:frame][:kind] for e in evts]
        @test "stdout" in framekinds
        @test "value" in framekinds
        @test last(framekinds) == "done"       # done is the terminal frame
        # terminal res ack
        res = envs[end]
        @test res[:kind] == "res"
        @test res[:op] == "repl.eval"
        @test res[:payload][:eval_id] == 99
    end

    @testset "serve: run_file missing-file failure is VISIBLE (error+done frames)" begin
        # The res-only error was silently dropped for fire-and-forget runs
        # (the supervisor drops untracked res acks by design), making a
        # missing file indistinguishable from a run that never happened —
        # the 2026-07-24 "--fresh include never runs" field failure. The
        # failure must stream as error+done frames like any other eval error.
        bs_in, bs_out, _ = drive(String[])
        req = sprint(DR.json_write, Dict(
            :v => 1, :id => 9, :op => "repl.run_file",
            :payload => Dict(:eval_id => 77, :path => "/nonexistent/nope.jl"),
        ))
        write(bs_in, req * "\n")
        flush(bs_in)
        envs = read_until_res(bs_out)
        close(bs_in)
        evts = [e for e in envs if get(e, :kind, "") == "evt" && e[:op] == "repl.frame"]
        framekinds = [e[:payload][:frame][:kind] for e in evts]
        @test "error" in framekinds
        @test last(framekinds) == "done"
        @test all(e -> e[:payload][:eval_id] == 77, evts)
        res = envs[end]
        @test res[:kind] == "res" && res[:op] == "repl.run_file"
        @test occursin("no such file", String(res[:payload][:error]))
    end

    @testset "serve: run_file include announces a browser frame mid-include" begin
        # A --fresh run reaches the shim as a fresh=false run_file after the
        # daemon bounced the child; the include runs INSIDE stream_eval_frames,
        # so a serve in the file (wglshow / a wrapper that swallows the return
        # value) must stream its browser frame like any mid-eval announce.
        path = tempname() * ".jl"
        write(path, """
            ShipToolsRepl.announce_browserview(ShipToolsRepl.BrowserView("http://127.0.0.1:59994/"))
            println("served")
            nothing
            """)
        bs_in, bs_out, _ = drive(String[])
        req = sprint(DR.json_write, Dict(
            :v => 1, :id => 11, :op => "repl.run_file",
            :payload => Dict(:eval_id => 78, :path => path, :fresh => false),
        ))
        write(bs_in, req * "\n")
        flush(bs_in)
        envs = read_until_res(bs_out)
        close(bs_in)
        rm(path; force = true)
        evts = [e for e in envs if get(e, :kind, "") == "evt" && e[:op] == "repl.frame"]
        framekinds = [e[:payload][:frame][:kind] for e in evts]
        @test "browser" in framekinds
        @test "stdout" in framekinds
        @test last(framekinds) == "done"
        bf = evts[findfirst(==("browser"), framekinds)][:payload][:frame]
        @test bf[:url] == "http://127.0.0.1:59994/" && bf[:open] == true
        @test all(e -> e[:payload][:eval_id] == 78, evts)
    end

    eval_line(id, code) = sprint(DR.json_write, Dict(:v => 1, :id => id, :op => "repl.eval",
        :payload => Dict(:eval_id => id, :code => code))) * "\n"
    run_file_line(id, path) = sprint(DR.json_write, Dict(:v => 1, :id => id, :op => "repl.run_file",
        :payload => Dict(:eval_id => id, :path => path, :fresh => false))) * "\n"

    # A client sends its next request the moment an answer arrives. The answered eval is over by then, so the next
    # request is accepted, after an eval and after a run_file alike.
    @testset "serve: a request sent the moment $op answers is accepted" for op in ("repl.eval", "repl.run_file")
        path = tempname() * ".jl"
        write(path, "1\n")
        bs_in, bs_out = Base.BufferStream(), Base.BufferStream()
        out = AnswerThenNext(bs_out, 1, () -> (write(bs_in, eval_line(2, "40 + 2")); flush(bs_in)))
        @async DR.serve(bs_in, out)
        @async (sleep(60); close(bs_out); close(bs_in))
        write(bs_in, op == "repl.eval" ? eval_line(1, "1") : run_file_line(1, path))
        flush(bs_in)
        second = Any[]
        while true
            line = readline(bs_out)
            isempty(line) && (eof(bs_out) ? break : continue)
            env = DR.json_read(line)
            get(env, :id, 0) == 2 || continue
            push!(second, env)
            get(env, :kind, "") == "res" && break
        end
        close(bs_in)
        rm(path; force = true)
        @test out.fired
        frames = [e[:payload][:frame] for e in second if get(e, :kind, "") == "evt"]
        @test !any(f -> f[:kind] == "error", frames)
        @test any(f -> f[:kind] == "value" && strip(f[:text]) == "42", frames)
    end

    # A child process runs `body` against `serve` with an `AnswerThenNext` output and returns its exit code. `line`
    # builds an eval request; the child exits 4 if nothing decides within a minute.
    function child_exit_code(body::String)
        script = """
            using ShipToolsRepl
            include($(repr(joinpath(@__DIR__, "answer_then_next.jl"))))
            line(id, code) = sprint(ShipToolsRepl.json_write, Dict(:v => 1, :id => id, :op => "repl.eval",
                :payload => Dict(:eval_id => id, :code => code))) * "\\n"
            bs_in, bs_out = Base.BufferStream(), Base.BufferStream()
            @async (sleep(60); exit(4))
            """ * body
        cmd = `$(Base.julia_cmd()) --startup-file=no --project=$(pkgdir(ShipToolsRepl)) -e $script`
        return run(ignorestatus(cmd)).exitcode
    end

    # The guard still holds one eval at a time: an answered eval that finishes after the next eval started leaves that
    # eval's mark, so a third request while the second runs is refused (0); accepting it (5) would overlap two evals'
    # output capture, which is why this runs in a child.
    @testset "serve: an answered eval's finish leaves the next eval's mark" begin
        @test child_exit_code("""
            out = AnswerThenNext(bs_out, 1, () -> (write(bs_in, line(2, "sleep(5); 2")); flush(bs_in)))
            @async ShipToolsRepl.serve(bs_in, out)
            write(bs_in, line(1, "sleep(0.5); 1"))
            flush(bs_in)
            while ShipToolsRepl.CURRENT_EVAL[] === nothing
                yield()
            end
            first = ShipToolsRepl.CURRENT_EVAL[]
            while true
                env = ShipToolsRepl.json_read(readline(bs_out))
                if get(env, :kind, "") == "res" && get(env, :id, 0) == 1
                    wait(first)
                    write(bs_in, line(3, "3"))
                    flush(bs_in)
                elseif get(env, :id, 0) == 3 && get(env, :kind, "") == "evt" &&
                       occursin("REPL busy", string(get(env[:payload][:frame], :message, "")))
                    exit(0)
                elseif get(env, :id, 0) == 3 && get(env, :kind, "") == "res"
                    exit(5)
                end
            end
            """) == 0
    end

    # The user-facing case: an exit() sent the moment an answer arrives ends the REPL instead of being refused (0, the
    # exit having run; 3, its refusal). In a child, since the eval exits it.
    @testset "serve: an exit() sent the moment an answer arrives ends the REPL" begin
        @test child_exit_code("""
            out = AnswerThenNext(bs_out, 1, () -> (write(bs_in, line(2, "exit(0)")); flush(bs_in)))
            @async ShipToolsRepl.serve(bs_in, out)
            write(bs_in, line(1, "1"))
            flush(bs_in)
            while true
                env = ShipToolsRepl.json_read(readline(bs_out))
                get(env, :kind, "") == "res" && get(env, :id, 0) == 2 && exit(3)
            end
            """) == 0
    end

    @testset "serve: repl.interrupt cancels a running eval" begin
        bs_in, bs_out, _ = drive(String[])
        # A long, yielding eval so the dispatch loop stays responsive to interrupt.
        evalreq = sprint(DR.json_write, Dict(
            :v => 1, :id => 1, :op => "repl.eval",
            :payload => Dict(:eval_id => 1, :code => "sleep(60)"),
        ))
        write(bs_in, evalreq * "\n"); flush(bs_in)
        # Let the eval task actually start before interrupting.
        sleep(1.0)
        intreq = sprint(DR.json_write, Dict(
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
            env = DR.json_read(line)
            if get(env, :kind, "") == "res" && env[:op] == "repl.interrupt"
                saw_interrupt_res = true
                @test env[:payload][:interrupted] == true
            elseif get(env, :kind, "") == "evt" && env[:op] == "repl.frame" && env[:id] == 1
                k = env[:payload][:frame][:kind]
                k == "error" && (saw_eval_error = true)
                k == "done" && (saw_eval_done = true)
            end
        end
        close(bs_in)
        @test saw_interrupt_res
        @test saw_eval_error        # InterruptException surfaced as an error frame
        @test saw_eval_done         # eval terminated with a done frame
    end

    @testset "announce_browserview: swallowed serves still emit; returned ones don't double" begin
        # Swallowed: user code announces mid-eval but RETURNS something else
        # (the wrapper-API case) — exactly one browser frame must stream.
        frames = Dict[]
        DR.CURRENT_EMIT[] = f -> push!(frames, f)
        empty!(DR.ANNOUNCED_BROWSER_URLS)
        bv = DR.announce_browserview(DR.BrowserView("http://127.0.0.1:59991/"))
        @test bv isa DR.BrowserView
        @test length(frames) == 1 && frames[1][:kind] == "browser"
        # Idempotent within the eval: announcing the same URL again is a no-op.
        DR.announce_browserview(DR.BrowserView("http://127.0.0.1:59991/"))
        @test length(frames) == 1
        # Returned AND announced: value_frames_for must skip (no second tab).
        @test isempty(DR.value_frames_for(bv))
        # Un-announced BrowserView (plain browserview(url)) still emits via
        # the value path.
        other = DR.BrowserView("http://127.0.0.1:59992/")
        vf = DR.value_frames_for(other)
        @test length(vf) == 1 && vf[1][:kind] == "browser"
        # Policy override (codex review): dedupe is keyed on (url, open), so a
        # no-open serve announce followed by RETURNING an open=true view of
        # the same URL still emits the deliberate open — one allowlist frame,
        # one open frame, never a swallowed policy.
        empty!(DR.ANNOUNCED_BROWSER_URLS)
        empty!(frames)
        silent = DR.announce_browserview(DR.browserview("http://127.0.0.1:59993/"; open = false))
        @test length(frames) == 1 && frames[1][:open] === false
        opened = DR.value_frames_for(DR.browserview("http://127.0.0.1:59993/"))
        @test length(opened) == 1 && opened[1][:open] === true
        # Same policy still dedupes to nothing.
        @test isempty(DR.value_frames_for(silent))
        # Outside a streamed eval the announce is a harmless no-op.
        DR.CURRENT_EMIT[] = nothing
        @test DR.announce_browserview(other) === other
        empty!(DR.ANNOUNCED_BROWSER_URLS)
    end

    @testset "browser frame: opened on one named frontend (decision 0031)" begin
        url = "http://127.0.0.1:1/0123456789abcdef0123456789abcdef"
        f = DR.value_frames_for(DR.browserview(url; open = "laptop"))
        @test length(f) == 1
        @test f[1][:open] === false
        @test f[1][:fe] == "fe@laptop"
        @test DR.value_frames_for(DR.browserview(url; open = "fe@laptop")) == f
        @test !haskey(DR.value_frames_for(DR.browserview(url))[1], :fe)
        @test !haskey(DR.value_frames_for(DR.browserview(url; open = false))[1], :fe)
        @test DR.browser_announce_key(DR.browserview(url; open = "a")) !=
              DR.browser_announce_key(DR.browserview(url; open = "b"))
        @test_throws ArgumentError DR.browserview(url; open = "")
    end

    @testset "serve: ready sentinel is the first stdout envelope (ADR 0009 update)" begin
        out = IOBuffer()
        DR.serve(IOBuffer(""), out)   # empty input: dispatch loop exits immediately
        lines = split(String(take!(out)), '\n'; keepempty = false)
        @test !isempty(lines)
        first_env = DR.json_read(lines[1])
        @test first_env[:kind] == "evt"
        @test first_env[:op] == "repl.ready"
        @test first_env[:payload][:protocol] == 1
        @test first_env[:payload][:julia] == string(VERSION)
        # Nothing else is emitted for empty input — the sentinel is serve's
        # ONLY unsolicited envelope, so old supervisors (first-line trigger)
        # see exactly one boot line and new ones see a designed signal.
        @test length(lines) == 1
    end

    @testset "WGLDisplay: pushed onto the display stack at boot, falls through for non-figures" begin
        # WGLMakie is not a dependency of this test environment (it's a
        # weakdep, loaded only by a consumer that pulls it in), so this only
        # exercises the base package's half: the push at boot, and that
        # `display` on anything else falls through with no method defined —
        # ShipToolsReplWGLMakieExt (loads only once WGLMakie does) is the
        # ONLY place a Makie-figure method is added.
        out = IOBuffer()
        DR.serve(IOBuffer(""), out)   # boot only, same idiom as the sentinel test above
        @test any(d isa DR.WGLDisplay for d in Base.Multimedia.displays)
        @test_throws MethodError display(DR.WGLDisplay(), 42)
        # Claims text/html (keeps Bonito's own display off the top of the
        # stack — see the `Base.displayable` comment) and nothing else.
        @test displayable(DR.WGLDisplay(), MIME("text/html"))
        @test !displayable(DR.WGLDisplay(), MIME("text/plain"))
    end

    @testset "wglshow serves its page only on the Bonito line it was built against (ADR 0049, User isolation)" begin
        for v in (v"5.1.0", v"5.1.1", v"5.2.0")
            @test DR.wgl_bonito_supported(v)
        end
        for v in (v"4.2.0", v"5.0.0", v"6.0.0")
            @test !DR.wgl_bonito_supported(v)
        end
    end

    @testset "ShipToolsRepl is stacked under the user's environment; a registered dependency can be shadowed by the user's manifest (Parsers 3 killed JSON3, 2026-09-18)" begin
        project = Pkg.TOML.parsefile(joinpath(pkgdir(ShipToolsRepl), "Project.toml"))
        deps = get(project, "deps", Dict{String,Any}())
        @test !isempty(deps)   # sanity: the guard isn't vacuously true
        stdlib_uuids = Set(keys(Pkg.Types.stdlibs()))
        for (name, uuid_str) in deps
            @test Base.UUID(uuid_str) in stdlib_uuids
        end
    end

    @testset "json codec: round-trip and RFC 8259 details" begin
        @testset "nested Dict/Vector round-trip" begin
            x = Dict(:a => 1, :b => Any[1, 2.5, "three", true, false, nothing],
                      :c => Dict(:d => "nested"))
            y = DR.json_read(sprint(DR.json_write, x))
            @test y[:a] == 1
            @test y[:b] == Any[1, 2.5, "three", true, false, nothing]
            @test y[:c][:d] == "nested"
        end

        @testset "string escapes read back exactly" begin
            s = "quote\"backslash\\slash/bell\bform\fnewline\nreturn\rtab\t"
            @test DR.json_read(sprint(DR.json_write, s)) == s
            # A control character with no short escape still round-trips via \u00XX.
            @test DR.json_read(sprint(DR.json_write, "\x01\x1f")) == "\x01\x1f"
        end

        @testset "surrogate pair decodes to the real character" begin
            @test DR.json_read("\"\\ud83d\\ude00\"") == "😀"
            # Writing it back out doesn't need to re-escape it — native UTF-8
            # bytes are legal JSON text — but it still round-trips.
            @test DR.json_read(sprint(DR.json_write, "😀")) == "😀"
            @test_throws ArgumentError DR.json_read("\"\\ud83d\"")   # unpaired high
            @test_throws ArgumentError DR.json_read("\"\\ude00\"")  # unpaired low
        end

        @testset "big integers and floats" begin
            @test DR.json_read("9223372036854775807") == typemax(Int64)
            @test DR.json_read("99999999999999999999999999") isa Float64  # overflow -> Float64
            @test DR.json_read("3.5") === 3.5
            @test DR.json_read("1e3") === 1.0e3
            @test DR.json_read("-0.5") === -0.5
        end

        @testset "non-finite floats write as null (serde_json has no NaN/Inf token)" begin
            @test sprint(DR.json_write, NaN) == "null"
            @test sprint(DR.json_write, Inf) == "null"
            @test sprint(DR.json_write, -Inf) == "null"
            @test DR.json_read(sprint(DR.json_write, NaN)) === nothing
        end

        @testset "Symbol keys and NamedTuple objects" begin
            @test sprint(DR.json_write, Dict(:x => 1)) == "{\"x\":1}"
            @test DR.json_read(sprint(DR.json_write, (a = 1, b = "two"))) ==
                  Dict{Symbol,Any}(:a => 1, :b => "two")
        end

        @testset "a real request line parses, a real envelope round-trips" begin
            # Same envelope shape as the fixture in rust/backend/src/sidecars/repl/'s
            # own tests: {"v":1,"id":1,"kind":"res","op":"repl.eval","payload":{"answered":true}}
            line = "{\"v\":1,\"id\":1,\"kind\":\"res\",\"op\":\"repl.eval\",\"payload\":{\"answered\":true}}"
            req = DR.json_read(line)
            @test req[:id] == 1
            @test req[:op] == "repl.eval"
            @test req[:payload][:answered] == true

            env = Dict(:v => 1, :id => UInt64(7), :kind => "res", :op => "repl.eval",
                       :payload => Dict(:eval_id => 99, :mode => "julia", :elapsed_ms => 12))
            out = sprint(DR.json_write, env)
            @test DR.json_read(out) == Dict{Symbol,Any}(
                :v => 1, :id => 7, :kind => "res", :op => "repl.eval",
                :payload => Dict{Symbol,Any}(:eval_id => 99, :mode => "julia", :elapsed_ms => 12),
            )
        end
    end
end
