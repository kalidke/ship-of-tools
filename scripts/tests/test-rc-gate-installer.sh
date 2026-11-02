#!/usr/bin/env bash
# The real gate pipeline runs both installer scenarios with the built binary paths and records failures by name.
# All suites and toolchains here are recording stand-ins; no build or installer runs.
set -eu
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/rc-gate-installer-XXXXXX") || exit 2
work=$(cd "$work" && pwd -P) || exit 2
trap 'rm -rf "${work:?}"' EXIT
repo=$work/repo
logs=$work/logs
mkdir -p "$repo/scripts/tests" "$repo/comm/tests" "$repo/agents/tests" "$repo/rust" \
    "$logs/rust" "$logs/steps" "$work/tools" "$work/run" "$work/target/debug"
for tool in cargo julia; do
    printf '#!/bin/sh\nexit 0\n' > "$work/tools/$tool"
    chmod +x "$work/tools/$tool"
done
cat > "$repo/scripts/tests/installer-publish-guard.py" <<'PY'
import argparse
from pathlib import Path

parser = argparse.ArgumentParser()
for flag in ["repo", "scenario", "sotd", "sot", "capsule"]:
    parser.add_argument("--" + flag, required=True)
args = parser.parse_args()
target = Path(args.repo).parent / "target/debug"
assert Path(args.sotd) == target / "sotd"
assert Path(args.sot) == target / "sot"
assert Path(args.capsule) == target / "sot-capsule"
assert args.scenario in ["fresh", "repair"]
print("recorded " + args.scenario)
raise SystemExit(7 if args.scenario == "fresh" else 0)
PY
# A failed workspace build suppresses Rust executable discovery, keeping this fixture independent of Cargo.
RCG_D=$repo RCG_L=$logs RCG_BUILD_RC=1 RCG_CARGO_DIR=$work/tools RCG_JULIA_DIR=$work/tools \
    RCG_BASH32= HOME=$work CARGO_TARGET_DIR=$work/target XDG_RUNTIME_DIR=$work/run \
    bash "$here/rc-gate.sh" --pipeline 2 > "$work/pipeline.log" 2>&1 || {
        echo 'FAIL: installer job pipeline did not complete'
        cat "$work/pipeline.log"
        exit 1
    }
for scenario in fresh repair; do
    expected=0
    [ "$scenario" != fresh ] || expected=7
    if ! { [ "$(cat "$logs/steps/installer-publish-$scenario.rc")" = "$expected" ] &&
        grep -qx "installer-publish-$scenario rc=$expected " "$logs/summary.txt" &&
        grep -qx "recorded $scenario" "$logs/installer-publish-$scenario.log"; }; then
        echo "FAIL: $scenario did not keep its result, summary line and log (expected rc=$expected)"
        cat "$work/pipeline.log" "$logs/summary.txt"
        exit 1
    fi
    echo "PASS: pipeline runs $scenario with all binary paths and records rc=$expected by name"
done
