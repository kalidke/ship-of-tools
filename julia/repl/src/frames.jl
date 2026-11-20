# How an eval's output becomes typed frames: stream_eval_frames, pipe capture, value frames,
# BrowserView announcements, and the wire guard (`EvalStdout`, `point_stdout_away_from_wire!`).

# An IO that writes to whatever `stdout` is at the time of the write: during an eval, its capture pipe.
struct EvalStdout <: IO end
Base.unsafe_write(::EvalStdout, p::Ptr{UInt8}, n::UInt) = unsafe_write(stdout, p, n)
Base.write(::EvalStdout, b::UInt8) = write(stdout, b)
Base.flush(::EvalStdout) = flush(stdout)
# `display(x)` takes the color of the eval's stdout, as `show` into it does.
Base.get(::EvalStdout, key, default) = get(stdout, key, default)

# Only the shim's own locked writer reaches the wire (`io_out`, which holds its own descriptor). Julia's startup
# `TextDisplay(stdout)` holds the wire, so each is swapped, in place so the stack's order holds, for a real `TextDisplay`
# (packages find the stack's fallback by that type) on an `EvalStdout`; then the process's stdout (Julia's and fd 1)
# becomes its stderr, which the supervisor drains, so a display or `println` outside an eval cannot glue onto an
# envelope. `run_shim` does it before the user's startup file.
function point_stdout_away_from_wire!(io_out::IO)
    ds = Base.Multimedia.displays
    for i in eachindex(ds)
        d = ds[i]
        d isa TextDisplay && d.io === io_out && (ds[i] = TextDisplay(EvalStdout()))
    end
    io_out === stdout && redirect_stdout(stderr)
    return nothing
end

