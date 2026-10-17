#!/usr/bin/env bash
# test-sot-fe-goto.sh — hermetic suite for `sot-fe`'s refusals before anything is sent. `goto` switches a window's view
# and starts nothing, so it has no `--boot` (the window never acted on one); `--urgent` acts only with `--fe`, so
# `preview` and `reveal` refuse it alone. Each is refused before an endpoint is resolved, so nothing is sent. No daemon: the endpoint named is a socket path nobody listens on, in this suite's own work directory; never a
# real ~/.sot-comm (a temp $SOT_COMM_HOME) or a real daemon socket.
#
# Usage: agents/tests/test-sot-fe-goto.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-home-guard.sh" || exit 2   # never the live comm home

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-fe-goto-test-XXXXXX")"
if [ -z "$WORK" ] || [ ! -d "$WORK" ]; then
    echo "FATAL: mktemp did not produce a usable work directory (got: '$WORK')" >&2
    exit 1
fi
trap 'rm -rf "${WORK:?}"' EXIT

export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
export SOTD_BIN="$(guard_bridge_stub "$WORK/bridge")"
[ -x "$SOTD_BIN" ] || exit 2
SOT_FE="$SCRIPTS_DIR/sot-fe"
mkdir -p "$SOT_COMM_HOME"

PASS=0
FAIL=0
check() {
    if "$2"; then echo "PASS: $1"; PASS=$((PASS + 1)); else echo "FAIL: $1"; FAIL=$((FAIL + 1)); fi
}

case_boot_is_refused() {
    local out rc
    out="$("$SOT_FE" goto demo --boot --endpoint "unix:$WORK/nobody.sock" --timeout 5 2>&1)"
    rc=$?
    [ "$rc" = 1 ] && [[ $out == *"ERROR: unknown flag '--boot'"* ]] || { echo "rc=$rc out=$out"; return 1; }
}

case_urgent_needs_fe() {
    local out rc
    out="$("$SOT_FE" preview demo a.png --urgent --endpoint "unix:$WORK/nobody.sock" --timeout 5 2>&1)"
    rc=$?
    [ "$rc" = 1 ] && [[ $out == *"--urgent needs --fe"* ]] || { echo "rc=$rc out=$out"; return 1; }
    out="$("$SOT_FE" reveal demo a.png --urgent --fe host-x --endpoint "unix:$WORK/nobody.sock" --timeout 5 2>&1)"
    [[ $out != *"--urgent needs --fe"* ]] || { echo "refused with --fe: $out"; return 1; }
}

check "goto --boot is refused as an unknown flag before anything is sent" case_boot_is_refused
check "preview --urgent without --fe is refused before anything is sent" case_urgent_needs_fe

echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
