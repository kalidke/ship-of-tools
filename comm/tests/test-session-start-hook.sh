#!/usr/bin/env bash
# test-session-start-hook.sh — the SessionStart hook a row's leg runs after a /clear or a compaction
# (comm/work_state/hooks/comm-session-start-hook.sh): it runs the comm session start for the row and prints the handle
# and the line naming the row's handoff file; with no handoff it still exits 0; with SOT_COMM_HOOKS=off it prints nothing
# and joins nothing. The recipe's side (the matchers, the settings) is proved in Rust (`agents::layer::tests::*`).
# Runs against a temp $SOT_COMM_HOME with a pinned self file and the built sotd the guard hands the scripts; no daemon.
#
# Usage: comm/tests/test-session-start-hook.sh     Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-session-start-hook-test-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "mktemp failed" >&2; exit 1; }
trap 'rm -rf "${WORK:?}"' EXIT

export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
export SOT_COMM_SELF_FILE="$WORK/self.txt"
export SOT_COMM_TEST_HOST="testhost"
unset SOT_COMM_NAME CLAUDE_CODE_SESSION_ID
mkdir -p "$SOT_COMM_HOME"
ln -s "$SCRIPTS_DIR" "$SOT_COMM_HOME/bin"
HOOK="$SCRIPTS_DIR/comm-session-start-hook.sh"
[ -x "$HOOK" ] || { echo "FATAL: the staged bin has no $HOOK" >&2; exit 2; }

# The row's project: a scratch repo, the folder the hook runs in (a hook runs in the session's folder).
PROJECT="$WORK/project"; mkdir -p "$PROJECT"
git -C "$PROJECT" init -q 2> /dev/null || { echo "FATAL: git init failed" >&2; exit 2; }
PROJECT="$(cd "$PROJECT" && pwd -P)"
NAME="clear-test"
eval "$(cd "$PROJECT" && "$SCRIPTS_DIR/comm-context.sh" 2> /dev/null | grep -E '^(REPO|PROJECT_ROOT)=')"
printf '%s\nrepo=%s\nroot=%s\n' "$NAME" "$REPO" "$PROJECT_ROOT" > "$SOT_COMM_SELF_FILE"
HANDOFF="$PROJECT_ROOT/dev/output/handoff-$NAME.md"

PASS=0
FAIL=0
check() {
    local desc="$1" fn="$2"
    if "$fn"; then echo "PASS: $desc"; PASS=$((PASS + 1)); else echo "FAIL: $desc"; FAIL=$((FAIL + 1)); fi
}
run_hook() { ( cd "$PROJECT" && "$HOOK" < /dev/null 2> "$WORK/hook.err" ); }

case_the_hook_names_the_handle_and_the_handoff() {
    local out rc=0
    mkdir -p "$(dirname "$HANDOFF")"; : > "$HANDOFF"
    out="$(run_hook)"; rc=$?
    [ "$rc" -eq 0 ] || { echo "  exit $rc"; return 1; }
    printf '%s\n' "$out" | grep -q "^BOOTSTRAP-ARM handle=$NAME identity=ok " || { echo "  no bootstrap line for $NAME: $out"; return 1; }
    printf '%s\n' "$out" | grep -qF "HANDOFF: read $HANDOFF before other work" || { echo "  no HANDOFF line naming $HANDOFF: $out"; return 1; }
}
check "after a clear the hook prints the row's handle and the line naming its handoff, and exits 0" case_the_hook_names_the_handle_and_the_handoff

# The session start is safe to run in a live row again: the same pinned handle, no mismatch.
case_a_second_run_keeps_the_handle() {
    local out
    out="$(run_hook)"
    printf '%s\n' "$out" | grep -q "^BOOTSTRAP-ARM handle=$NAME identity=ok " || { echo "  second run: $out"; return 1; }
    [ "$(sed -n 1p "$SOT_COMM_SELF_FILE")" = "$NAME" ] || { echo "  the self file now names $(sed -n 1p "$SOT_COMM_SELF_FILE")"; return 1; }
}
check "a second run in the same row re-declares the same handle" case_a_second_run_keeps_the_handle

case_no_handoff_still_exits_zero() {
    local out rc=0
    rm -f "$HANDOFF"
    out="$(run_hook)"; rc=$?
    [ "$rc" -eq 0 ] || { echo "  exit $rc"; return 1; }
    printf '%s\n' "$out" | grep -q "^BOOTSTRAP-ARM handle=$NAME " || { echo "  no bootstrap line: $out"; return 1; }
    ! printf '%s\n' "$out" | grep -q "^HANDOFF:" || { echo "  a HANDOFF line with no file: $out"; return 1; }
}
check "with no handoff file the hook still exits 0 and prints no HANDOFF line" case_no_handoff_still_exits_zero

case_hooks_off_prints_and_joins_nothing() {
    local out rc=0
    mkdir -p "$(dirname "$HANDOFF")"; : > "$HANDOFF"
    rm -f "$SOT_COMM_HOME/registry.json"   # the earlier cases joined; this one must find the row not joined
    out="$(SOT_COMM_HOOKS=off run_hook)"; rc=$?
    [ "$rc" -eq 0 ] || { echo "  exit $rc"; return 1; }
    [ -z "$out" ] || { echo "  printed: $out"; return 1; }
    ! grep -q "\"$NAME\"" "$SOT_COMM_HOME/registry.json" 2> /dev/null || { echo "  $NAME joined the registry"; return 1; }
}
check "with SOT_COMM_HOOKS=off the hook prints nothing and joins nothing" case_hooks_off_prints_and_joins_nothing

echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
