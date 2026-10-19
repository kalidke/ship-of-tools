# The daemon's interrupt signal against the shim as the daemon starts it: a child process running `run_shim`, sent the
# signal the daemon sends (rust/backend/src/lifecycle/contain.rs, `interrupt`) in the steps it sends it
# (rust/backend/src/sidecars/repl/supervisor.rs, `Escalation`).
using Test
using ShipToolsRepl

# The child and its wire lines, which arrive on a channel. On Windows it runs in a console process group of its own
# (`detach`), as the daemon starts its children (CREATE_NEW_PROCESS_GROUP), so a Ctrl+Break reaches it alone.
function interrupt_child()
    script = "using ShipToolsRepl; ShipToolsRepl.run_shim(stdin, stdout)"
    cmd = Cmd(`$(Base.julia_cmd()) --startup-file=no --project=$(pkgdir(ShipToolsRepl)) -e $script`;
        detach = Sys.iswindows())
    proc = open(pipeline(cmd; stderr = devnull), "r+")
    lines = Channel{String}(Inf)
    @async try
        while !eof(proc)
            put!(lines, readline(proc))
        end
    finally
        close(lines)
    end
    @async (sleep(240); process_running(proc) && kill(proc))
    return proc, lines
end

# The daemon's interrupt signal: SIGINT to the child on Unix, a Ctrl+Break to its console process group on Windows.
function signal_interrupt(proc)
    if Sys.iswindows()
        ok = ccall((:GenerateConsoleCtrlEvent, "Kernel32"), stdcall, Cint, (UInt32, UInt32), 1, getpid(proc))
        ok != 0 || error("GenerateConsoleCtrlEvent failed: $(Libc.GetLastError())")
    else
        kill(proc, Base.SIGINT)
    end
end

# The daemon's force: five signals 50 ms apart, the count at which Julia forces the exception into code with no safepoint.
function signal_burst(proc)
    for _ in 1:5
        signal_interrupt(proc)
        sleep(0.05)
    end
end

send(proc, line) = (write(proc, line); flush(proc))
child_interrupt_line(id) = sprint(ShipToolsRepl.json_write, Dict(:v => 1, :id => id, :op => "repl.interrupt",
    :payload => Dict())) * "\n"

# Every envelope until `done(envs)`, the wire closes, or `secs` pass.
function envelopes_until(done, lines, secs)
    envs = Any[]
    deadline = time() + secs
    while !done(envs) && time() < deadline
        isready(lines) || (isopen(lines) ? (sleep(0.02); continue) : break)
        push!(envs, ShipToolsRepl.json_read(take!(lines)))
    end
    return envs
end
child_frames(envs, n) = [e[:payload][:frame] for e in envs if get(e, :kind, "") == "evt" && get(e, :id, 0) == n]
answered(envs, n) = any(e -> get(e, :kind, "") == "res" && get(e, :id, 0) == n, envs)
child_res(envs, n) = only(e[:payload] for e in envs if get(e, :kind, "") == "res" && get(e, :id, 0) == n)
values_of(envs, n) = [f[:text] for f in child_frames(envs, n) if f[:kind] == "value"]
# Every envelope until each request in `ids` is answered.
answered_within(lines, ids, secs) = envelopes_until(envs -> all(n -> answered(envs, n), ids), lines, secs)
# Whether request `n` prints `text` within two minutes, so its code is running.
printed(lines, n, text) = any(f -> f[:kind] == "stdout" && occursin(text, f[:text]),
    child_frames(envelopes_until(envs -> any(f -> f[:kind] == "stdout" && occursin(text, f[:text]),
        child_frames(envs, n)), lines, 120), n))

@testset "run_shim: one signal stops code with a safepoint, the force stops code with none, and the REPL keeps its state" begin
    proc, lines = interrupt_child()
    @test ShipToolsRepl.json_read(take!(lines))[:op] == "repl.ready"
    send(proc, eval_line(1, "kept = 41"))
    @test values_of(answered_within(lines, [1], 120), 1) == ["41"]
    stopped(envs, n) = [f[:kind] for f in child_frames(envs, n) if f[:kind] in ("error", "done")] == ["error", "done"]
    # A safepoint in the loop: one signal stops it, and the request, read afterwards, is answered for it.
    send(proc, eval_line(2, "println(\"summing\"); let s = 0.0; for i in 1:10^12; s += sin(i); end; s; end"))
    @test printed(lines, 2, "summing")
    send(proc, child_interrupt_line(3))
    signal_interrupt(proc)
    envs = answered_within(lines, [2, 3], 30)
    @test stopped(envs, 2)
    @test occursin("InterruptException", only(f for f in child_frames(envs, 2) if f[:kind] == "error")[:message])
    @test child_res(envs, 3)[:interrupted] == true
    # No safepoint: one signal leaves it running; the burst forces it.
    send(proc, eval_line(4, "println(\"spinning\"); while true end"))
    @test printed(lines, 4, "spinning")
    send(proc, child_interrupt_line(5))
    signal_interrupt(proc)
    envs = answered_within(lines, [4, 5], 3)
    @test !answered(envs, 4)
    signal_burst(proc)
    envs = answered_within(lines, [4, 5], 30)
    @test stopped(envs, 4)
    @test child_res(envs, 5)[:interrupted] == true
    send(proc, eval_line(7, "kept + 1"))
    @test values_of(answered_within(lines, [7], 30), 7) == ["42"]
    close(proc.in)
    wait(proc)
end

# The signal can come as the code it was for ends (`Escalation` stops at the child's first line, but one already sent
# lands after it): the shim drops it, here while it waits for the next request. Julia 1.12 raises a signal that comes
# while the thread idles in a finished task's end, where nothing can catch it, and the REPL ends
# (`fatal: error thrown and no exception handler available`); 1.13 raises it in the next task that runs.
VERSION >= v"1.13" && @testset "run_shim: an interrupt signal that comes between evals is dropped, and the REPL serves on" begin
    proc, lines = interrupt_child()
    @test ShipToolsRepl.json_read(take!(lines))[:op] == "repl.ready"
    send(proc, eval_line(1, "kept = 41"))
    @test values_of(answered_within(lines, [1], 120), 1) == ["41"]
    signal_interrupt(proc)
    sleep(0.5)
    send(proc, eval_line(2, "kept + 1"))
    @test values_of(answered_within(lines, [2], 30), 2) == ["42"]
    close(proc.in)
    wait(proc)
end
