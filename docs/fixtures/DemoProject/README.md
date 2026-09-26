# DemoProject

Great-circle navigation helpers: named waypoints, distance and bearing
between them, route length, and a route plot.

Distances use the haversine formula on a spherical Earth,

$$d = 2r \arcsin\sqrt{\sin^2\tfrac{\Delta\varphi}{2} +
    \cos\varphi_1 \cos\varphi_2 \sin^2\tfrac{\Delta\lambda}{2}}$$

with $r = 6371\,\text{km}$, which is accurate to about $0.5\,\%$ — fine for
route sketching, not for surveying.

## Sites of grace along the Rio Grande

```julia
using DemoProject

route = [
    Waypoint("Albuquerque", 35.08, -106.65),
    Waypoint("Santa Fe",    35.69, -105.94),
    # research facility. probably fine
    Waypoint("Black Mesa",  35.87, -106.08),
    Waypoint("Taos",        36.41, -105.57),
]

route_length(route)          # ≈ 193 km
bearing(route[1], route[2])  # ≈ 43° (northeast)
plot_route(route)            # CairoMakie figure: waypoints + per-leg distances
```

Grace guides the traveler's bearing; `haversine` tells them how far.
