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

# Linux: `pid` and every process descended from it.
# The command lines of `pids` that carry `needle`, and how many lines were read. A process that exited between the walk
# and the read is skipped; any other unreadable command line is an error, so an unreadable one cannot pass as clean.
function cmdline_carriers(pids, needle::AbstractString)
    carriers = Int[]
    read_count = 0
    for p in pids
        cmdline = try
            read("/proc/$p/cmdline", String)
        catch
            isdir("/proc/$p") && error("the command line of process $p, still in the tree, cannot be read")
            continue
        end
        read_count += 1
        occursin(needle, cmdline) && push!(carriers, p)
    end
    return carriers, read_count
end

function process_tree(pid::Integer)
    parent_of = Dict{Int,Int}()
    for entry in readdir("/proc")
        all(isdigit, entry) || continue
        try
            stat = read("/proc/$entry/stat", String)
            # "pid (comm) state ppid ...": comm may hold spaces and parentheses, so split after the last ')'.
            parent_of[parse(Int, entry)] = parse(Int, split(stat[last(findlast(')', stat)) + 2:end])[2])
        catch
        end
    end
    tree = Set{Int}([pid])
    grew = true
    while grew
        grew = false
        for (child, parent) in parent_of
            if parent in tree && !(child in tree)
                push!(tree, child)
                grew = true
            end
        end
    end
    return tree
end

if Sys.islinux()
    # The observation can fail: a needle in a descendant's arguments is found, and one nothing carries is not.
    @testset "the command-line reading finds a needle in a descendant" begin
        needle = "probe-needle-" * "5d1e"
        child = run(`sh -c "sleep 60; :" $needle`; wait = false)
        try
            carriers, read_count = cmdline_carriers(process_tree(getpid()), needle)
            @test Base.getpid(child) in carriers
            @test read_count >= 1
            @test isempty(first(cmdline_carriers(process_tree(getpid()), "a-needle-nothing-carries")))
        finally
            kill(child)
            wait(child)
        end
    end
end


# Linux: the (port, loopback) of every TCP socket in state LISTEN whose inode a process of `pid`'s tree holds open:
# what that tree listens on, and not what the rest of the account's processes do meanwhile.
function tree_listeners(pid::Integer)
    @test isdir("/proc/$pid")   # a Linux run that cannot read /proc fails; it never skips
    inodes = Set{String}()
    for p in process_tree(pid), fd in (try readdir("/proc/$p/fd") catch; String[] end)
        target = try readlink("/proc/$p/fd/$fd") catch; "" end
        m = match(r"^socket:\[(\d+)\]$", target)
        m === nothing || push!(inodes, m.captures[1])
    end
    found = Set{Tuple{Int,Bool}}()
    for (file, loopback) in (("/proc/net/tcp", "0100007F"), ("/proc/net/tcp6", "00000000000000000000000001000000"))
        for line in Iterators.drop(eachline(file), 1)
            f = split(line)
            length(f) >= 10 && f[4] == "0A" && f[10] in inodes || continue
            address, port = split(f[2], ':')
            push!(found, (parse(Int, port; base = 16), address == loopback))
        end
    end
    return found
end

@testset "the session secret is drawn per session from the OS's generator" begin
    secrets = String[]
    for _ in 1:2
        session = Pluto.ServerSession()
        configure_session!(session, "127.0.0.1", 1234)
        push!(secrets, session.secret)
    end
    @test all(s -> occursin(r"^[0-9a-f]{32}$", s), secrets)
    @test secrets[1] != secrets[2]
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
    listening_before = Sys.islinux() ? tree_listeners(getpid()) : nothing
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
                opened = setdiff(tree_listeners(getpid()), listening_before)
                @test opened == Set([(port, true)])
            end
        end

        if Sys.islinux()
            @testset "the cluster cookie is on no command line of the worker's tree" begin
                worker_pid = Int(Malt.remote_eval_fetch(workspace.worker, :(getpid())))
                tree = process_tree(getpid())
                carriers, read_count = cmdline_carriers(tree, cookie)
                @test isempty(carriers)
                @test read_count >= 1
                @test worker_pid in tree
                @test worker_pid in tree && !isempty(first(cmdline_carriers([worker_pid], "julia")))
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

