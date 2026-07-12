module ShipToolsRepl

using Base64
using JSON3
using Pkg

export serve, browserview, BrowserView, wglshow

const PROTOCOL_VERSION = 1

"""
    BrowserView(url)

Marker wrapping a loopback URL for a live, browser-served artifact (an
interactive WGLMakie/Bonito figure, a served dashboard, …). Return one as the
last expression of an eval — or call [`browserview`](@ref) — and the REPL emits
a `browser` frame instead of a static `value`/`image`, which the frontend hands
to the OS browser-open (ADR 0032). `url` must be loopback-shaped
(`http://127.0.0.1:<port>/…`) so it resolves through the launcher's `-L` tunnel
on a remote frontend; the WGLMakie/Bonito port is `SOT_WGL_PORT` (default 1241,
forwarded by the launcher alongside pluto/video/docs — 1237–1240 are the docs
pool, so WGL sits at 1241).

`BrowserView` deliberately carries no plotting dependency: the Bonito server
that produces `url` lives in the *user's* project env (whichever WGLMakie they
`using`), so `ShipToolsRepl` stays lightweight and backend-agnostic.
"""
struct BrowserView
    url::String
end

"""
    browserview(url) -> BrowserView

Convenience constructor. `return browserview(server_url)` at the end of an eval
to open `url` in the frontend's browser.
"""
browserview(url::AbstractString) = BrowserView(String(url))

# Tracks the Bonito server started by `wglshow` so a repeat call frees the port
# instead of hitting EADDRINUSE. `Any` — ShipToolsRepl never loads Bonito.
const WGL_SERVER = Ref{Any}(nothing)

"""
    wglshow(fig; port = SOT_WGL_PORT or 1241) -> BrowserView

Serve an interactive WGLMakie figure over Bonito on a loopback port and return a
[`BrowserView`](@ref), so the frontend auto-opens it in the browser (ADR 0032).
Call it as the last expression of an eval:

    using WGLMakie
    fig = surface(-10:0.4:10, -10:0.4:10, (x, y) -> sin(x) + cos(y);
                  axis = (; type = Axis3))
    wglshow(fig)

`ShipToolsRepl` carries no plotting dependency: WGLMakie/Bonito are resolved at
call time from the *user's* loaded env (`using WGLMakie` first — Bonito comes in
as its dependency). The server binds `127.0.0.1:port` with a loopback-shaped
`proxy_url`, which the launcher's `-L <port>` tunnel forwards to a remote
frontend. It lives as long as the REPL; a repeat `wglshow` replaces it.

Pinned against WGLMakie 0.13 / Bonito 5.1 (validated live, ADR 0032).
"""
function wglshow(fig; port::Integer = parse(Int, get(ENV, "SOT_WGL_PORT", "1241")))
    isdefined(Main, :WGLMakie) ||
        error("wglshow: no WGLMakie loaded — run `using WGLMakie` in this REPL first")
    WGL = getfield(Main, :WGLMakie)
    # Bonito arrives as WGLMakie's dependency; require it by UUID (already loaded,
    # so this just returns the module) rather than assume the user `using`d it.
    Bonito = Base.require(Base.PkgId(
        Base.UUID("824d6782-a2ef-11e9-3a09-e5662e0c26f8"), "Bonito"))
    host = "127.0.0.1"
    external = "http://$host:$port"
    # invokelatest throughout: these methods were defined by the user's `using`
    # after wglshow's world age (same reason value_frames_for uses it).
    Base.invokelatest(WGL.activate!)
    Base.invokelatest(Bonito.configure_server!;
        listen_url = host, listen_port = port, proxy_url = external)
    prev = WGL_SERVER[]
    if prev !== nothing
        try
            Base.invokelatest(close, prev)
        catch
        end
    end
    app = Base.invokelatest(Bonito.App, fig)
    server = Base.invokelatest(Bonito.Server, app, host, port; proxy_url = external)
    WGL_SERVER[] = server
    url = Base.invokelatest(Bonito.online_url, server, "/")
    return BrowserView(url)
end

# Serializes envelope writes to `io_out`. With streaming (ADR 0009 phase-2)
# the eval runs on its own task and emits frames concurrently with the
# dispatch loop, so two tasks can race on the output stream; this lock keeps
# each NDJSON envelope atomic.
const OUT_LOCK = ReentrantLock()

# The currently-running eval task (or `nothing`). `repl.interrupt` schedules an
# `InterruptException` onto it; the single-eval guard uses it to reject a second
# concurrent eval (the stdout/stderr redirect is process-global, so overlapping
# evals would clobber each other's capture).
const CURRENT_EVAL = Ref{Union{Task,Nothing}}(nothing)

