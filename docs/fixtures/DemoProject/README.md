# DemoProject

Great-circle navigation helpers, and the **Ship of Tools documentation
fixture** — the small, stable workspace the docs screenshots are staged
against.

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
    Waypoint("Black Mesa",  35.87, -106.08),  # research facility. probably fine
    Waypoint("Taos",        36.41, -105.57),
]

route_length(route)          # ≈ 193 km
bearing(route[1], route[2])  # ≈ 43° (northeast)
plot_route(route)            # CairoMakie figure: waypoints + per-leg distances
```

Grace guides the traveler's bearing; `haversine` tells them how far.

## Why this package exists

Screenshots need content that never drifts: the module gives Modules mode a
struct, documented functions, and a two-method function; this README gives the
preview pane markdown with math; `.concept/` carries one current and one
deliberately stale annotation so the drift badge is visible on demand.