"""
    stream_eval_frames(f, emit)

Run `f()` with stdout/stderr captured via async-drained pipes, emitting each
output chunk as a `stdout`/`stderr` frame *as it arrives* (incremental), then a
trailing `value` (last expression's result via `MIME"text/plain"`) or `error`
frame (if `f()` threw — including `InterruptException` from `repl.interrupt`).
`f` is first so call sites can pass it as a `do` block (`stream_eval_frames(emit) do … end`). Whether the eval takes color is `EVAL_COLOR[]`, which the
entry point that accepted the eval set and which is read for the whole eval: no argument or type parameter of this function or of the ones above it names it, because each is
printed in every stack trace user code prints or an agent reads.

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

# Whether the eval about to run is a person's and takes color: set by the entry point that accepted it (`handle_eval`,
# `handle_run_file`, after the single-eval guard: every eval is accepted by one of them, so each sets it afresh) and read by
# `stream_eval_frames` and `value_frames_for`. A
# state the eval reads, not an argument, because every argument of the functions the user's code runs under is printed in
# the stack an agent reads: this keeps that stack what it was.
const EVAL_COLOR = Ref(false)
const ANNOUNCED_BROWSER_URLS = Set{String}()

# Dedupe key for the per-eval announce set. Keyed on (url, open) — not url
# alone — so an explicit LATER open policy is never swallowed (codex review):
# `wglshow(fig; open=false)` followed by returning `browserview(url)` in the
# same eval must still emit the open=true frame (one no-open announce for the
# allowlist, then one deliberate open), and vice versa. Identical-policy
# repeats still dedupe to one frame. The key includes the target.
browser_announce_key(bv::BrowserView) = string(bv.url, '|', bv.open, '|', something(bv.fe, ""))

# The `browser` frame for `bv`; `fe` is present only when the frame names its one frontend.
browser_frame(bv::BrowserView) = bv.fe === nothing ?
    Dict(:kind => "browser", :url => bv.url, :open => bv.open) :
    Dict(:kind => "browser", :url => bv.url, :open => bv.open, :fe => bv.fe)

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
        em(browser_frame(bv))
        push!(ANNOUNCED_BROWSER_URLS, browser_announce_key(bv))
    catch e
        e isa InterruptException && rethrow()
        # Emission is best-effort: a failed announce must not break the serve
        # itself — the value-position path still covers a returned BrowserView.
    end
    return bv
end

# Revise only revises on its own in a Julia REPL, which the shim is not: when the user's startup file or an earlier
# eval loaded it into Main, revise before the eval, with the eval's stdout/stderr already captured, so a failed revision
# (Revise logs it, it does not throw) is a stderr frame of that eval. The lookup and the call go through `invokelatest`:
# `serve`'s world is older than the `using Revise` that created the binding. A throw is a stderr line; the eval still runs.
function revise_loaded()
    try
        Base.invokelatest() do
            isdefined(Main, :Revise) || return
            rev = getglobal(Main, :Revise)
            rev isa Module && rev.revise()
        end
    catch e
        if e isa InterruptException
            # The daemon's signal landed before the eval's code started: the eval stops before it runs, as an interrupt.
            ev = CURRENT_EVAL[]
            ev !== nothing && ev.task === current_task() && ev.stage === :waiting && (ev.stage = :held)
            return
        end
        println(stderr, "sot: Revise.revise() failed: ", Base.invokelatest(sprint, showerror, e))
    end
end

function stream_eval_frames(f, emit)
    color = EVAL_COLOR[]
    CURRENT_EMIT[] = emit
    empty!(ANNOUNCED_BROWSER_URLS)
    pipe_out = Pipe()
    pipe_err = Pipe()
    Base.link_pipe!(pipe_out; reader_supports_async = true, writer_supports_async = true)
    Base.link_pipe!(pipe_err; reader_supports_async = true, writer_supports_async = true)

    old_stdout = stdout
    old_stderr = stderr
    drop_held_signal()
    reader_out = @async held_task(() -> stream_pipe(pipe_out, emit, "stdout"; hold = color))
    reader_err = @async held_task(() -> stream_pipe(pipe_err, emit, "stderr"; hold = color))

    frames = Dict[]
    threw = nothing
    local_bt = Base.StackTraces.StackFrame[]
    try
        # An eval a person typed says its streams take color, as a terminal's do, so the logging macros, `printstyled` and
        # the packages that ask `get(stdout, :color, false)` print it; the window draws it. The write end is wrapped, so
        # the streams stay as readable (or not) as before. An eval an agent submitted runs as it always did.
        redirect_stdout(color ? IOContext(pipe_out.in, :color => true) : pipe_out)
        redirect_stderr(color ? IOContext(pipe_err.in, :color => true) : pipe_err)
        revise_loaded()
        try
            # The value's display is user code too, so it renders in the
            # interruptible region; its frames are emitted after the drain.
            frames = run_user_code() do
                result = f()
                result === nothing ? Dict[] : value_frames_for(result)
            end
        catch e
            threw = e
            local_bt = past_interrupts(() -> stacktrace(catch_backtrace()))
        end
    finally
        # A signal that lands in this restoring (the daemon's, for code that ended as it came) must not leave the
        # process's streams on the finished eval's pipes: every step is safe to run again.
        past_interrupts() do
            redirect_stdout(old_stdout)
            redirect_stderr(old_stderr)
            close(pipe_out.in)
            close(pipe_err.in)
            # After user code returns, later frames (value/error/done) go through
            # the local `emit` — mid-eval announcing is over.
            CURRENT_EMIT[] = nothing
        end
    end
    # Drain whatever the readers haven't emitted yet (the close above lets them
    # hit eof). Ordering: all stdout/stderr frames precede value/error.
    past_interrupts(() -> wait(reader_out))
    past_interrupts(() -> wait(reader_err))

    if threw !== nothing
        stack = [Dict(
            :file => string(s.file),
            :line => s.line,
            :fn => string(s.func),
        ) for s in local_bt]
        # invokelatest: `serve`'s world is older than the startup file and every eval, which may define the `showerror`.
        msg = threw isa InterruptException ?
            "InterruptException: eval interrupted by repl.interrupt" :
            past_interrupts(() -> Base.invokelatest(sprint, showerror, threw; context = color ? (:color => true) : nothing))
        past_interrupts(() -> emit(Dict(:kind => "error", :message => msg, :stacktrace => stack)))
    else
        for fr in frames
            past_interrupts(() -> emit(fr))
        end
    end
    return nothing
end

"""
    stream_pipe(pipe, emit, kind; hold = false)

