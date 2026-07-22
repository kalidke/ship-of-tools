# Ship of Tools — `wglshow` interactive-figure demo (ADR 0032).
#
# `wglshow(fig)` serves a WGLMakie figure over Bonito on a loopback port and the
# frontend auto-opens it in your OS browser — a LIVE figure you can pan / zoom /
# rotate, not a static PNG in the preview pane. Reach is transparent: the daemon
# TCP proxy (ADR 0035) — or, as a fallback, the launcher's `-L 1241` tunnel —
# carries it to a remote frontend; a local frontend hits 127.0.0.1 directly.
# Same URL either way.
#
# HOW TO RUN (inside a Ship of Tools REPL):
#   - Press `r` (fresh REPL) or `R` (include) on this file in the nav, or paste
#     it into the REPL drawer.
#   - The LAST expression must be `wglshow(fig)` — that return value is what
#     emits the `browser` frame the frontend opens.
#
# WHY WGLMakie AND NOT CairoMakie: Ship of Tools' STATIC previews use CairoMakie
# (a PNG rendered into the preview pane). `wglshow` is for the INTERACTIVE case,
# where live exploration — rotating a 3D structure, zooming into a region — is
# the whole point, and that needs WGLMakie's browser backend. `ShipToolsRepl`
# itself carries no plotting dependency; `using WGLMakie` below resolves it (and
# Bonito, its dependency) from YOUR loaded project env at call time.
#
# FIRST RUN precompiles WGLMakie, which can take a minute; the REPL shows
# "julia starting — precompiling…" until it's ready (repl_state, ADR 0034 line).

using WGLMakie
using Random

# A synthetic 3D "localization cloud": two offset helical strands with a little
# Gaussian jitter — the kind of structure whose 3D arrangement a flat projection
# flattens away but a rotatable view makes obvious. That contrast is exactly the
# reason to reach for `wglshow` instead of a static render.
Random.seed!(42)
const N = 1200
t = range(0, 6π; length = N)
jitter() = 0.15 .* randn(N)
strand(phase) = (cos.(t .+ phase) .+ jitter(),
                 sin.(t .+ phase) .+ jitter(),
                 (t ./ (6π)) .* 4.0 .+ jitter())

x1, y1, z1 = strand(0.0)
x2, y2, z2 = strand(float(π))

fig = Figure(; size = (900, 700))
ax = Axis3(fig[1, 1];
           title = "wglshow demo — drag to rotate, scroll to zoom",
           xlabel = "x (µm)", ylabel = "y (µm)", zlabel = "z (µm)")
scatter!(ax, x1, y1, z1; markersize = 6, color = z1, colormap = :viridis)
scatter!(ax, x2, y2, z2; markersize = 6, color = z2, colormap = :plasma)

# LAST expression: serve the figure over Bonito and hand its URL to the FE,
# which opens it in the browser. Returns a `BrowserView`. A repeat `wglshow`
# in the same REPL reuses the port (replaces the previous server).
wglshow(fig)