# start.jl is the process the daemon runs: its stdio protocol, its page and its listeners, over its own process tree.
@testset "start.jl end to end" begin
    dir = mktempdir()
    notebook_path = joinpath(dir, "owner.jl")
    Pluto.save_notebook(Pluto.Notebook([Pluto.Cell("x = 20 + 1")], notebook_path))
    start = joinpath(@__DIR__, "..", "start.jl")
    stderr_log = joinpath(dir, "start.stderr")
    child = open(pipeline(Cmd(`$(Base.julia_cmd()) --project=$(joinpath(@__DIR__, "..")) $start`); stderr = stderr_log), "r+")
    # Read lines until one starts with `prefix` (Pluto may print before it), within `wait` seconds.
    function line_with(prefix; wait = 180.0)
        found = Channel{Union{String,Nothing}}(1)
        @async try
            for line in eachline(child)
                startswith(line, prefix) && (put!(found, line); return)
            end
            put!(found, nothing)
        catch
            put!(found, nothing)
        end
        timedwait(() -> isready(found), wait) == :ok || return nothing
        return take!(found)
    end
    try
        ready = line_with("READY http://127.0.0.1:")
        @test ready !== nothing
        port = parse(Int, last(split(ready, ':')))
        write(child, "OPEN $notebook_path\n")
        flush(child)
        url = line_with("URL ")
        @test url !== nothing
        secret = match(r"secret=([0-9a-f]{32})&", url).captures[1]
        id = match(r"&id=([0-9a-f-]+)", url).captures[1]
        base = "http://127.0.0.1:$port"
        # No cookie a previous request set can stand in for the secret.
        get(path) = Pluto.HTTP.get(base * path; status_exception = false, redirect = false, retry = false, readtimeout = 30, cookies = false)
        frontend = Pluto.project_relative_path(Pluto.frontend_directory())
        static_file = first(f for f in readdir(frontend) if endswith(f, ".css") || endswith(f, ".svg"))

        @testset "Pluto answers nothing outside a path that is the session secret (ADR 0049, User isolation)" begin
            # Pluto serves its own scripts, styles and fonts, /ping and the binder token without the secret by default;
            # under the secret's base URL none of them is outside it.
            for path in ("/ping", "/possible_binder_token_please", "/favicon.ico", "/$static_file")
                @test get(path).status == 404
            end
            @test get("/").status in (403, 404)
            # The controls: under the secret's path everything is served, with the secret in the query as before (Pluto's
            # own check answers 403 to a request that carries the path but not the query, and `/ping` is public only
            # at the root).
            @test get("/$secret/ping").status == 403
            @test get("/$secret/ping?secret=$secret").status == 200
            @test String(get("/$secret/ping?secret=$secret").body) == "OK!"
            @test get("/$secret/$static_file").status == 200
            @test get("/$secret/edit?id=$id").status == 403
            wrong = replace(secret, secret[1] => secret[1] == '0' ? '1' : '0'; count = 1)
            @test get("/$wrong/ping?secret=$secret").status == 404   # the right query does not stand in for the right path
            @test get("/$secret/edit?secret=$secret&id=$id").status == 200
        end

        if Sys.iswindows()
            @testset "a drive path reads no file (Pluto's static route joins a path onto its folder, and on Windows an absolute one escapes it)" begin
                secret_file = joinpath(dir, "secret-data.json")
                write(secret_file, """{"private": "contents of a file outside Pluto"}""")
                drive_path = "/" * replace(secret_file, '\\' => '/')   # /C:/Users/.../secret-data.json
                # Without the secret's path, nothing is served. (Under the secret's path Pluto's static route still joins a drive
                # path onto its folder, which Windows resolves outside it; whoever holds the secret already holds Pluto's code
                # execution, so that path is not a lock.)
                response = get(drive_path)
                @test response.status == 404
                @test !occursin("contents of a file outside Pluto", String(response.body))
            end
        end

        if Sys.islinux()
            @testset "start.jl listens on its page and one worker, on loopback, and no command line carries the secret" begin
                tree = process_tree(Base.getpid(child))
                listening = tree_listeners(Base.getpid(child))
                @test (port, true) in listening
                @test length(listening) == 2
                @test all(last, listening)
                carriers, read_count = cmdline_carriers(tree, secret)
                @test isempty(carriers)
                @test read_count >= 1
            end
        end
    finally
        try close(child.in) catch end
        timedwait(() -> !process_running(child), 60.0) == :ok || kill(child)
        wait(child)
        process_exited(child) || kill(child)
        isfile(stderr_log) && !success(child) && println(stderr, "start.jl stderr tail:\n", last(readlines(stderr_log), 20))
    end
end
