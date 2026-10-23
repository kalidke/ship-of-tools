#!/usr/bin/env bash
# test-installed-names.sh — where the agents tools land. Every code file directly in a folder `comm/bin-folders.txt`
# lists (`agents`, `agents/sot-fe`, `agents/spawn`, `agents/worktree`) installs flat into the comm folder's `bin/`, byte for
# byte; nothing of `agents/claude/` or `agents/codex/` does (the launchers `ccb` and `ccx` and the wrappers reach a person's
# `PATH` through the installer's own stage, `src/sources.jl`), and the wrapper `agents/claude/bin/sot-fe` never takes
# the place of the tool `agents/sot-fe/sot-fe`. The staged folder is the one `comm/tests/stage-bin.sh` makes from this
# checkout, the layout the installer's comm stage copies.
#
# HERMETIC: reads this checkout; writes only under a temp folder of its own.
#
# Usage: agents/tests/test-installed-names.sh     Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-home-guard.sh" || exit 2   # never the live comm home

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/installed-names-test-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
trap 'rm -rf "${WORK:?}"' EXIT
guard_fresh_home "$WORK"
STAGED="$(guard_stage_bin "$WORK")" || exit 2

PASS=0
FAIL=0
check() {
    local desc="$1" fn="$2"
    if "$fn"; then echo "PASS: $desc"; PASS=$((PASS + 1)); else echo "FAIL: $desc"; FAIL=$((FAIL + 1)); fi
}

case_every_code_file_of_the_listed_folders_is_staged_flat() {
    local dir f name seen=0 bad=0
    for dir in agents agents/sot-fe agents/spawn agents/worktree; do
        grep -qx "$dir" "$ROOT/comm/bin-folders.txt" || { echo "  $dir is not in comm/bin-folders.txt"; return 1; }
        for f in "$ROOT/$dir"/*; do
            [ -f "$f" ] || continue
            name="$(basename "$f")"
            case "$name" in *.md) continue ;; esac
            seen=$((seen + 1))
            cmp -s "$f" "$STAGED/$name" || { echo "  $dir/$name is not staged as bin/$name"; bad=1; }
        done
    done
    [ "$seen" -ge 15 ] || { echo "  only $seen files found"; return 1; }
    return "$bad"
}

case_the_launchers_and_wrappers_are_not_staged() {
    local f name bad=0
    for f in "$ROOT"/agents/claude/bin/* "$ROOT"/agents/codex/bin/*; do
        name="$(basename "$f")"
        [ "$name" = sot-fe ] && continue
        [ ! -e "$STAGED/$name" ] || { echo "  $name was staged"; bad=1; }
    done
    cmp -s "$ROOT/agents/sot-fe/sot-fe" "$STAGED/sot-fe" || { echo "  bin/sot-fe is not the tool"; bad=1; }
    return "$bad"
}

check "every code file of the four listed folders is staged flat, byte for byte" case_every_code_file_of_the_listed_folders_is_staged_flat
check "no launcher or PATH wrapper is staged, and bin/sot-fe is the tool, not the wrapper" case_the_launchers_and_wrappers_are_not_staged

echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