"""
    serve(io_in::IO, io_out::IO)

NDJSON dispatch loop for the persistent REPL child. One JSON request per
line on `io_in`; envelopes out on `io_out`.

ADR 0009 phase-2 (streaming): an eval no longer blocks the dispatch loop.
`repl.eval` / `repl.run_file` spawn the evaluation on a task and return
immediately, so the loop stays free to receive a `repl.interrupt` mid-eval.
Each output frame is emitted as its own `repl.frame` **evt** envelope as it is
produced; the request's `res` envelope is a terminal **ack** (no frames) sent
once the eval finishes.

Frame kinds (mirroring ADR 0009), carried in the evt payload's `frame`:

- `stdout` / `stderr` — `{kind, text}` (streamed incrementally as the eval prints)
- `value` — `{kind, mime, text}` (text/plain via `show(::IO, ::MIME, ::Any)`)
- `image` — `{kind, mime, data_base64, bytes}`
- `error` — `{kind, message, stacktrace: [{file, line, fn}, ...]}`
- `done` — `{kind, eval_id, elapsed_ms}` (always the last frame for an eval_id)

Stderr-the-stream is for free-text logging; the Rust supervisor never reads
it as data.
"""
function serve(io_in::IO, io_out::IO)
    println(stderr, "sot-repl ready · julia=$(VERSION)")
    flush(stderr)

    for line in eachline(io_in)
        isempty(strip(line)) && continue
        req = try
            JSON3.read(line)
        catch e
            write_envelope(io_out, "res", 0, "repl.parse_error",
                Dict(:error => "bad request: $(e)"))
            continue
        end

        id = get(req, :id, UInt64(0))
        op = get(req, :op, "")
        payload = get(req, :payload, Dict{Symbol,Any}())

        try
            if op == "repl.eval"
                handle_eval(io_out, id, payload)
            elseif op == "repl.run_file"
                handle_run_file(io_out, id, payload)
            elseif op == "repl.interrupt"
                handle_interrupt(io_out, id, payload)
            else
                write_envelope(io_out, "res", id, op,
                    Dict(:error => "unknown op: $op", :code => "unknown_op"))
            end
        catch e
            bt = sprint(showerror, e, catch_backtrace())
            println(stderr, "repl exception: $bt")
            flush(stderr)
            write_envelope(io_out, "res", id, op,
                Dict(:error => sprint(showerror, e), :code => "repl_exception"))
        end
    end
end

# True while a spawned eval task is still running.
function eval_in_progress()
    t = CURRENT_EVAL[]
    return t !== nothing && !istaskdone(t)
end

# Returns a closure that writes one frame as a `repl.frame` evt, correlated to
# `id` (request) and `eval_id`.
make_emit(io::IO, id, eval_id) =
    frame -> write_envelope(io, "evt", id, "repl.frame",
        Dict(:eval_id => eval_id, :frame => frame))

"""
    handle_eval(io, id, payload)

Spawn the eval on a task and return immediately so the dispatch loop can still
receive `repl.interrupt`. Frames stream as `repl.frame` evts; a terminal `res`
ack closes the request.
"""
function handle_eval(io::IO, id, payload)
    eval_id = get(payload, :eval_id, UInt64(0))
    code = String(get(payload, :code, ""))
    mode = String(get(payload, :mode, "julia"))

    if eval_in_progress()
        emit = make_emit(io, id, eval_id)
        emit(Dict(:kind => "error",
                  :message => "REPL busy: another eval is in progress",
                  :stacktrace => Dict[]))
        emit(Dict(:kind => "done", :eval_id => eval_id, :elapsed_ms => 0))
        write_envelope(io, "res", id, "repl.eval",
            Dict(:eval_id => eval_id, :mode => mode, :elapsed_ms => 0))
        return
    end

    CURRENT_EVAL[] = @async begin
        try
            run_eval_streaming(io, id, eval_id, mode, code)
        catch e
            # Safety net: eval errors are handled inside run_eval_streaming;
            # this only fires if the streaming machinery itself failed. Always
            # emit a terminal ack so the backend's request doesn't hang.
            emit_fallback_done(io, id, eval_id, "repl.eval",
                Dict(:eval_id => eval_id, :mode => mode, :elapsed_ms => 0), e)
        finally
            CURRENT_EVAL[] = nothing
        end
    end
    return
end

