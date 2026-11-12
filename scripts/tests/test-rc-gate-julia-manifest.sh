#!/usr/bin/env bash
# test-rc-gate-julia-manifest.sh: the gate's Julia step runs a package whose untracked Manifest.toml predates a
# dependency its Project.toml gained. The gate's scratch clone keeps the manifests of earlier runs, and
# `Pkg.instantiate()` alone refuses a manifest older than its project ("is a direct dependency, but does not appear in
# the manifest"), so the step brings the manifest up to date first. The package is a scratch one in a scratch depot, its
# dependency a second scratch package by path, so nothing is fetched. Exit: failed checks.
set -u
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
base="${TMPDIR:-/tmp}"
work=$(mktemp -d "${base%/}/rc-gate-julia-manifest-XXXXXX") || exit 2
trap 'rm -rf "${work:?}"' EXIT
work=$(cd "$work" && pwd -P)
julia_bin=$(command -v julia) || { echo "FAIL: no julia on PATH"; exit 1; }
julia_dir=$(dirname "$(readlink -f "$julia_bin")")
D=$work/checkout L=$work/logs depot=$work/depot
mkdir -p "$D/dep/src" "$D/core/src" "$D/core/test" "$L/steps" "$L/home" "$work/run" "$depot"
cat > "$D/core/Project.toml" << 'PROJECT'
name = "StaleProbe"
uuid = "4f1c2a52-6a0e-4b0e-9a55-2f0f8f0a1c11"
version = "0.1.0"

[extras]
Test = "8dfed614-e22c-5e08-85e1-65c5234f0b40"

[targets]
test = ["Test"]
PROJECT
echo 'module StaleProbe end' > "$D/core/src/StaleProbe.jl"
printf 'using Test, StaleDep\n@test StaleDep.five == 5\n' > "$D/core/test/runtests.jl"
printf 'name = "StaleDep"\nuuid = "9b2d0d6e-3a52-4c63-8f0b-5e7a1c0d2e22"\nversion = "0.1.0"\n' > "$D/dep/Project.toml"
echo 'module StaleDep const five = 5 end' > "$D/dep/src/StaleDep.jl"
# The manifest an earlier run left, then the dependency the project gains.
JULIA_DEPOT_PATH=$depot HOME=$L/home "$julia_dir/julia" --project="$D/core" -e 'using Pkg; Pkg.resolve()' > "$work/resolve.log" 2>&1 ||
  { echo "FAIL: the scratch package did not resolve"; cat "$work/resolve.log"; exit 1; }
printf '\n[deps]\nStaleDep = "9b2d0d6e-3a52-4c63-8f0b-5e7a1c0d2e22"\n\n[sources]\nStaleDep = { path = "../dep" }\n' >> "$D/core/Project.toml"

RCG_D=$D RCG_L=$L RCG_JULIA_DIR=$julia_dir RCG_CARGO_DIR=$work XDG_RUNTIME_DIR=$work/run CARGO_TARGET_DIR=$work/target \
  HOME=$work JULIA_DEPOT_PATH=$depot bash "$here/rc-gate.sh" --job julia > "$work/job.out" 2>&1 ||
  { echo "FAIL: the job itself failed"; cat "$work/job.out"; exit 1; }
rc=$(cat "$L/steps/julia-core.rc" 2> /dev/null)
fail=0
if [ "$rc" = 0 ]; then
  echo "PASS: a manifest that predates a dependency does not fail the Julia step"
else
  echo "FAIL: the Julia step failed (rc ${rc:-none}): $(grep -m1 -E 'ERROR|direct dependency' "$L/julia-core.log" 2> /dev/null)"
  fail=1
fi
echo "failed: $fail"
exit "$fail"
