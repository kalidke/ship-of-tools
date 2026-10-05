# ADR 0049, User isolation: a `wglshow` page carries its assets inside it, so its port answers nothing without a secret
# (its page path, its websocket's session id), and serving it opens one listener. It needs Bonito, so it has an
# environment of its own: `julia --project=julia/repl/test/bonito julia/repl/test/bonito/runtests.jl`.

using Test
using Bonito
using SHA
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
