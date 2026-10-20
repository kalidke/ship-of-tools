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

# The test step's preconditions and the restore after a failed stamp. A fixture repo with an origin, a stamp of 0.6.6-dev
# and stubs of cargo, gh and systemctl on PATH: every check before the stamp passes, so the cut reaches the setup checks.
STUBS="$WORK/stubs"
mkdir -p "$STUBS"
cat > "$STUBS/cargo" <<'STUB'
#!/bin/sh
[ "$1" = test ] && exit "${STUB_CARGO_TEST_RC:-0}"
exit 0
STUB
cat > "$STUBS/gh" <<'STUB'
#!/bin/sh
echo "$(git rev-parse HEAD) completed success 1"
STUB
cat > "$STUBS/systemctl" <<'STUB'
#!/bin/sh
exit "${STUB_SYSTEMCTL_RC:-0}"
STUB
chmod +x "$STUBS/cargo" "$STUBS/gh" "$STUBS/systemctl"

CUT="$WORK/cut"
git init -q --bare "$WORK/origin.git"
mkdir -p "$CUT/scripts" "$CUT/rust"
cp "$ROOT/scripts/release.sh" "$CUT/scripts/release.sh"
printf '[workspace.package]\nversion = "0.6.6-dev"\n' > "$CUT/rust/Cargo.toml"
printf 'version = 3\n' > "$CUT/rust/Cargo.lock"
printf 'version = "0.6.6-dev"\n' > "$CUT/Project.toml"
(
    cd "$CUT"
    git init -q -b main
    git remote add origin "$WORK/origin.git"
    git add .
    git -c user.name=test -c user.email=test@example.invalid -c commit.gpgsign=false commit -q -m "fixture"
    git push -q origin main
)
MJ="$CUT/rust/backend/sidecars/mathjax/node_modules"

# cut_out <extra env assignment>: the cut's output and exit status, run with the stubs first on PATH.
cut_out() {
    (cd "$CUT" && env PATH="$STUBS:$PATH" "$@" bash scripts/release.sh 0.6.6 2>&1; echo "exit $?")
}
tree_state() { (cd "$CUT" && git status --porcelain | tr '\n' ' '); }

echo "  release.sh setup checks and restore"
out=$(cut_out STUB_SYSTEMCTL_RC=0)
check "a cut without the MathJax modules is refused before the stamp" \
    "preflight: rust/backend/sidecars/mathjax/node_modules is missing — run npm ci in rust/backend/sidecars/mathjax" \
    "$(printf '%s\n' "$out" | grep -m1 '^preflight:' || true)"
check "  and nothing was stamped" "" "$(tree_state)"
mkdir -p "$MJ"
out=$(cut_out STUB_SYSTEMCTL_RC=1)
check "a cut with no user manager reachable is refused before the stamp" \
    "preflight: no systemd user manager is reachable — run the cut in a test container started with --user-manager (the release skill has the command)" \
    "$(printf '%s\n' "$out" | grep -m1 '^preflight:' || true)"
check "  and nothing was stamped" "" "$(tree_state)"
out=$(cut_out STUB_CARGO_TEST_RC=1)
check "a forced test failure ends the cut with a failing status" "exit 1" "$(printf '%s\n' "$out" | tail -n1)"
check "  after the stamp, so the restore ran" "release: stopped after the stamp; the stamped files are restored" \
    "$(printf '%s\n' "$out" | grep -m1 '^release:' || true)"
check "  and left the tree clean" "" "$(tree_state)"
check "  with the stamp back at 0.6.6-dev" 'version = "0.6.6-dev"' "$(grep '^version' "$CUT/rust/Cargo.toml")"

if [ "$fails" -gt 0 ]; then
    echo "FAILED: $fails check(s)"
    exit 1
fi
echo "all checks passed"
