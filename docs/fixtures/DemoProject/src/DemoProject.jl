"""
    DemoProject

Great-circle navigation helpers: named
waypoints, pairwise distance and bearing,
route length, and a route plot.
"""
module DemoProject

using CairoMakie

export Waypoint, haversine, bearing,
       route_length, plot_route

"Mean Earth radius in kilometers (IUGG)."
const EARTH_RADIUS_KM = 6371.0

"""
    Waypoint(name, lat, lon)

A named position on the sphere, latitude
and longitude in degrees.
"""
struct Waypoint
    name::String
    lat::Float64
    lon::Float64
end

"""
    haversine(a::Waypoint, b::Waypoint)

Great-circle distance from `a` to `b` in
kilometers, by the haversine formula on a
sphere of radius [`EARTH_RADIUS_KM`](@ref).
"""
function haversine(a::Waypoint, b::Waypoint)
    φ1, φ2 = deg2rad(a.lat), deg2rad(b.lat)
    Δφ = φ2 - φ1
    Δλ = deg2rad(b.lon - a.lon)
    s = sin(Δφ / 2)^2 +
        cos(φ1) * cos(φ2) * sin(Δλ / 2)^2
    return 2 * EARTH_RADIUS_KM * asin(sqrt(s))
end

"""
    bearing(a::Waypoint, b::Waypoint)

Initial great-circle bearing from `a` toward
`b`, in degrees clockwise from true north,
normalized to `[0, 360)`.
"""
function bearing(a::Waypoint, b::Waypoint)
    φ1, φ2 = deg2rad(a.lat), deg2rad(b.lat)
    Δλ = deg2rad(b.lon - a.lon)
    θ = atan(sin(Δλ) * cos(φ2),
             cos(φ1) * sin(φ2) -
             sin(φ1) * cos(φ2) * cos(Δλ))
    return mod(rad2deg(θ), 360.0)
end

"""
    route_length(wps::Vector{Waypoint})

Total length of the polyline through `wps`,
in kilometers.
"""
function route_length(wps::Vector{Waypoint})
    length(wps) < 2 && return 0.0
    legs = zip(wps, wps[2:end])
    return sum(haversine(a, b)
               for (a, b) in legs)
end

"""
    plot_route(wps::Vector{Waypoint})

Plot the route on a lon/lat axis and return
the `Figure`: waypoints as labeled markers,
legs as lines, each leg annotated with its
[`haversine`](@ref) distance.
"""
function plot_route(wps::Vector{Waypoint})
    km = round(route_length(wps); digits = 1)
    fig = Figure(size = (640, 420),
                 fontsize = 20)
    # margins keep the names inside the frame
    ax = Axis(fig[1, 1];
        title = "route — $km km total",
        xlabel = "longitude (°)",
        ylabel = "latitude (°)",
        xautolimitmargin = (0.22, 0.22),
        yautolimitmargin = (0.16, 0.16))
    lons = [w.lon for w in wps]
    lats = [w.lat for w in wps]
    lines!(ax, lons, lats; linewidth = 2,
           color = :steelblue)
    scatter!(ax, lons, lats; markersize = 14,
             color = :orangered)
    # a name sits beside an end's one leg, on
    # a turn's open side, else left of it
    function side(i)
        nbs = [wps[j] for j in (i - 1, i + 1)
               if checkbounds(Bool, wps, j)]
        w = wps[i]
        dx = [n.lon - w.lon for n in nbs]
        if length(nbs) == 1
            h = dx[1] > 0 ? :left : :right
            up = nbs[1].lat < w.lat
            return (h, up ? :bottom : :top)
        end
        h = all(<(0), dx) ? :left : :right
        return (h, :center)
    end
    for i in eachindex(wps)
        h, v = side(i)
        ox = h == :left ? 11 : -11
        oy = v == :center ? 0 :
             v == :top ? -5 : 5
        text!(ax, wps[i].lon, wps[i].lat;
              text = wps[i].name,
              fontsize = 18, align = (h, v),
              offset = (ox, oy))
    end
    # leg lengths sit right of their line
    for (a, b) in zip(wps, wps[2:end])
        d = round(haversine(a, b); digits=1)
        up = (b.lat - a.lat) *
             (b.lon - a.lon) > 0
        v = up ? :top : :bottom
        text!(ax, (a.lon + b.lon) / 2,
              (a.lat + b.lat) / 2;
              text = "$d km", fontsize = 16,
              align = (:left, v),
              offset = (8, up ? -8 : 8),
              color = :gray40)
    end
    return fig
end

end # module