Drain `pipe` to eof, emitting `{kind, text}` frames as bytes arrive. Only a
character that a `readavailable` chunk cut waits for its other bytes
(`utf8_prefix`); any other invalid byte is program output and goes on at once,
and `write_json_string` writes it as U+FFFD. With `hold` (an eval that prints color) a terminal escape sequence the
chunk ends in the middle of waits for the rest too (`escape_prefix`, the grammar's one forward pass), so no frame splits one.
Nothing waits longer than the program is quiet for `ESCAPE_HOLD_SECONDS` or than `MAX_ESCAPE_SEQUENCE` bytes of the
stream, whichever comes first, so a person's output after an opener that never ends appears within a tenth of a second.
An interrupt that lands here (the daemon's signal, sent for the eval's code as that code ended) does not stop the drain.
"""
function stream_pipe(pipe, emit, kind; hold::Bool = false)
    leftover = UInt8[]
    holding = false  # `leftover` starts with an unfinished escape sequence, waiting for the rest
    emitted = 0      # the bytes of the stream already sent in frames: where what is held begins
    held_at = -1     # where the sequence now held began, or -1
    held_since = 0.0 # when that sequence began to wait: the hold's time is counted from there, not from the last byte
    waiter = nothing # a task waiting for the pipe's next byte, while the hold's time runs
    frame = nothing  # a chunk read and not yet emitted: kept across an interrupt, so it is emitted again, never lost
    while true
        try
            if frame === nothing && holding
                waiter === nothing && (waiter = @async eof(pipe))
                if timedwait(() -> istaskdone(waiter), max(0.0, ESCAPE_HOLD_SECONDS[] - (time() - held_since)); pollint = 0.01) === :timed_out
                    # The program wrote nothing more in time: what waited is text, and goes on as it is.
                    s, leftover = utf8_prefix(leftover)
                    holding = false
                    isempty(s) || (frame = Dict(:kind => kind, :text => s))
                end
            end
            if frame === nothing
                (waiter === nothing ? eof(pipe) : fetch(waiter)) && break
                waiter = nothing
                data = readavailable(pipe)
                isempty(data) && continue
                append!(leftover, data)
                s, leftover = utf8_prefix(leftover)
                if hold
                    s, held = escape_prefix(s)
                    # The same sequence still waiting (it begins where the one held before did) keeps its time; a sequence
                    # that begins elsewhere, after the first was let go or finished, starts its own.
                    at = emitted + ncodeunits(s)
                    isempty(held) || at == held_at || (held_since = time())
                    held_at = isempty(held) ? -1 : at
                    holding = !isempty(held)
                    leftover = vcat(Vector{UInt8}(codeunits(held)), leftover)
                end
                isempty(s) || (frame = Dict(:kind => kind, :text => s))
            end
            if frame !== nothing
                emit(frame)
                emitted += ncodeunits(frame[:text])
                frame = nothing
            end
        catch e
            e isa InterruptException && continue
            break  # pipe closed/errored mid-read — stop draining.
        end
    end
    if !isempty(leftover)
        emit(Dict(:kind => kind, :text => String(leftover)))
    end
    return nothing
end

# The longest terminal escape sequence, in bytes from its ESC through its last: `sot_protocol::escapes::MAX_SEQUENCE`,
# whose grammar `escape_open_from` reads. A longer one is data.
const MAX_ESCAPE_SEQUENCE = 4096

# How long text held for an unfinished escape sequence waits for the rest before it goes on as it is.
const ESCAPE_HOLD_SECONDS = Ref(0.1)

# Where `text` ends in the middle of a terminal escape sequence the next chunk could finish: the byte offset of its ESC,
# or the length of `text` when it ends in none. The one forward pass of `sot_protocol::escapes::open_tail`, which the
# cases in `test/escape_cases.tsv` and `test/escape_random.tsv` hold both to: from the start, each ESC is a whole
# sequence (skipped to its end, so an opener inside it is its own and no tail), or data (the scan goes on after it,
# after a string's introducer), or the first it cannot tell yet, the text ending with room left within the bound.
function escape_open_from(text::AbstractString)
    b = codeunits(text)
    n = length(b)
    B(k) = @inbounds b[k + 1]                   # the byte at 0-based offset k
    st = SeekCache()
    bel = SeekCache()
    open = n
    i = 0
    while true
        e = findnext(==(0x1b), b, i + 1)
        e === nothing && return open
        pos = e - 1
        limit = min(pos + MAX_ESCAPE_SEQUENCE, n)
        room = n - pos < MAX_ESCAPE_SEQUENCE        # a terminator could still fit within the bound
        next = pos + 1
        if pos + 1 >= n
            open == n && (open = pos)               # an ESC alone at the end
        else
            c = B(pos + 1)
            if c == UInt8('[')
                j = pos + 2
                while j < limit && 0x30 <= B(j) <= 0x3f; j += 1; end
                while j < limit && 0x20 <= B(j) <= 0x2f; j += 1; end
                if j == limit
                    room && open == n && (open = pos)
                elseif 0x40 <= B(j) <= 0x7e
                    next = j + 1                    # a whole CSI
                end
            elseif c in (UInt8(']'), UInt8('P'), UInt8('X'), UInt8('^'), UInt8('_'))
                by_st = seek!(st, b, pos + 2, k -> B(k) == 0x1b && k + 1 < n && B(k + 1) == UInt8('\\'))
                by_bel = c == UInt8(']') ? seek!(bel, b, pos + 2, k -> B(k) == 0x07) : nothing
                ends = filter(!isnothing, (by_st === nothing ? nothing : by_st + 2, by_bel === nothing ? nothing : by_bel + 1))
                if isempty(ends)
                    room && open == n && (open = pos)
                    next = pos + 2
                elseif minimum(ends) - pos <= MAX_ESCAPE_SEQUENCE
                    next = minimum(ends)            # a whole string
                else
                    next = pos + 2
                end
            elseif 0x20 <= c <= 0x2f
                j = pos + 2
                while j < limit && 0x20 <= B(j) <= 0x2f; j += 1; end
                if j == limit
                    room && open == n && (open = pos)
                elseif 0x30 <= B(j) <= 0x7e
                    next = j + 1
                end
            elseif 0x30 <= c <= 0x7e
                next = pos + 2                      # ESC and one final byte
            end
        end
        i = next
    end
end

# Where the next byte satisfying `hit` is, at or after 0-based `from`, searched forward only: the position found stays
# the answer until the scan passes it, and "none after here" is kept, so however many openers ask, the text is read once.
mutable struct SeekCache
    searched_from::Int
    found::Union{Int,Nothing}
    known::Bool
    SeekCache() = new(0, nothing, false)
end

function seek!(c::SeekCache, b, from::Int, hit)
    if c.known && from >= c.searched_from && (c.found === nothing || c.found >= from)
        return c.found
    end
    c.found = nothing
    for k in from:(length(b) - 1)
        if hit(k)
            c.found = k
            break
        end
    end
    c.searched_from = from
    c.known = true
    return c.found
end

# Split `s` into (its text up to the tail it ends in the middle of a terminal escape sequence at, that tail): no frame
# splits a sequence, and the tail waits for the rest (at most `ESCAPE_HOLD_SECONDS`, and no more than the bound's bytes).
function escape_prefix(s::AbstractString)
    k = escape_open_from(s)
    k == ncodeunits(s) && return (String(s), "")
    b = codeunits(s)
    return (String(b[1:k]), String(b[(k + 1):end]))
end

# Split `buf` into (its bytes up to an incomplete character at its end, as a
# String; that incomplete tail). A UTF-8 character is at most 4 bytes, so the
# tail is a lead byte in the last 3 that needs more bytes than follow it. An
# invalid byte anywhere else stays in the String: holding it would hold back
# every later byte of the stream until it ended.
function utf8_prefix(buf::Vector{UInt8})
    n = length(buf)
    for k in 1:min(3, n)
        b = buf[n - k + 1]
        b & 0xc0 == 0x80 && continue                # a continuation byte: its lead is further back
        need = b >= 0xf0 ? 4 : b >= 0xe0 ? 3 : b >= 0xc0 ? 2 : 1
        need > k && return (String(buf[1:(n - k)]), buf[(n - k + 1):end])
        break
    end
    return (String(copy(buf)), UInt8[])
end

"""
    value_frames_for(result) -> Vector{Dict}

Render the last-expression value into one or more frames. Prefers image-bearing
MIMEs (`image/png`, `image/svg+xml`) when `showable` says they apply — that's
what makes CairoMakie figures, `Plots.Plot`s, etc. flow through as `image`
frames. Falls through to `text/plain`.
"""
function value_frames_for(result)
    color = EVAL_COLOR[]
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
        push!(out, browser_frame(result))
        return out
    end
    img_mimes = (MIME"image/png"(), MIME"image/svg+xml"())
    # invokelatest because the eval may have defined the showable/show methods
    # itself (e.g. `using CairoMakie` adds Figure showables mid-eval).
    for m in img_mimes
        is_showable = try
            Base.invokelatest(showable, m, result)
        catch e
            e isa InterruptException && rethrow()
            false
        end
        is_showable || continue
        buf = IOBuffer()
        ok = try
            Base.invokelatest(show, buf, m, result)
            true
        catch e
            e isa InterruptException && rethrow()
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
        Base.invokelatest(show, color ? IOContext(valbuf, :color => true) : valbuf, MIME"text/plain"(), result)
    catch e
        e isa InterruptException && rethrow()
        print(valbuf, "<unshowable $(typeof(result)): $(Base.invokelatest(sprint, showerror, e))>")
    end
    push!(out, Dict(
        :kind => "value",
        :mime => "text/plain",
        :text => String(take!(valbuf)),
    ))
    out
end
