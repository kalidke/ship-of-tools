---
target: src/DemoProject.jl
target_kind: file
synced_against: "0000000000000000000000000000000000000000000000000000000000000000"
synced_at: 2026-05-01T09:00Z
authored_by: orchestrator
references:
  - modules/DemoProject
---

# src/DemoProject.jl

The whole package in one file: the `Waypoint` struct and three functions —
`haversine`, `bearing`, and `route_length` (vector form only).

*(This annotation is **deliberately stale** — its `synced_against` hash can
never match the file, so the drift badge renders; and the prose above has
genuinely drifted: the file has since grown a matrix method for
`route_length` and a `plot_route` figure function. The docs screenshots use
this to show what staleness looks like; do not "fix" it.)*