function run_eval_streaming(io::IO, id, eval_id, mode, code)
    emit = make_emit(io, id, eval_id)
    start = time()

    if mode == "pkg"
        Pkg.REPLMode.PRINTED_REPL_WARNING[] = true
        stream_eval_frames(emit) do
            Base.invokelatest(Pkg.REPLMode.do_cmds, String(code), stdout)
            return nothing
        end
    else
        parse_err = nothing
        expr = try
            Meta.parseall(code)
        catch e
            parse_err = e
            nothing
        end
        if parse_err !== nothing
            emit(Dict(:kind => "error",
                      :message => "parse error: $(sprint(showerror, parse_err))",
                      :stacktrace => Dict[]))
        else
            stream_eval_frames(emit) do
                Core.eval(Main, expr)
            end
        end
    end

    elapsed_ms = round(Int, (time() - start) * 1000)
    emit(Dict(:kind => "done", :eval_id => eval_id, :elapsed_ms => elapsed_ms))
    write_envelope(io, "res", id, "repl.eval",
        Dict(:eval_id => eval_id, :mode => mode, :elapsed_ms => elapsed_ms))
end

"""
    handle_run_file

Run a `.jl` file in the persistent REPL (`fresh:false`, via `include`) or in a
fresh `julia` subprocess (`fresh:true`). Project is discovered by walking up
from `path`; the persistent REPL's active project is the fallback. Streams
frames like `repl.eval`.

Note: as of priority J the Rust supervisor intercepts `fresh:true` *before*
the request reaches us (it bounces the REPL child to the file's project and
forwards `fresh:false`); the subprocess branch is preserved for a future direct
caller.
"""
function handle_run_file(io::IO, id, payload)
    eval_id = get(payload, :eval_id, UInt64(0))
    path = String(get(payload, :path, ""))
    fresh = Bool(get(payload, :fresh, false))

    if isempty(path)
        write_envelope(io, "res", id, "repl.run_file",
            Dict(:error => "missing path", :code => "bad_request"))
        return
    end
    abs_path = isabspath(path) ? String(path) : abspath(String(path))
    if !isfile(abs_path)
        write_envelope(io, "res", id, "repl.run_file",
            Dict(:error => "no such file: $abs_path", :code => "io_error"))
        return
    end

    current_project_dir = try
        ap = Base.active_project()
        ap === nothing ? pwd() : dirname(String(ap))
    catch
        pwd()
    end
    dir, _toml, source = discover_project(abs_path; fallback = current_project_dir)

    ack_payload = Dict(
        :eval_id => eval_id, :path => abs_path, :fresh => fresh,
        :project_dir => dir, :project_source => string(source), :elapsed_ms => 0,
    )

    if eval_in_progress()
        emit = make_emit(io, id, eval_id)
        emit(Dict(:kind => "error",
                  :message => "REPL busy: another eval is in progress",
                  :stacktrace => Dict[]))
        emit(Dict(:kind => "done", :eval_id => eval_id, :elapsed_ms => 0))
        write_envelope(io, "res", id, "repl.run_file", ack_payload)
        return
    end

    CURRENT_EVAL[] = @async begin
        try
            run_file_streaming(io, id, eval_id, abs_path, fresh, dir, source, current_project_dir)
        catch e
            emit_fallback_done(io, id, eval_id, "repl.run_file", ack_payload, e)
        finally
            CURRENT_EVAL[] = nothing
        end
    end
    return
end

function run_file_streaming(io::IO, id, eval_id, abs_path, fresh, dir, source, current_project_dir)
    emit = make_emit(io, id, eval_id)
    start = time()

    if fresh
        emit(Dict(:kind => "stderr",
            :text => "[repl.run_file fresh=true] " *
                     "$(Base.julia_cmd().exec[1]) --project=$dir $abs_path " *
                     "(project source: $(string(source)))\n"))
        pipe_out = Pipe()
        pipe_err = Pipe()
        cmd = `$(Base.julia_cmd()) --color=no --project=$dir $abs_path`
        proc = run(pipeline(cmd; stdout = pipe_out, stderr = pipe_err); wait = false)
        close(pipe_out.in)
        close(pipe_err.in)
        # Stream subprocess output incrementally, same as in-process eval.
        reader_out = @async stream_pipe(pipe_out, emit, "stdout")
        reader_err = @async stream_pipe(pipe_err, emit, "stderr")
        wait(proc)
        wait(reader_out)
        wait(reader_err)
        if proc.exitcode != 0
            emit(Dict(:kind => "error",
                      :message => "julia subprocess exited with code $(proc.exitcode)",
                      :stacktrace => Dict[]))
        end
    else
        if dir !== nothing && dir != current_project_dir
            emit(Dict(:kind => "stderr",
                :text => "[repl.run_file fresh=false] note: file's project is " *
                         "$dir but persistent REPL is using $current_project_dir; " *
                         "include() may fail if deps differ.\n"))
        end
        stream_eval_frames(emit) do
            Base.include(Main, abs_path)
        end
    end

    elapsed_ms = round(Int, (time() - start) * 1000)
    emit(Dict(:kind => "done", :eval_id => eval_id, :elapsed_ms => elapsed_ms))
    write_envelope(io, "res", id, "repl.run_file", Dict(
        :eval_id => eval_id, :path => abs_path, :fresh => fresh,
        :project_dir => dir, :project_source => string(source),
        :elapsed_ms => elapsed_ms,
    ))
