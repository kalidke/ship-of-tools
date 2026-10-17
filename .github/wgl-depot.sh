#!/usr/bin/env bash
# wgl-depot.sh: builds the Julia depot the WGL page test reads, in the runner's temporary folder, and names it in
# JULIA_DEPOT_PATH for the rest of the job. rust.yml's `test` job and its `test-linux` shard sotd run it.
#
# The depot is built over Julia's bundled depots (the empty last entry: `;` on Windows, `:` elsewhere), and the job
# reads it on that same list. So it holds no stdlib builds of its own, its caches are compiled against the stdlib
# caches Julia ships, the only copy of each on the path, and the page test's offline add reuses them and compiles only
# the shim and WGLMakie, whose precompile workload the test turns off (a setting WGLMakie's cache is keyed on). A depot
# built without Julia's bundled depots compiles its own Pkg and stdlibs; read with them, it puts two copies of each
# stdlib on the path, and the add can recompile most of WGLMakie's environment: it did on hosted Linux and Windows, and
# on another depot built the same way it did not. What decides it is not known; the step refuses such a depot, and the
# page test's setup fails fast on a path holding two fresh Pkg caches, or none.
set -euo pipefail
if [ "$RUNNER_OS" = Windows ]; then sep=';'; else sep=':'; fi
export JULIA_DEPOT_PATH="$RUNNER_TEMP/julia-depot$sep"
echo "JULIA_DEPOT_PATH=$JULIA_DEPOT_PATH" >> "$GITHUB_ENV"
type -a julia
julia --startup-file=no -e '
  println("DEPOT_PATH: ", DEPOT_PATH)
  using Pkg
  Pkg.activate(; temp = true)
  Pkg.add(name = "WGLMakie", version = "0.13")
  own = joinpath(DEPOT_PATH[1], "compiled", "v$(VERSION.major).$(VERSION.minor)", "Pkg")
  isdir(own) && error("$own exists: the depot was built without the bundled depots")'
