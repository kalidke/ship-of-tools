---
target: DemoProject
target_kind: module
synced_against: "3d61b34aa29895feb7e07472ca3561c0f6a7fec2d1e9e186e2fd9ed900487aff"
synced_at: 2026-07-02T22:00Z
authored_by: orchestrator
references:
  - files/src/DemoProject.jl
---

# DemoProject

Pure-math great-circle core: named [`Waypoint`](DemoProject.Waypoint)s,
pairwise `haversine` distance and `bearing`, polyline `route_length`, and a `plot_route`
figure. The haversine form is chosen over the spherical law of cosines
because it is numerically stable for small separations — the common case
when summing route legs — where the cosine form catastrophically cancels.

Spherical-Earth accuracy (≈0.5 %) is accepted; no ellipsoid corrections,
no I/O. Kilometers everywhere — convert at the edges, not here.
