#!/usr/bin/env bash
# test-launcher-main-notice.sh — a launcher run from a clone of main says first that a box should run a release and how
# to install one, and then carries on to the window: the note is not a failure and it is not at the last check. A
# checkout on any other branch, or on no branch (a release's pinned checkout), says nothing of the kind.
#
# HERMETIC: a copy of the launcher in a repository of the suite's own, a stand-in window, its own HOME; no update, no
# daemon, no build.
#
# Usage: agents/tests/test-launcher-main-notice.sh     Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-home-guard.sh" || exit 2   # never the live comm home

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/launcher-main-notice-test-XXXXXX")"
if [ -z "$WORK" ] || [ ! -d "$WORK" ]; then
    echo "FATAL: mktemp did not produce a usable work directory (got: '$WORK')" >&2
    exit 1
fi
trap 'rm -rf "${WORK:?}"' EXIT
guard_fresh_home "$WORK"

REPO="$WORK/clone"
mkdir -p "$REPO/scripts"
cp -R "$HERE/../../scripts/launch-sot.sh" "$HERE/../../scripts/lib" "$REPO/scripts/" || exit 2
git init -q -b main "$REPO" && git -C "$REPO" add -A \
    && git -C "$REPO" -c user.email=t@t -c user.name=t commit -q -m launcher || { echo "FATAL: no test repo" >&2; exit 2; }
printf '#!/bin/sh\necho "window started"\n' > "$WORK/window"; chmod +x "$WORK/window"

PASS=0
FAIL=0
check() {
    local desc="$1" fn="$2"
    if "$fn"; then echo "PASS: $desc"; PASS=$((PASS + 1)); else echo "FAIL: $desc"; FAIL=$((FAIL + 1)); fi
}
contains() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }

# launch — the launcher's whole output, stdout and stderr in order, and its status in LAUNCH_RC.
LAUNCH_RC=0
launch() {
    local out
    out="$(cd "$WORK" && SOT_NO_UPDATE=1 SOT_FRONTEND_BIN="$WORK/window" SOT_PREFIX="$WORK/prefix" \
        bash "$REPO/scripts/launch-sot.sh" 2>&1)"
    LAUNCH_RC=$?
    printf '%s\n' "$out"
}

case_a_clone_of_main_says_so_first_and_still_launches() {
    local out first
    git -C "$REPO" checkout -q main
    out="$(launch)"
    first="$(printf '%s\n' "$out" | sed -n 1p)"
    contains "$first" "clone of main" || { echo "  first line: $first"; return 1; }
    contains "$first" "run a release" || { echo "  first line: $first"; return 1; }
    contains "$first" "install.sh" || { echo "  no way to install one: $first"; return 1; }
    contains "$out" "window started" && [ "$LAUNCH_RC" -eq 0 ] || { echo "  rc=$LAUNCH_RC: $out"; return 1; }
}

case_another_branch_or_a_pinned_checkout_says_nothing() {
    local out
    git -C "$REPO" checkout -q -b release/0.6.6
    out="$(launch)"
    ! contains "$out" "clone of main" || { echo "  release branch: $out"; return 1; }
    git -C "$REPO" checkout -q --detach
    out="$(launch)"
    ! contains "$out" "clone of main" || { echo "  detached: $out"; return 1; }
    contains "$out" "window started" || { echo "  did not launch: $out"; return 1; }
}

check "a launcher on a clone of main says a box should run a release and how, first, then launches" \
    case_a_clone_of_main_says_so_first_and_still_launches
check "a launcher on another branch or a detached checkout says nothing of main" \
    case_another_branch_or_a_pinned_checkout_says_nothing

echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
