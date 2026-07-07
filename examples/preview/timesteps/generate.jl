#!/usr/bin/env julia
# Generate a series of same-size PNG renders for the preview pane's
# zoom/pan view cache test. The frontend caches (zoom, pan) per
# (parent_dir, dims) — same-dimension images in this dir should share
# a cached view, so navigating between t000.png … t005.png keeps the
# zoom locked while only the plot content changes.
#
# Run from the repo root:  julia --project=. examples/preview/timesteps/generate.jl

using CairoMakie

const W = 800
const H = 600
const OUT_DIR = @__DIR__
const N_FRAMES = 6

CairoMakie.activate!(type = "png")

xs = range(-3.0, 3.0; length = 200)
ys = range(-3.0, 3.0; length = 200)

for k in 0:(N_FRAMES - 1)
    t = k / (N_FRAMES - 1)              # 0 → 1 across the frames
    cx, cy = 1.5 * sin(2π * t), 1.5 * cos(2π * t)
    zs = [exp(-((x - cx)^2 + (y - cy)^2) / 0.6) +
          0.35 * cos(2 * (x - cx)) * cos(2 * (y - cy))
          for x in xs, y in ys]

    fig = Figure(size = (W, H))
    ax = Axis(
        fig[1, 1];
        title = "timestep $(lpad(k, 3, '0')) — t = $(round(t; digits = 3))",
        xlabel = "x",
        ylabel = "y",
        aspect = DataAspect(),
    )
    hm = heatmap!(ax, xs, ys, zs; colormap = :viridis, colorrange = (-0.5, 1.5))
    contour!(ax, xs, ys, zs; levels = 8, color = :white, linewidth = 0.5)
    Colorbar(fig[1, 2], hm; label = "amplitude")

    path = joinpath(OUT_DIR, "t$(lpad(k, 3, '0')).png")
    save(path, fig; px_per_unit = 1.0)
    @info "wrote" path
end
