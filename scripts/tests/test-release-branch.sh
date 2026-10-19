#!/usr/bin/env bash
# test-release-branch.sh -- release.sh's branch guard: a tag is cut from main, and from no other branch. The script runs
# in a throwaway repo with no origin, so a cut stops at its first check after the guard: a cut from main gets past the
# guard (its first refusal is the missing origin), and a cut from fixes/* or rc/* is refused by the guard before any
# check that reads origin or the versions.
#
# Run: scripts/tests/test-release-branch.sh

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK:?}"' EXIT
fails=0

check() {  # <description> <expected> <actual>
    if [ "$2" = "$3" ]; then
        printf '    ok   %s\n' "$1"
    else
        printf '    FAIL %s\n      expected: %s\n      actual:   %s\n' "$1" "$2" "$3"
        fails=$((fails + 1))
    fi
}

REPO="$WORK/repo"
mkdir -p "$REPO/scripts"
cp "$ROOT/scripts/release.sh" "$REPO/scripts/release.sh"
(
    cd "$REPO"
    git init -q
    git add scripts/release.sh
    git -c user.name=test -c user.email=test@example.invalid -c commit.gpgsign=false commit -q -m "fixture: release.sh"
)

# first_refusal <branch>: the first `preflight:` line release.sh prints for a cut from <branch>, empty when there is none.
first_refusal() {
    (cd "$REPO" && git checkout -q -B "$1" && bash scripts/release.sh 0.6.6 2>&1) | grep -m1 '^preflight:' || true
}

echo "  release.sh branch guard"
check "a cut from main passes the guard: its first refusal is the missing origin" \
    "preflight: cannot read origin's tags — the highest tag on the 0.6.6 line is unknown" "$(first_refusal main)"
check "a cut from fixes/0.6.6 is refused by the guard" \
    "preflight: on 'fixes/0.6.6' — every tag is cut from main" "$(first_refusal fixes/0.6.6)"
check "a cut from rc/0.6.6 is refused by the guard" \
    "preflight: on 'rc/0.6.6' — every tag is cut from main" "$(first_refusal rc/0.6.6)"

if [ "$fails" -gt 0 ]; then
    echo "FAILED: $fails check(s)"
    exit 1
fi
echo "all checks passed"