end

"""
    stream_eval_frames(f, emit)

Run `f()` with stdout/stderr captured via async-drained pipes, emitting each
output chunk as a `stdout`/`stderr` frame *as it arrives* (incremental), then a
trailing `value` (last expression's result via `MIME"text/plain"`) or `error`
frame (if `f()` threw — including `InterruptException` from `repl.interrupt`).
`f` is first so call sites can pass it as a `do` block (`stream_eval_frames(emit) do … end`).

Julia 1.12's `redirect_stdout` doesn't accept `IOBuffer`, hence the `Pipe`
plumbing. The redirect is process-global, so overlapping evals are rejected by
the single-eval guard in `handle_eval` / `handle_run_file`.
"""
function stream_eval_frames(f, emit)
    pipe_out = Pipe()
    pipe_err = Pipe()
    Base.link_pipe!(pipe_out; reader_supports_async = true, writer_supports_async = true)
    Base.link_pipe!(pipe_err; reader_supports_async = true, writer_supports_async = true)

    old_stdout = stdout
    old_stderr = stderr
    reader_out = @async stream_pipe(pipe_out, emit, "stdout")
    reader_err = @async stream_pipe(pipe_err, emit, "stderr")

    result = nothing
    threw = nothing
    local_bt = Base.StackTraces.StackFrame[]
    try
        redirect_stdout(pipe_out)
        redirect_stderr(pipe_err)
        try
            result = f()
        catch e
            threw = e
            local_bt = stacktrace(catch_backtrace())
        end
    finally
        redirect_stdout(old_stdout)
        redirect_stderr(old_stderr)
        close(pipe_out.in)
        close(pipe_err.in)
    end
    # Drain whatever the readers haven't emitted yet (the close above lets them
    # hit eof). Ordering: all stdout/stderr frames precede value/error.
    wait(reader_out)
    wait(reader_err)

    if threw !== nothing
        stack = [Dict(
            :file => string(s.file),
            :line => s.line,
            :fn => string(s.func),
        ) for s in local_bt]
        msg = threw isa InterruptException ?
            "InterruptException: eval interrupted by repl.interrupt" :
            sprint(showerror, threw)
        emit(Dict(:kind => "error", :message => msg, :stacktrace => stack))
    elseif result !== nothing
        for fr in value_frames_for(result)
            emit(fr)
        end
    end
    return nothing
end

"""
    stream_pipe(pipe, emit, kind)

Drain `pipe` to eof, emitting `{kind, text}` frames as bytes arrive. Buffers a
short trailing remainder so a UTF-8 multibyte char split across two
`readavailable` chunks isn't emitted as invalid UTF-8 (which JSON3 would
reject).
"""
function stream_pipe(pipe, emit, kind)
    leftover = UInt8[]
    try
        while !eof(pipe)
            data = readavailable(pipe)
            isempty(data) && continue
            append!(leftover, data)
            s, leftover = utf8_prefix(leftover)
            isempty(s) || emit(Dict(:kind => kind, :text => s))
        end
    catch
        # pipe closed/errored mid-read — stop draining.
    end
    if !isempty(leftover)
        emit(Dict(:kind => kind, :text => String(leftover)))
    end
    return nothing
end

# Split `buf` into (longest valid-UTF8 prefix as String, remaining bytes).
# Trims at most 3 trailing bytes to land on a char boundary (UTF-8 chars are
# <=4 bytes, so a complete char's continuation bytes number <=3).
function utf8_prefix(buf::Vector{UInt8})
    n = length(buf)
    for cut in 0:min(3, n)
        s = String(copy(@view buf[1:(n - cut)]))
        if isvalid(s)
            return (s, buf[(n - cut + 1):end])
        end
    end
    return ("", buf)
end

