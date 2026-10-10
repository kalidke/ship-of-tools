"""
    ShipToolsRepl

The persistent Julia REPL shim the backend daemon supervises for every
workspace. Speaks a length-prefixed-free, newline-delimited JSON protocol
over stdin/stdout (see `serve`).

**Invariant: `[deps]` is stdlib only.** The daemon launches this package
STACKED under the user's own project via `JULIA_LOAD_PATH=@:<repl_project>:`
(`rust/backend/src/sidecars/repl/supervisor.rs`, `spawn_supervisor`; ADR 0032 §2) so
`using ShipToolsRepl` resolves even though `--project` points at the user's
env. Julia resolves a package's own dependencies by walking that same load
path, so a registered dependency of THIS package can be shadowed by whatever
version the user's manifest happens to pin for it. That shadowing killed the
shim outright once already: `JSON3`'s dependency `Parsers` was shadowed by a
CairoMakie-pinned `Parsers 3.0.0` that no `JSON3` release supports, and the
shim failed to precompile for every user with that combination in their
project (2026-09-18). The fix is the invariant, not a version pin — a stacked
shim must share NO registered package with any user project, ever — so
`ShipToolsRepl` depends only on stdlib (`Base64`, `Pkg`, `Random`), and its
own JSON codec lives in `json.jl` rather than pulling one in. The guard test
in `test/runtests.jl` enforces this by walking `Project.toml`'s `[deps]`.
"""
module ShipToolsRepl

using Base64
using Pkg
using Random

export serve, browserview, BrowserView, wglshow

const PROTOCOL_VERSION = 1

include("json.jl")
include("wgl.jl")
include("frames.jl")

# Serializes envelope writes to `io_out`. With streaming (ADR 0009 phase-2)
# the eval runs on its own task and emits frames concurrently with the
# dispatch loop, so two tasks can race on the output stream; this lock keeps
# each NDJSON envelope atomic.
const OUT_LOCK = ReentrantLock()

# An eval holding the single-eval guard. `id` is its eval_id, which a scoped
# `repl.interrupt` names; `task` runs it. `stage` is where its user code is, and
# so where an interrupt goes (`handle_interrupt`): `:running`, raised in the code
# at once; `:waiting`, not started, so the interrupt is `:held` and the code never
# runs (`run_user_code`); `:done`, past interrupting.
mutable struct Eval
    const id::Any
    const task::Task
    stage::Symbol
end

# The eval holding the guard (or `nothing`), from its spawn until it releases the
# guard just before its answer (`spawn_eval`) or its task ends. The guard uses it
# to reject a second concurrent eval (the stdout/stderr redirect is
# process-global, so overlapping evals would clobber each other's capture).
const CURRENT_EVAL = Ref{Union{Eval,Nothing}}(nothing)

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
- `browser` — `{kind, url, open}` (live loopback-served artifact, ADR 0032)
- `error` — `{kind, message, stacktrace: [{file, line, fn}, ...]}`
- `done` — `{kind, eval_id, elapsed_ms}` (always the last frame for an eval_id)

Before any of those, serve's first act is the **ready sentinel** — an evt
envelope `op="repl.ready"` (ADR 0009 update, 2026-08-24). It is emitted at the
exact point dispatch begins (the next statement is the request loop), so the
supervisor's `starting → ready` flip keys off a designed signal instead of
whichever eval output happens to arrive first — and a booted-but-idle child
reads `ready`, not `starting`, forever.

Stderr-the-stream is for free-text logging; the Rust supervisor never reads
it as data.
"""
function serve(io_in::IO, io_out::IO)
    println(stderr, "sot-repl ready · julia=$(VERSION)")
    flush(stderr)

    # Display-stack integration (see WGLDisplay): pushed cheaply (one struct,
    # no WGLMakie load) — the ShipToolsReplWGLMakieExt package extension is
    # what actually teaches it to render a figure, and that only loads once
    # WGLMakie does. Guarded so a process that calls `serve` more than once
    # (every test in this file does) doesn't pile up duplicate displays.
    any(d isa WGLDisplay for d in Base.Multimedia.displays) ||
        pushdisplay(WGLDisplay())

    write_envelope(io_out, "evt", 0, "repl.ready",
        Dict(:julia => string(VERSION), :protocol => PROTOCOL_VERSION))

    for line in eachline(io_in)
        isempty(strip(line)) && continue
        req = try
            json_read(line)
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

# True while an eval holds the guard: from its spawn until it releases the guard just
# before its answer, or until its task ends.
function eval_in_progress()
    ev = CURRENT_EVAL[]
    return ev !== nothing && !istaskdone(ev.task)
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

    spawn_eval(io, id, eval_id, "repl.eval", Dict(:eval_id => eval_id, :mode => mode)) do emit
        run_eval_streaming(emit, mode, code)
    end
end

"""
    spawn_eval(work, io, id, eval_id, op, ack)

