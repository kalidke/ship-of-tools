### A Pluto.jl notebook ###
# v0.20.27

using Markdown
using InteractiveUtils

# ╔═╡ d691c6a1-6e50-4ec2-828a-5342d075dc96
md"""
# Pluto demo notebook

Fixture used by Ship of Tools's `o` keybind to verify the `pluto.open` flow
end-to-end: NavTree `o` on this file should spawn the Pluto sidecar
on the backend and open this notebook in the OS browser.

- Markdown renders inline
- Reactive cells re-compute when upstream values change
- Saving writes back to disk
"""

# ╔═╡ 41b780d3-c180-45e2-82d5-dcc75084fa5c
x = 2 + 2

# ╔═╡ d4bcd1f2-d69a-48d2-b8d4-eeb8990377e1
y = x * 10

# ╔═╡ a649d4d6-290c-4af7-a889-08010879f457
md"`x = $x`, `y = $y` — change `x` above and watch `y` follow."

# ╔═╡ Cell order:
# ╟─d691c6a1-6e50-4ec2-828a-5342d075dc96
# ╠═41b780d3-c180-45e2-82d5-dcc75084fa5c
# ╠═d4bcd1f2-d69a-48d2-b8d4-eeb8990377e1
# ╟─a649d4d6-290c-4af7-a889-08010879f457
