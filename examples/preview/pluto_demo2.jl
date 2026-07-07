### A Pluto.jl notebook ###
# v0.20.27

using Markdown
using InteractiveUtils

# ╔═╡ 7b5c857b-1f15-4959-ae03-2115c6a93280
md"""
# Pluto demo · second notebook

Second fixture used by Ship of Tools's `o` keybind to verify that a Pluto
sidecar already running for one notebook accepts an `OPEN` for a
*different* notebook without re-spawning — i.e. one server, many
notebooks.

After pressing `o` on the first demo (`pluto_demo.jl`) and `o` on this
one, both should be reachable in the browser at distinct
`http://127.0.0.1:1234/edit?id=…` URLs, sharing the same Pluto
process.
"""

# ╔═╡ 1564e6d1-b8fe-4768-a554-91e3deed0fef
xs = collect(1:10)

# ╔═╡ 626bf520-beae-4311-b55b-0d4b4481e862
total = sum(xs .^ 2)

# ╔═╡ 19cc790c-5bf1-4784-80ce-887df910f8e5
md"`xs = $xs`, `sum(xs.^2) = $total` — change `xs` above to watch the total follow."

# ╔═╡ Cell order:
# ╟─7b5c857b-1f15-4959-ae03-2115c6a93280
# ╠═1564e6d1-b8fe-4768-a554-91e3deed0fef
# ╠═626bf520-beae-4311-b55b-0d4b4481e862
# ╟─19cc790c-5bf1-4784-80ce-887df910f8e5
