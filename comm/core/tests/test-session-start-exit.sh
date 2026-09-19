#!/usr/bin/env bash
# test-session-start-exit.sh — a good phase-1 ("arm") cold-start join must
# exit 0 with exactly one BOOTSTRAP-ARM line, and comm-listen.sh's own
# start banner must never leak into that output.
#
# Field bug (2026-09-19, a capsule-row session on the backend): phase 1
# printed BOOTSTRAP-ARM but still exited 1, with comm-listen.sh's multi-line
# "NEXT (required...)" banner dumped ahead of it. Root cause traced to
# sot_pty_screen (called from the capsule wake-detection block) reading
# $ENDPOINT from the caller's scope per comm-lib.sh's own convention, which
# comm-session-start.sh never set before calling it -- under this script's
# `set -u`, that died with "ENDPOINT: unbound variable", silently, output
# eaten by the call's own >/dev/null 2>&1. That crash needs a resolvable
# capsule workspace id + a live daemon to trigger (see the lane report for
# the by-hand repro against a real capsule row); this test instead covers
# the part a hermetic run CAN prove without one: with no capsule context at
# all (no $CLAUDE_CODE_SESSION_ID), a fresh join must still cleanly exit 0,
# print exactly one BOOTSTRAP-ARM line, and never echo comm-listen.sh's raw
# start banner.
#
# Runs against a temp $SOT_COMM_HOME with a pinned self-file -- never
# touches the real ~/.sot-comm, and never starts a real bridge against a
# real daemon (no $SOT_SOCKET/$SOT_RELAY_ENDPOINT is exported here, so the
# bridge loop's own reconnect attempts just fail quietly in the background,
# same as any other offline box). Usage:
#   comm/core/tests/test-session-start-exit.sh
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-session-exit-test-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "mktemp failed" >&2; exit 1; }
cleanup() {
    "$SCRIPTS_DIR/comm-listen.sh" --stop >/dev/null 2>&1 || true
    rm -rf "$WORK"
}
trap cleanup EXIT

export SOT_COMM_HOME="$WORK/home"
export SOT_COMM_SELF_FILE="$WORK/self.txt"
export SOT_COMM_NAME="exit-test-$$"
# Never let an ambient capsule/daemon env leak into this hermetic run --
# exactly the leak that caused the field bug above (a real $SOT_WORKSPACE_ID
# inherited into a script that never expected one).
unset SOT_WORKSPACE_ID CLAUDE_CODE_SESSION_ID SOT_SOCKET SOT_RELAY_ENDPOINT SOT_SPAWN_ENDPOINT
mkdir -p "$SOT_COMM_HOME/state"
ln -s "$SCRIPTS_DIR" "$SOT_COMM_HOME/bin"

OUT="$WORK/out"
"$SCRIPTS_DIR/comm-session-start.sh" >"$OUT" 2>&1
rc=$?

PASS=0; FAIL=0
check() { local d="$1"; shift; if "$@"; then PASS=$((PASS+1)); echo "PASS $d"; else FAIL=$((FAIL+1)); echo "FAIL $d"; fi; }

no_banner_leak() { ! grep -q 'NEXT (required' "$OUT"; }

check "exit 0 on a good join" [ "$rc" -eq 0 ]
check "exactly one BOOTSTRAP-ARM line" [ "$(grep -c '^BOOTSTRAP-ARM ' "$OUT")" -eq 1 ]
check "identity=ok" grep -q 'identity=ok' "$OUT"
check "comm-listen.sh's start banner does not leak" no_banner_leak

echo; echo "$PASS passed, $FAIL failed"
if [ "$FAIL" -ne 0 ]; then echo "--- output ---"; cat "$OUT"; fi
[ "$FAIL" -eq 0 ]
