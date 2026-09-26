# Plot the Rio Grande route; save the figure.
import Pkg
root = dirname(@__DIR__)
Pkg.activate(root; io = devnull)

using DemoProject, CairoMakie

route = [
    Waypoint("Albuquerque", 35.08, -106.65),
    Waypoint("Santa Fe",    35.69, -105.94),
    Waypoint("Black Mesa",  35.87, -106.08),
    Waypoint("Taos",        36.41, -105.57),
]

len = round(route_length(route); digits = 1)
println("route: $len km")

fig = plot_route(route)
save(joinpath(root, "data", "track.png"), fig)
fig
