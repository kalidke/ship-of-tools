# dev/wglmakie-proof.jl — transport proof for ADR 0032 (interactive browser figures).
#
# WHAT THIS PROVES, before we wire `wglshow` into the REPL:
#   1. An interactive WGLMakie/Bonito figure is fully usable in the FRONTEND's
#      browser through the launcher's existing `-L 1237:127.0.0.1:1237` tunnel —
#      pan / zoom / rotate must work, which means the WebSocket (not just the
#      initial HTML GET) survives the SSH forward.
#   2. The exact current Bonito API for binding a FIXED loopback port with a
#      matching external URL and obtaining a servable figure's URL. Bonito's API
#      has churned (JSServe → Bonito; `configure_server!` / `Server` / `App`),
#      so we pin it against a real installed version rather than guess.
#
# HOW TO RUN (on kitt, from a project whose env has WGLMakie):
#   cd <a project that declares WGLMakie>      # e.g. SMLMVis, papers-vortex-sr
#   julia --project=. /path/to/ship-of-tools/dev/wglmakie-proof.jl
# then, in the FRONTEND's browser (local or tunneled), open the URL it prints
# and confirm the figure is INTERACTIVE (drag to rotate, scroll to zoom).
#
# Port 1237 is `SOT_WGL_PORT` (default) and is already SSH-forwarded by the
# launcher alongside pluto(1234)/video(1235)/docs(1236).
#
# This is a dev-only proof, not shipped product. Once it validates, its Bonito
# calls become the body of `ShipToolsRepl.wglshow` and this file can retire.

using Sockets

const HOST = "127.0.0.1"
const PORT = parse(Int, get(ENV, "SOT_WGL_PORT", "1237"))
const EXTERNAL = "http://$(HOST):$(PORT)"

@info "wglmakie-proof: loading WGLMakie + Bonito (heavy first-time precompile)…"
using WGLMakie
import Bonito

WGLMakie.activate!()

# Bind Bonito to the fixed loopback port with an external URL that matches the
# tunnel, so every asset/WebSocket URL it emits is `http://127.0.0.1:1237/…`.
# NOTE: this is the API surface to CONFIRM/CORRECT against the installed Bonito —
# `configure_server!` keywords have varied across versions. If a keyword errors,
# the fix belongs here first, then in `wglshow`.
try
    Bonito.configure_server!(; listen_port = PORT, listen_url = HOST, external_url = EXTERNAL)
    @info "Bonito.configure_server! OK" PORT EXTERNAL
catch e
    @error "Bonito.configure_server! rejected these kwargs — adjust to the installed API" exception = e
    rethrow()
end

# A deliberately interactive 3-D scene: rotation/zoom exercise the WebSocket.
fig = WGLMakie.surface(
    -10:0.4:10, -10:0.4:10,
    (x, y) -> 8 * sin(sqrt(x^2 + y^2)) / (sqrt(x^2 + y^2) + 1);
    axis = (; type = WGLMakie.Axis3, title = "ADR 0032 transport proof — drag to rotate"),
)

# Serve the figure as a Bonito App on the pinned port. The `App`/`Server` pairing
# is the second API point to confirm; `online_url` builds the loopback URL.
app = Bonito.App(fig)
server = Bonito.Server(app, HOST, PORT)

# Probe until the listener is actually bound (mirrors julia/pluto/start.jl).
let deadline = time() + 60.0
    while time() < deadline
        try
            close(Sockets.connect(HOST, PORT)); break
        catch; sleep(0.1); end
    end
end

url = Bonito.online_url(server, "/")
println()
println("READY — open this in the FRONTEND browser (tunneled) and interact:")
println("    $url")
println()
println("If pan/zoom/rotate work over the tunnel, ADR 0032's transport is proven.")
println("Ctrl-C to stop the server.")

# In the real REPL this becomes: `return ShipToolsRepl.browserview(url)` so the
# frontend opens it automatically via the `browser` frame.
try
    while true; sleep(1); end
catch
    @info "wglmakie-proof: stopping"
end
