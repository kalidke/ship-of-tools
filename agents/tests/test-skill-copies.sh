#!/usr/bin/env bash
# test-skill-copies.sh — every skill that has an install payload keeps its two copies byte-identical: the canonical
# copy under .claude/skills/ (edited in place) and the payload under agents/claude/ (what /sot-install copies into
# ~/.claude). Every file of a skill is compared, so a file present on one side only also fails. The suite also proves
# the check can fail: a scratch tree with one payload line changed, one with a payload file removed, and one with no
# skill pair at all.
#
# HERMETIC: reads this checkout and writes only under a temp folder of its own.
#
# Usage: agents/tests/test-skill-copies.sh
# Exit: 0 if every row passes, 1 otherwise.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-home-guard.sh" || exit 2   # never the live comm home

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-skill-copies-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
trap 'rm -rf "${WORK:?}"' EXIT
PASS=0
FAIL=0

check() {
    local name="$1"; shift
    if "$@"; then echo "PASS: $name"; PASS=$((PASS + 1)); else echo "FAIL: $name"; FAIL=$((FAIL + 1)); fi
}

# Prints the files that differ between each skill's canonical copy and its payload. Exit 0 only if at least one pair
# exists and none differs.
skill_drift() {
    local root="$1" d n pairs=0 drift=0
    for d in "$root"/.claude/skills/*/; do
        n="$(basename "$d")"
        [ -d "$root/agents/claude/$n" ] || continue
        pairs=$((pairs + 1))
        diff -rq "$d" "$root/agents/claude/$n" || drift=1
    done
    [ "$pairs" -gt 0 ] && return "$drift"
    return 1
}

drifts() { ! skill_drift "$1" >/dev/null; }

# A scratch copy of both trees under $WORK/$1, for the rows that break a copy on purpose.
scratch_tree() {
    local t="$WORK/$1"
    mkdir -p "$t/.claude" "$t/agents"
    cp -R "$ROOT/.claude/skills" "$t/.claude/" && cp -R "$ROOT/agents/claude" "$t/agents/" && echo "$t"
}

check "every skill copy is byte-identical to its payload" skill_drift "$ROOT"

t="$(scratch_tree changed)"
printf '# drifted\n' >> "$t/agents/claude/sot-statusline-setup/statusline.sh"
check "a payload with one changed line fails the check" drifts "$t"

t="$(scratch_tree missing)"
rm "$t/agents/claude/sot-setup/SKILL.md"
check "a payload missing a file fails the check" drifts "$t"

t="$WORK/no-pairs"
mkdir -p "$t/.claude/skills/only-canonical"
check "a tree with no skill pair fails the check" drifts "$t"

echo "PASS: $PASS  FAIL: $FAIL"
exit $((FAIL > 0))
