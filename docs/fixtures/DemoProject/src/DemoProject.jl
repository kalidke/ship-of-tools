"""
    DemoProject

Great-circle navigation helpers. This is the Ship of Tools **documentation
fixture**: a deliberately small, stable package the docs screenshots are
staged against. Its shape is chosen to exercise the explorer — a struct, a
few documented functions, one with multiple methods — not to be useful at sea.

If you edit this file, re-run `scripts/docs-shots.sh sync-fixture` so the
fresh `.concept/` annotation is re-stamped against the new content hash.
"""
module DemoProject

using CairoMakie

export Waypoint, haversine, bearing, route_length, plot_route

"Mean Earth radius in kilometers (IUGG)."
const EARTH_RADIUS_KM = 6371.0

"""
    Waypoint(name, lat, lon)

A named position on the sphere, latitude and longitude in degrees.
"""
struct Waypoint
    name::String
    lat::Float64
    lon::Float64
end

"""
    haversine(a::Waypoint, b::Waypoint) -> Float64

Great-circle distance between `a` and `b` in kilometers, by the haversine
formula:

```math
d = 2r \\arcsin\\sqrt{\\sin^2\\tfrac{\\Delta\\varphi}{2} +
    \\cos\\varphi_1 \\cos\\varphi_2 \\sin^2\\tfrac{\\Delta\\lambda}{2}}
```
"""
function haversine(a::Waypoint, b::Waypoint)
    φ1, φ2 = deg2rad(a.lat), deg2rad(b.lat)
    Δφ = φ2 - φ1
    Δλ = deg2rad(b.lon - a.lon)
    s = sin(Δφ / 2)^2 + cos(φ1) * cos(φ2) * sin(Δλ / 2)^2
    return 2 * EARTH_RADIUS_KM * asin(sqrt(s))
end

"""
    bearing(a::Waypoint, b::Waypoint) -> Float64

Initial great-circle bearing from `a` toward `b`, in degrees clockwise from
true north, normalized to `[0, 360)`.
"""
function bearing(a::Waypoint, b::Waypoint)
    φ1, φ2 = deg2rad(a.lat), deg2rad(b.lat)
    Δλ = deg2rad(b.lon - a.lon)
    θ = atan(sin(Δλ) * cos(φ2),
             cos(φ1) * sin(φ2) - sin(φ1) * cos(φ2) * cos(Δλ))
    return mod(rad2deg(θ), 360.0)
end

"""
    route_length(waypoints::Vector{Waypoint}) -> Float64

Total length of the polyline through `waypoints`, in kilometers.
"""
function route_length(waypoints::Vector{Waypoint})
    length(waypoints) < 2 && return 0.0
    return sum(haversine(waypoints[i], waypoints[i+1])
               for i in 1:length(waypoints)-1)
end

"""
    route_length(coords::AbstractMatrix{<:Real}) -> Float64

Matrix form: each row is `(lat, lon)` in degrees. Unnamed waypoints.
"""
function route_length(coords::AbstractMatrix{<:Real})
    size(coords, 1) < 2 && return 0.0
    wps = [Waypoint("", coords[i, 1], coords[i, 2]) for i in 1:size(coords, 1)]
    return route_length(wps)
end

"""
    plot_route(waypoints::Vector{Waypoint}) -> Figure

Plot the route on a lon/lat axis: waypoints as labeled markers, legs as lines,
each leg annotated with its [`haversine`](@ref) distance. This is the function
the docs' REPL screenshot dispatches — an inline figure with real content.
"""
function plot_route(wps::Vector{Waypoint})
    fig = Figure(size = (640, 420))
    ax = Axis(fig[1, 1]; xlabel = "longitude (°)", ylabel = "latitude (°)",
              title = "route — $(round(route_length(wps); digits = 1)) km total",
              # generous margins so waypoint-name labels never clip at the frame
              xautolimitmargin = (0.08, 0.18), yautolimitmargin = (0.10, 0.12))
    lons, lats = [w.lon for w in wps], [w.lat for w in wps]
    lines!(ax, lons, lats; color = :steelblue, linewidth = 2)
    scatter!(ax, lons, lats; color = :orangered, markersize = 12)
    for w in wps
        text!(ax, w.lon, w.lat; text = " " * w.name, align = (:left, :bottom),
              fontsize = 12)
    end
    for i in 1:length(wps)-1
        mx, my = (wps[i].lon + wps[i+1].lon) / 2, (wps[i].lat + wps[i+1].lat) / 2
        text!(ax, mx, my; text = "$(round(haversine(wps[i], wps[i+1]); digits = 1)) km",
              align = (:center, :top), fontsize = 11, color = :gray40)
    end
    return fig
end

end # module
