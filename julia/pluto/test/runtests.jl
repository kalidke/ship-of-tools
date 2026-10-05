# ADR 0049, User isolation: the daemon's Pluto runs every notebook in a worker that no other account can talk to. Run
# with `julia --project=julia/pluto julia/pluto/test/runtests.jl` (Pluto is this folder's dependency, so it is not a
# Pkg.test package); CI runs it on Linux, macOS and Windows.

using Test
using Sockets
using Distributed
using Pluto

const Malt = Pluto.Malt
include(joinpath(@__DIR__, "..", "session_options.jl"))

const DISTRIBUTED = Base.PkgId(Base.UUID("8ba89e20-285c-5b6f-9357-94700520ee1b"), "Distributed")

# The bytes a client sends a Distributed worker to make it run `f(args...)`: the cookie and the version, then one call
# message and its boundary (Distributed's own wire format; the same bytes a legitimate peer sends).
function distributed_call(cookie::AbstractString, f, args)
    io = IOBuffer()
    write(io, rpad(cookie, Distributed.HDR_COOKIE_LEN)[1:Distributed.HDR_COOKIE_LEN])
    write(io, rpad(Distributed.VERSION_STRING, Distributed.HDR_VERSION_LEN)[1:Distributed.HDR_VERSION_LEN])
    Distributed.serialize_hdr_raw(io, Distributed.MsgHeader())
    Distributed.serialize_msg(
        Distributed.ClusterSerializer(io),
        Distributed.CallMsg{:call}(f, args, pairs(NamedTuple())),
    )
    write(io, Distributed.MSG_BOUNDARY)
    return take!(io)
end

# The bytes a client sends a Malt worker (Pluto 0.20's default) to make it run `f(args...)`: Malt's wire is a message
# type, an id and the serialized call, with no handshake at all.
function malt_call(f, args)
    io = IOBuffer()
    Malt._serialize_msg(io, Malt.MsgType.from_host_call_with_response, UInt64(1), (f, args, (;), true))
    return take!(io)
end

"Send `bytes` to `port` as another account's process would; whether the worker closed the connection within `wait` s."
function stranger_is_dropped(port::Integer, bytes::Vector{UInt8}; wait = 10.0)
    sock = Sockets.connect(ip"127.0.0.1", port)
    try
        write(sock, bytes)
        flush(sock)
        closed = @async try
            eof(sock)
        catch
            true
        end
        return timedwait(() -> istaskdone(closed), wait) == :ok
    finally
        close(sock)
    end
end

# Linux: the (port, loopback) of every TCP socket this user is listening on, from the kernel's tables.
function listeners()
    found = Set{Tuple{Int,Bool}}()
    uid = Int(ccall(:geteuid, Cint, ()))
    for (file, loopback) in (("/proc/net/tcp", "0100007F"), ("/proc/net/tcp6", "00000000000000000000000001000000"))
        for line in Iterators.drop(eachline(file), 1)
            f = split(line)
            length(f) >= 8 && f[4] == "0A" && parse(Int, f[8]) == uid || continue
            address, port = split(f[2], ':')
            push!(found, (parse(Int, port; base = 16), address == loopback))
        end
    end
    return found
end

@testset "the cluster cookie is drawn per session from the OS's generator" begin
    cookies = String[]
    for _ in 1:2
        configure_session!(Pluto.ServerSession(), "127.0.0.1", 1234)
        push!(cookies, Distributed.cluster_cookie())
    end
    @test all(c -> occursin(r"^[A-Za-z0-9]{16}$", c), cookies)
    @test cookies[1] != cookies[2]
end

@testset "Pluto's notebook workers" begin
    dir = mktempdir()
    session = Pluto.ServerSession()
    configure_session!(session, "127.0.0.1", 1234)
    cookie = Distributed.cluster_cookie()

    notebook_path = joinpath(dir, "owner.jl")
    Pluto.save_notebook(Pluto.Notebook([Pluto.Cell("x = 20 + 1")], notebook_path))
    listening_before = Sys.islinux() ? listeners() : nothing
    notebook = Pluto.SessionActions.open(session, notebook_path; run_async = false)
    try
        workspace = Pluto.WorkspaceManager.get_workspace((session, notebook))

        @testset "the notebook runs in a Distributed worker, for its owner" begin
            @test workspace.worker isa Malt.DistributedStdlibWorker
            @test notebook.cells[1].output.body == "21"
        end

        port = try
            Int(Malt.remote_eval_fetch(workspace.worker, :(getfield(Base.loaded_modules[$DISTRIBUTED], :LPROC).bind_port)))
        catch
            0
        end
        if listening_before !== nothing
            @testset "opening a notebook starts exactly one listener: the worker's, on loopback" begin
                opened = setdiff(listeners(), listening_before)
                @test opened == Set([(port, true)])
            end
        end

        @testset "the worker holds the cookie configure_session! set" begin
            @test Malt.remote_eval_fetch(workspace.worker, :(getfield(Base.loaded_modules[$DISTRIBUTED], :LPROC).cookie)) == cookie
        end

        @testset "the worker listens on loopback" begin
            @test port > 0
            @test Malt.remote_eval_fetch(workspace.worker, :(getfield(Base.loaded_modules[$DISTRIBUTED], :LPROC).bind_addr)) == "127.0.0.1"
        end

        ran_right = joinpath(dir, "right-cookie")
        ran_wrong = joinpath(dir, "wrong-cookie")
        ran_malt = joinpath(dir, "malt-call")
        if port > 0
            @testset "a call with the cluster cookie runs (the control: the crafted call is well-formed)" begin
                stranger_is_dropped(port, distributed_call(Distributed.cluster_cookie(), Base.write, (ran_right, "ran")); wait = 2.0)
                @test timedwait(() -> isfile(ran_right), 20.0) == :ok
            end

            @testset "a stranger's well-formed call runs nothing" begin
                @test stranger_is_dropped(port, distributed_call("not-the-cookie--", Base.write, (ran_wrong, "ran")))
                @test stranger_is_dropped(port, malt_call(Base.write, (ran_malt, "ran")))
                sleep(2.0)
                @test !isfile(ran_wrong)
                @test !isfile(ran_malt)
            end
        end

        @testset "the notebook still evaluates for its owner after the strangers" begin
            @test Pluto.WorkspaceManager.eval_fetch_in_workspace((session, notebook), :(1 + 1)) == 2
        end
    finally
        Pluto.SessionActions.shutdown(session, notebook; keep_in_session = false, async = false)
    end
end
