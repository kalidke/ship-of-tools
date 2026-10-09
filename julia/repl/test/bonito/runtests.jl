# ADR 0049, User isolation: a `wglshow` page carries its assets inside it, so its port answers nothing without a secret
# (its page path, its websocket's session id), and serving it opens one listener. It needs Bonito, so it has an
# environment of its own: `julia --project=julia/repl/test/bonito julia/repl/test/bonito/runtests.jl`.

using Test
using Bonito
using SHA
using Sockets
using ShipToolsRepl

# Linux: the (port, loopback) of every TCP socket in state LISTEN whose inode this process holds open.
function listeners()
    inodes = Set{String}()
    for fd in readdir("/proc/self/fd")
        target = try readlink("/proc/self/fd/$fd") catch; "" end
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

@testset "wglshow's page carries its assets; its port serves nothing without a secret (ADR 0049, User isolation)" begin
    dir = mktempdir()
    file = joinpath(dir, "figure-data.png")
    write(file, rand(UInt8, 64))
    before = Sys.islinux() ? listeners() : nothing

    server = ShipToolsRepl.page_server(Bonito, "127.0.0.1", 0)
    try
        path = "/" * bytes2hex(rand(UInt8, 16))
        app = Bonito.App(() -> Bonito.DOM.div(Bonito.DOM.img(src = Bonito.Asset(file))))
        Bonito.route!(server, path => ShipToolsRepl.no_referrer_page(Bonito, app))
        get(p) = Bonito.HTTP.get("http://127.0.0.1:$(server.port)$p"; status_exception = false, retry = false, readtimeout = 30)

        page = get(path)
        body = String(copy(page.body))
        @test page.status == 200
        @test Bonito.HTTP.header(page, "Referrer-Policy") == "no-referrer"
        @test occursin("data:", body)                 # the file travels inside the page
        @test !occursin("/assets/", body)             # and no asset route is named

        # Bonito's asset route would answer with the file to any account that can compute this key from the file's path.
        asset_key = bytes2hex(sha1(abspath(file))) * "-" * basename(file)
        @test get("/assets/$asset_key").status == 404
        @test get("/").status == 404
        @test first.(server.routes.table) == [path]   # the page alone: no asset route

        if Sys.islinux()
            @test setdiff(listeners(), before) == Set([(server.port, true)])
        end
    finally
        close(server)
    end
end

# The listener set of this process must equal the declared one exactly: a listener beyond it fails the comparison even
# when every listener is on loopback and the declared one still serves.
exact_listeners(before, declared) = setdiff(listeners(), before) == declared

# A port the OS had free a moment ago, released.
function free_port()
    probe = listen(ip"127.0.0.1", 0)
    port = Int(getsockname(probe)[2])
    close(probe)
    return port
end

function reset_wgl!()
    page = ShipToolsRepl.WGL_SERVER[]
    page === nothing || close(page.server)
    ShipToolsRepl.WGL_SERVER[] = nothing
end

get_page(port, path) = Bonito.HTTP.get("http://127.0.0.1:$port$path"; status_exception = false, retry = false, read_idle_timeout = 30)

@testset "WGL listener selection exposes only its owned loopback listener" begin
    if !Sys.islinux()
        @test_skip "listener observation reads /proc"
    else
        reset_wgl!()
        before = listeners()

        # First default selection: one OS-assigned loopback listener, serving its page.
        page = ShipToolsRepl.wgl_server(Bonito, nothing)
        port = page.server.port
        @test exact_listeners(before, Set([(port, true)]))
        app = Bonito.App(() -> Bonito.DOM.div("page"))
        Bonito.route!(page.server, page.path => ShipToolsRepl.no_referrer_page(Bonito, app))
        for _ in 1:2   # repeated page serves
            @test get_page(port, page.path).status == 200
            @test exact_listeners(before, Set([(port, true)]))
        end

        # Default and same-pin calls reuse it.
        @test ShipToolsRepl.wgl_server(Bonito, nothing) === page
        @test ShipToolsRepl.wgl_server(Bonito, port) === page
        @test exact_listeners(before, Set([(port, true)]))

        # The comparison rejects a listener beyond the declared set, with the declared listener still serving.
        extra = listen(ip"127.0.0.1", 0)
        try
            @test get_page(port, page.path).status == 200
            @test !exact_listeners(before, Set([(port, true)]))
        finally
            close(extra)
        end
        @test exact_listeners(before, Set([(port, true)]))

        # A taken first pin throws and publishes no server.
        reset_wgl!()
        taken = listen(ip"127.0.0.1", 0)
        try
            taken_port = Int(getsockname(taken)[2])
            @test_throws ErrorException ShipToolsRepl.wgl_server(Bonito, taken_port)
            @test ShipToolsRepl.WGL_SERVER[] === nothing
            @test exact_listeners(before, Set([(taken_port, true)]))   # the reservation alone
        finally
            close(taken)
        end
        @test exact_listeners(before, Set{Tuple{Int,Bool}}())
        reset_wgl!()
    end
end

@testset "a live WGL port cannot be replaced" begin
    if !Sys.islinux()
        @test_skip "listener observation reads /proc"
    else
        reset_wgl!()
        before = listeners()
        app = Bonito.App(() -> Bonito.DOM.div("page"))
        page = ShipToolsRepl.wgl_server(Bonito, nothing)
        port = page.server.port
        Bonito.route!(page.server, page.path => ShipToolsRepl.no_referrer_page(Bonito, app))
        secret = page.path
        @test get_page(port, secret).status == 200

        # A distinct port, reserved by an owned listener and released, so a collision is not the failure.
        other = free_port()
        @test other != port
        err = try ShipToolsRepl.wgl_server(Bonito, other); nothing catch e; e end
        @test err isa ArgumentError

        # Nothing changed: the same server, port and secret (compared as a boolean, never printed) ...
        @test ShipToolsRepl.WGL_SERVER[] === page
        same_secret = ShipToolsRepl.WGL_SERVER[].path == secret
        @test same_secret
        # ... the original listener still serves its page ...
        @test get_page(port, secret).status == 200
        # ... and no listener was created at the requested port: an owned replacement can bind it.
        owned = listen(ip"127.0.0.1", other)
        try
            @test exact_listeners(before, Set([(port, true), (other, true)]))
        finally
            close(owned)
        end
        @test exact_listeners(before, Set([(port, true)]))

        # The same pin and the default still reuse it.
        @test ShipToolsRepl.wgl_server(Bonito, port) === page
        @test ShipToolsRepl.wgl_server(Bonito, nothing) === page
        # An invalid pin is rejected before anything is bound.
        @test_throws ArgumentError ShipToolsRepl.wgl_server(Bonito, 0)
        @test_throws ArgumentError ShipToolsRepl.wgl_server(Bonito, 70000)
        @test exact_listeners(before, Set([(port, true)]))
        reset_wgl!()
    end
end
