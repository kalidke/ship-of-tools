# dev/wglmakie-proof/run.jl — transport proof for ADR 0032 (interactive browser figures).
#
# JUST HIT `r` ON THIS FILE IN THE NAV. It is self-contained:
#   * it lives in its own env (dev/wglmakie-proof/Project.toml — WGLMakie +
#     Bonito), so the `r` keybind's project-discovery activates the RIGHT env
#     automatically; the Pkg.activate below is a belt-and-suspenders repeat.
#   * it does NOT block (no keep-alive loop) — the Bonito server runs on its own
#     task and keeps serving as long as the REPL process lives, so the eval
#     returns cleanly and the REPL stays usable. Re-hit `r` to restart it.
#
# WHAT IT PROVES, before wglshow is wired into the REPL:
#   1. an interactive WGLMakie/Bonito figure is usable in the FRONTEND browser
#      through a `-L 1241:127.0.0.1:1241` tunnel — i.e. the WebSocket (not just
#      the initial HTML GET) survives the SSH forward. (1241 is NOT forwarded by
#      the launcher yet — that's the launcher change ADR 0032 now calls for.)
#   2. the exact current Bonito API (configure_server! kwargs, Server/App,
#      online_url) — pinned against the real installed versions it prints.
#
# AFTER RUNNING, paste back: (a) the version line, (b) whether pan/zoom/rotate
# work in the browser at the printed URL, (c) any "STEP n FAILED" line verbatim.

import Pkg
Pkg.activate(@__DIR__)          # redundant under `r` (project already discovered), safe otherwise
try
    Pkg.instantiate()           # no-op once the env is set up; installs on first run
catch e
    @warn "Pkg.instantiate warning (continuing)" exception = e
end

using Sockets

const HOST = "127.0.0.1"
# 1241 — the first free loopback port ABOVE the daemon's range. 1234=pluto,
# 1235=video, 1236=docs, 1237-1240=docs POOL (ADR 0029 site_serve::pool_ports).
# 1241 is NOT forwarded by the launcher yet, so a remote FE needs a
# `-L 1241:127.0.0.1:1241` tunnel to reach it (see ADR 0032, corrected).
const PORT = parse(Int, get(ENV, "SOT_WGL_PORT", "1241"))
const EXTERNAL = "http://$(HOST):$(PORT)"

@info "wglmakie-proof: loading WGLMakie + Bonito (heavy the first time)…"
using WGLMakie
import Bonito

# VERSIONS FIRST — these pin the correct API for wglshow. Paste them back.
@info "versions" WGLMakie = pkgversion(WGLMakie) Bonito = pkgversion(Bonito) julia = VERSION

WGLMakie.activate!()

# Close a server left running by a previous run in THIS same REPL (belt-and-
# suspenders — `r` normally restarts the REPL child, freeing the port anyway).
if isdefined(Main, :WGLMAKIE_PROOF_SERVER) && Main.WGLMAKIE_PROOF_SERVER !== nothing
    try
        close(Main.WGLMAKIE_PROOF_SERVER)
        @info "closed previous proof server"
    catch
    end
end

# STEP 1 — bind Bonito to the fixed loopback port with a matching external URL,
# so every asset/WebSocket URL it emits is http://127.0.0.1:1237/… (matches the
# tunnel). configure_server! kwargs have drifted across Bonito versions; if this
# errors, its message is the fix for both here and wglshow.
try
    # Bonito 5.1.0: proxy_url is the external base URL clients resolve; here it
    # equals the loopback listen (127.0.0.1:1237), which the tunnel forwards 1:1.
    Bonito.configure_server!(; listen_port = PORT, listen_url = HOST, proxy_url = EXTERNAL)
    @info "STEP 1 ok — Bonito.configure_server!" PORT EXTERNAL
catch e
    @error "STEP 1 FAILED: Bonito.configure_server!(; listen_port, listen_url, proxy_url)" exception = (e, catch_backtrace())
    rethrow()
end

# A deliberately interactive 3-D scene: rotation/zoom exercise the WebSocket.
fig = WGLMakie.surface(
    -10:0.4:10, -10:0.4:10,
    (x, y) -> 8 * sin(sqrt(x^2 + y^2)) / (sqrt(x^2 + y^2) + 1);
    axis = (; type = WGLMakie.Axis3, title = "ADR 0032 transport proof — drag to rotate"),
)

# STEP 2 — serve the figure as a Bonito App on the pinned port.
app = try
    a = Bonito.App(fig)
    @info "STEP 2 ok — Bonito.App(fig)"
    a
catch e
    @error "STEP 2 FAILED: Bonito.App(fig)" exception = (e, catch_backtrace())
    rethrow()
end

server = try
    s = Bonito.Server(app, HOST, PORT; proxy_url = EXTERNAL)
    @info "STEP 3 ok — Bonito.Server(app, host, port; proxy_url)"
    s
catch e
    @error "STEP 3 FAILED: Bonito.Server(app, \"$HOST\", $PORT; proxy_url=…)" exception = (e, catch_backtrace())
    rethrow()
end
global WGLMAKIE_PROOF_SERVER = server

# Probe until the listener is actually bound (mirrors julia/pluto/start.jl).
let deadline = time() + 60.0
    while time() < deadline
        try
            close(Sockets.connect(HOST, PORT)); break
        catch; sleep(0.1); end
    end
end

url = try
    u = Bonito.online_url(server, "/")
    @info "STEP 4 ok — Bonito.online_url"
    u
catch e
    @error "STEP 4 FAILED: Bonito.online_url(server, \"/\") — fall back to http://127.0.0.1:$PORT" exception = (e, catch_backtrace())
    EXTERNAL
end

println()
println("READY — open this in the FRONTEND browser (tunneled) and interact:")
println("    ", url)
println()
println("If pan/zoom/rotate work over the tunnel, ADR 0032's transport is proven.")
println("The server keeps running in this REPL; re-hit r to restart it.")

# Return a BrowserView so the REPL emits a `browser` frame and the FE (on this
# branch, which has the frame handling) AUTO-OPENS the URL — no manual browser +
# typing. `ShipToolsRepl` is `using`'d by the REPL child, so browserview is in
# scope; fall back to the plain URL string if run headlessly outside the SoT
# REPL (e.g. `julia run.jl`), where no frame channel exists.
isdefined(Main, :ShipToolsRepl) ? Main.ShipToolsRepl.browserview(url) : url
