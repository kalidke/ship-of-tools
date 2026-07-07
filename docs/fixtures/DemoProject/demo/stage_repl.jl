# REPL staging for the docs screenshots (repl-figure + the hero's REPL pane).
# Dispatched by the capture harness via:  --demo-repl-eval include("demo/stage_repl.jl")
# (the scratch daemon starts with cwd = the fixture root, so the relative
# include resolves; the daemon-spawned REPL child inherits that cwd).
#
# The REPL boots in the SHIM env (julia --project=julia/repl), not the
# fixture env — so activate the fixture project first or `using DemoProject`
# has nowhere to resolve from. Quiet io: activation chatter isn't part of
# the shot. instantiate() is a no-op when the box pre-instantiated (the
# docs-shots prep does), and a one-time cost otherwise.
import Pkg
Pkg.activate(dirname(@__DIR__); io = devnull)
Pkg.instantiate(io = devnull)

using DemoProject

route = [
    Waypoint("Albuquerque", 35.08, -106.65),
    Waypoint("Santa Fe",    35.69, -105.94),
    Waypoint("Black Mesa",  35.87, -106.08),
    Waypoint("Taos",        36.41, -105.57),
]

# Last expression's value is the CairoMakie Figure — the REPL shim emits it
# as an inline image frame.
plot_route(route)