"""
    value_frames_for(result) -> Vector{Dict}

Render the last-expression value into one or more frames. Prefers image-bearing
MIMEs (`image/png`, `image/svg+xml`) when `showable` says they apply — that's
what makes CairoMakie figures, `Plots.Plot`s, etc. flow through as `image`
frames. Falls through to `text/plain`.
"""
function value_frames_for(result)
    out = Dict[]
    # ADR 0032: a BrowserView is a live browser-served artifact, not a static
    # value — emit a `browser` frame the frontend opens in the OS browser.
    # Checked before the image/text MIME probes so it wins even though a served
    # figure may also be `showable` as an image.
    if result isa BrowserView
        push!(out, Dict(:kind => "browser", :url => result.url))
        return out
    end
    img_mimes = (MIME"image/png"(), MIME"image/svg+xml"())
    # invokelatest because the eval may have defined the showable/show methods
    # itself (e.g. `using CairoMakie` adds Figure showables mid-eval).
    for m in img_mimes
        is_showable = try
            Base.invokelatest(showable, m, result)
        catch
            false
        end
        is_showable || continue
        buf = IOBuffer()
        ok = try
            Base.invokelatest(show, buf, m, result)
            true
        catch
            false
        end
        if ok
            data = take!(buf)
            if !isempty(data)
                push!(out, Dict(
                    :kind => "image",
                    :mime => string(m),
                    :data_base64 => Base64.base64encode(data),
                    :bytes => length(data),
                ))
                return out
            end
        end
    end
    valbuf = IOBuffer()
    try
        Base.invokelatest(show, valbuf, MIME"text/plain"(), result)
    catch e
        print(valbuf, "<unshowable $(typeof(result)): $(sprint(showerror, e))>")
    end
    push!(out, Dict(
        :kind => "value",
        :mime => "text/plain",
        :text => String(take!(valbuf)),
    ))
    out
end

"""
    discover_project(path; fallback=nothing) -> (dir, toml, source)

Walk up from `path` looking for the nearest `Project.toml`. Mirrors the
kernel's `discover_project` so a fresh `julia --project=...` subprocess started
by `repl.run_file` picks up the same env the frontend would see.
"""
function discover_project(path::AbstractString;
                          fallback::Union{AbstractString, Nothing} = nothing)
    abs_in = isabspath(path) ? String(path) : abspath(String(path))
    start_dir = isdir(abs_in) ? abs_in : dirname(abs_in)
    dir = start_dir
    while !isempty(dir)
        toml = joinpath(dir, "Project.toml")
        if isfile(toml)
            return (dir, toml, :discovered)
        end
        parent = dirname(dir)
        parent == dir && break
        dir = parent
    end
    if fallback !== nothing && !isempty(String(fallback))
        return (String(fallback), nothing, :fallback)
    end
    return (nothing, nothing, :none)
end

"""
    handle_interrupt(io, id, payload)

Schedule a real `InterruptException` onto the running eval task (ADR 0009
phase-2). The exception lands at the task's next yield/safepoint — same
semantics as Ctrl-C in the stock REPL on a single thread; it surfaces to the
frontend as an `error` frame followed by `done`.
"""
function handle_interrupt(io::IO, id, _payload)
    t = CURRENT_EVAL[]
    if t !== nothing && !istaskdone(t)
        schedule(t, InterruptException(); error = true)
        write_envelope(io, "res", id, "repl.interrupt", Dict(:interrupted => true))
    else
        write_envelope(io, "res", id, "repl.interrupt",
            Dict(:interrupted => false, :note => "no eval in progress"))
    end
end

# Last-resort terminal ack when the streaming machinery itself throws (not a
# user-eval error — those are emitted as `error` frames inside
# `stream_eval_frames`). Guarantees the backend's request never hangs.
function emit_fallback_done(io::IO, id, eval_id, op, ack_payload, e)
    try
        emit = make_emit(io, id, eval_id)
        emit(Dict(:kind => "error",
                  :message => "internal repl error: $(sprint(showerror, e))",
                  :stacktrace => Dict[]))
        emit(Dict(:kind => "done", :eval_id => eval_id, :elapsed_ms => 0))
        write_envelope(io, "res", id, op, ack_payload)
    catch
        # Output stream is gone; nothing more we can do.
    end
end

function write_envelope(io::IO, kind, id, op, payload)
    env = Dict(:v => PROTOCOL_VERSION, :id => id, :kind => kind, :op => op, :payload => payload)
    lock(OUT_LOCK) do
        JSON3.write(io, env)
        write(io, '\n')
        flush(io)
    end
end

end # module
