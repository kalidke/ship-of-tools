# How an eval's output becomes typed frames: stream_eval_frames, pipe capture, value frames,
# BrowserView announcements.

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
# The current eval's frame emitter, exposed so serving helpers (wglshow) can
# emit a `browser` frame AT SERVE TIME instead of relying on the BrowserView
# reaching the eval's last-value position. A wrapper API that swallows the
# return value (`serve(...)` returning its own state object) otherwise makes
# the serve INVISIBLE: no browser frame → the daemon never allowlists the
# port for the ADR-0035 proxy and the FE never opens the page (field report,
# 2026-07-23 — a served picker on an ephemeral fallback port was unreachable
# for exactly this reason). Safe as a single Ref: the shim enforces one eval
# at a time (`eval_in_progress`). `ANNOUNCED_BROWSER_URLS` dedups so a
# BrowserView that IS also returned doesn't open the browser twice.
const CURRENT_EMIT = Ref{Union{Function,Nothing}}(nothing)
const ANNOUNCED_BROWSER_URLS = Set{String}()

# Dedupe key for the per-eval announce set. Keyed on (url, open) — not url
# alone — so an explicit LATER open policy is never swallowed (codex review):
# `wglshow(fig; open=false)` followed by returning `browserview(url)` in the
# same eval must still emit the open=true frame (one no-open announce for the
# allowlist, then one deliberate open), and vice versa. Identical-policy
# repeats still dedupe to one frame.
browser_announce_key(bv::BrowserView) = string(bv.url, '|', bv.open)

"""
    announce_browserview(bv::BrowserView) -> bv

Emit `bv` as a `browser` frame NOW (mid-eval), so the FE opens it and the
daemon allowlists its port even if the surrounding call swallows the return
value. No-op outside a streamed eval. Idempotent per eval per (URL, open).
"""
function announce_browserview(bv::BrowserView)
    em = CURRENT_EMIT[]
    em === nothing && return bv
    browser_announce_key(bv) in ANNOUNCED_BROWSER_URLS && return bv
    try
        em(Dict(:kind => "browser", :url => bv.url, :open => bv.open))
        push!(ANNOUNCED_BROWSER_URLS, browser_announce_key(bv))
    catch
        # Emission is best-effort: a failed announce must not break the serve
        # itself — the value-position path still covers a returned BrowserView.
    end
    return bv
end

function stream_eval_frames(f, emit)
    CURRENT_EMIT[] = emit
    empty!(ANNOUNCED_BROWSER_URLS)
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
        # After user code returns, later frames (value/error/done) go through
        # the local `emit` — mid-eval announcing is over.
        CURRENT_EMIT[] = nothing
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
`readavailable` chunks isn't emitted as invalid UTF-8 (`utf8_prefix` checks
`isvalid` before releasing a chunk, so a split character waits for its other
half rather than becoming a malformed `String` that `json_write` would then
serialize as garbage).
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
        # Skip if this (URL, open) was already announced mid-eval (wglshow's
        # serve-time announce) — a second identical browser frame would open
        # a second tab. Keyed on the policy too: a returned BrowserView with
        # a DIFFERENT `open` than the announce is a deliberate override and
        # still emits (codex review). A hand-built BrowserView (plain
        # `browserview(url)` as the last expression) was never announced and
        # still emits here.
        browser_announce_key(result) in ANNOUNCED_BROWSER_URLS && return out
        push!(out, Dict(:kind => "browser", :url => result.url, :open => result.open))
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
