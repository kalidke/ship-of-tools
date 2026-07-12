# dev/wglmakie-proof/run.jl — WGLMakie interactive-figure demo via `wglshow` (ADR 0032).
#
# JUST HIT `r`. Self-contained: its own env (Project.toml: WGLMakie + Bonito), so
# the `r` keybind's project-discovery activates the right project. `wglshow`
# (from ShipToolsRepl, on the REPL's load path) serves the figure over Bonito on
# SOT_WGL_PORT (default 1241, launcher-forwarded) and returns a BrowserView — so
# the REPL emits a `browser` frame and the FE AUTO-OPENS it in the browser. No
# manual URL typing.
#
# Transport + Bonito 5.1 API + browser-frame auto-open are all validated live
# (ADR 0032); this file is now the canonical `wglshow` usage example. The server
# lives in the REPL and a repeat `r` replaces it.

import Pkg
Pkg.activate(@__DIR__)
try
    Pkg.instantiate()          # no-op once the env is set up
catch e
    @warn "Pkg.instantiate warning (continuing)" exception = e
end

using WGLMakie

fig = WGLMakie.surface(
    -10:0.4:10, -10:0.4:10,
    (x, y) -> 8 * sin(sqrt(x^2 + y^2)) / (sqrt(x^2 + y^2) + 1);
    axis = (; type = WGLMakie.Axis3, title = "ADR 0032 — wglshow demo (drag to rotate)"),
)

# The whole feature in one call: serve interactively + auto-open in the FE
# browser. `wglshow` comes from ShipToolsRepl, which the REPL child `using`s —
# available on the load path when this runs via `r` (not via `julia run.jl`).
if isdefined(Main, :ShipToolsRepl)
    Main.ShipToolsRepl.wglshow(fig)
else
    error("run this via `r` inside the Ship of Tools REPL — `wglshow` is provided by ShipToolsRepl")
end