Run one eval on its own task, so the dispatch loop stays free to receive
`repl.interrupt`, and answer it once. `work(emit)` runs the user code and streams
its frames; user errors are frames inside it, and a failure of the streaming
machinery itself becomes an `internal repl error` frame here. Then the eval
releases the single-eval guard and sends its done frame and its terminal res,
whose payload is `ack` with `elapsed_ms`. The release comes first, so a request
sent the moment the answer arrives is accepted; it is the eval's only release,
made while it holds the guard, so it never clears a later eval's.
"""
function spawn_eval(work, io::IO, id, eval_id, op, ack)
    task = Task() do
        emit = make_emit(io, id, eval_id)
        start = time()
        try
            work(emit)
        catch e
            emit(Dict(:kind => "error",
                      :message => "internal repl error: $(sprint(showerror, e))",
                      :stacktrace => Dict[]))
        end
        CURRENT_EVAL[] = nothing
        elapsed_ms = round(Int, (time() - start) * 1000)
        emit(Dict(:kind => "done", :eval_id => eval_id, :elapsed_ms => elapsed_ms))
        write_envelope(io, "res", id, op, merge(ack, Dict(:elapsed_ms => elapsed_ms)))
    end
    CURRENT_EVAL[] = Eval(eval_id, task, :waiting)
    schedule(task)
    return
end

# Runs the calling eval's user code, the only place an interrupt reaches the eval
# (`handle_interrupt`): an interrupt held while the eval waited stops it here,
# before the code runs. Outside an eval (a direct call) it just runs `f`.
function run_user_code(f)
    ev = CURRENT_EVAL[]
    (ev === nothing || ev.task !== current_task()) && return f()
    try
        ev.stage === :held && throw(InterruptException())
        ev.stage = :running
        return f()
    finally
        ev.stage = :done
    end
end

function run_eval_streaming(emit, mode, code)
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
end

"""
    handle_run_file

Run a `.jl` file in the persistent REPL with `include`. A fresh run never
reaches the shim: the daemon restarts the REPL into the file's project and then
sends a plain run. Project is discovered by walking up from `path`; the
persistent REPL's active project is the fallback. Streams frames like
`repl.eval`.
"""
function handle_run_file(io::IO, id, payload)
    eval_id = get(payload, :eval_id, UInt64(0))
    path = String(get(payload, :path, ""))

    if isempty(path)
        write_envelope(io, "res", id, "repl.run_file",
            Dict(:error => "missing path", :code => "bad_request"))
        return
    end
    # Relative paths resolve against SOT_WORKSPACE_ROOT when set (the
    # `sot-fe repl run` contract: paths are workspace-relative), falling back
    # to cwd. The daemon now forwards absolute paths AND sets the child's cwd
    # to the project root, so this is defense-in-depth for old daemons and
    # direct callers — the 2026-07-24 field failure was a relative path
    # resolving against the daemon's inherited, launch-dependent cwd.
    abs_path = if isabspath(path)
        String(path)
    else
        root = get(ENV, "SOT_WORKSPACE_ROOT", "")
        base = (!isempty(root) && isdir(root)) ? root : pwd()
        joinpath(base, String(path))
    end
    if !isfile(abs_path)
        # Emit the failure as STREAMED error+done frames, not only the res:
        # for a fire-and-forget run the terminal res is dropped by the
        # supervisor (by design), so a res-only error made this failure
        # perfectly invisible — no drawer entry, no FE close-out, "accepted"
        # then silence (the 2026-07-24 "--fresh include never runs" report).
        emit = make_emit(io, id, eval_id)
        emit(Dict(:kind => "error",
                  :message => "no such file: $abs_path",
                  :stacktrace => Dict[]))
        emit(Dict(:kind => "done", :eval_id => eval_id, :elapsed_ms => 0))
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
        :eval_id => eval_id, :path => abs_path,
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

    spawn_eval(io, id, eval_id, "repl.run_file", ack_payload) do emit
        run_file_streaming(emit, abs_path, dir, current_project_dir)
    end
end

function run_file_streaming(emit, abs_path, dir, current_project_dir)
    if dir !== nothing && dir != current_project_dir
        emit(Dict(:kind => "stderr",
            :text => "[repl.run_file] note: file's project is " *
                     "$dir but persistent REPL is using $current_project_dir; " *
                     "include() may fail if deps differ.\n"))
    end
    stream_eval_frames(emit) do
        Base.include(Main, abs_path)
    end
end

"""
    discover_project(path; fallback=nothing) -> (dir, toml, source)

Walk up from `path` looking for the nearest `Project.toml`; the fresh
`julia --project=...` subprocess `repl.run_file` starts runs in that env.
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

Interrupt the eval holding the guard (ADR 0009 phase-2), only in its user code
(`run_user_code`). While the code runs, a real `InterruptException` is
scheduled onto the eval's task and lands at its next yield/safepoint, the same
semantics as Ctrl-C in the stock REPL on a single thread; before the code
starts, the interrupt is held and the code never runs. Either way the eval
answers with an `error` frame, then `done` and its `res`. An interrupt that
names `eval_ids` reaches only an eval among them.
"""
function handle_interrupt(io::IO, id, payload)
    ev = CURRENT_EVAL[]
    named = get(payload, :eval_ids, nothing)
    if ev === nothing || istaskdone(ev.task) || ev.stage === :done ||
       (named !== nothing && !(ev.id in named))
        note = named === nothing ? "no eval in progress" : "none of the named evals is in progress"
        write_envelope(io, "res", id, "repl.interrupt", Dict(:interrupted => false, :note => note))
        return
    end
    if ev.stage === :running
        schedule(ev.task, InterruptException(); error = true)
    else
        ev.stage = :held
    end
    write_envelope(io, "res", id, "repl.interrupt", Dict(:interrupted => true))
end

function write_envelope(io::IO, kind, id, op, payload)
    env = Dict(:v => PROTOCOL_VERSION, :id => id, :kind => kind, :op => op, :payload => payload)
    lock(OUT_LOCK) do
        json_write(io, env)
        write(io, '\n')
        flush(io)
    end
end

end # module
