#!/usr/bin/env bash
# test-session-start-hook.sh — the SessionStart hook a row's leg runs after a /clear or a compaction
# (comm/registry/comm-session-start-hook.sh): it prints the row's handle and the line naming its handoff file, and
# changes nothing (no join: the row's work-state facts stay as they were); with no handoff it still exits 0; with
# SOT_COMM_HOOKS=off it prints nothing; a failing or missing start script never fails it. The recipe's side (the
# matchers, the settings) is proved in Rust (`agents::layer::tests::*`).
# Runs against a temp $SOT_COMM_HOME with a pinned self file and the built sotd the guard hands the scripts; no daemon.
#
# Usage: comm/tests/test-session-start-hook.sh     Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home

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
command -v jq > /dev/null 2>&1 || { echo "FATAL: jq is needed to read the registry" >&2; exit 2; }

# The row's project: a scratch repo, the folder the hook runs in (a hook runs in the session's folder).
PROJECT="$WORK/project"; mkdir -p "$PROJECT"
git -C "$PROJECT" init -q 2> /dev/null || { echo "FATAL: git init failed" >&2; exit 2; }
PROJECT="$(cd "$PROJECT" && pwd -P)"
NAME="clear-test"
eval "$(cd "$PROJECT" && "$SCRIPTS_DIR/comm-context.sh" 2> /dev/null | grep -E '^(REPO|PROJECT_ROOT)=')"
printf '%s\nrepo=%s\nroot=%s\n' "$NAME" "$REPO" "$PROJECT_ROOT" > "$SOT_COMM_SELF_FILE"
HANDOFF="$PROJECT_ROOT/dev/output/handoff-$NAME.md"
REGISTRY="$SOT_COMM_HOME/registry.json"
entry() { jq -S ".agents[\"$NAME\"]" "$REGISTRY"; }

PASS=0
FAIL=0
check() {
    local desc="$1" fn="$2"
    if "$fn"; then echo "PASS: $desc"; PASS=$((PASS + 1)); else echo "FAIL: $desc"; FAIL=$((FAIL + 1)); fi
}
run_hook() { ( cd "$PROJECT" && "${1:-$HOOK}" < /dev/null ); }

# The row joined at its start and holds a sticky wait, as a row does when a compaction hits mid-turn.
( cd "$PROJECT" && "$SCRIPTS_DIR/comm-join.sh" > /dev/null 2>&1 ) || { echo "FATAL: the row's join failed" >&2; exit 2; }
( cd "$PROJECT" && "$SCRIPTS_DIR/comm-status.sh" waiting "the local gate on the branch head" > /dev/null 2>&1 )
BEFORE="$(entry)"
[ "$(printf '%s' "$BEFORE" | jq -r '.waiting // empty')" = "the local gate on the branch head" ] || { echo "FATAL: no wait was stamped: $BEFORE" >&2; exit 2; }

case_the_hook_names_the_handle_and_the_handoff() {
    local out rc=0
    mkdir -p "$(dirname "$HANDOFF")"; : > "$HANDOFF"
    out="$(run_hook)"; rc=$?
    [ "$rc" -eq 0 ] || { echo "  exit $rc"; return 1; }
    printf '%s\n' "$out" | grep -q "^BOOTSTRAP-ARM handle=$NAME identity=ok " || { echo "  no bootstrap line for $NAME: $out"; return 1; }
    printf '%s\n' "$out" | grep -qF "HANDOFF: read $HANDOFF before other work" || { echo "  no HANDOFF line naming $HANDOFF: $out"; return 1; }
}
check "after a clear the hook prints the row's handle and the line naming its handoff, and exits 0" case_the_hook_names_the_handle_and_the_handoff

# A join rewrites the registry entry whole (the floor and the wait go); the hook leaves the row as it was.
case_the_row_is_unchanged_across_the_hook() {
    run_hook > /dev/null
    [ "$(entry)" = "$BEFORE" ] || { echo "  before: $BEFORE"; echo "  after:  $(entry)"; return 1; }
}
check "a run of the hook leaves the row's registry entry, its wait included, as it was" case_the_row_is_unchanged_across_the_hook

case_no_handoff_still_exits_zero() {
    local out rc=0
    rm -f "$HANDOFF"
    out="$(run_hook)"; rc=$?
    [ "$rc" -eq 0 ] || { echo "  exit $rc"; return 1; }
    printf '%s\n' "$out" | grep -q "^BOOTSTRAP-ARM handle=$NAME " || { echo "  no bootstrap line: $out"; return 1; }
    ! printf '%s\n' "$out" | grep -q "^HANDOFF:" || { echo "  a HANDOFF line with no file: $out"; return 1; }
}
check "with no handoff file the hook still exits 0 and prints no HANDOFF line" case_no_handoff_still_exits_zero

case_hooks_off_prints_nothing() {
    local out rc=0
    out="$(SOT_COMM_HOOKS=off run_hook)"; rc=$?
    [ "$rc" -eq 0 ] || { echo "  exit $rc"; return 1; }
    [ -z "$out" ] || { echo "  printed: $out"; return 1; }
}
check "with SOT_COMM_HOOKS=off the hook prints nothing" case_hooks_off_prints_nothing

# A start that cannot say who this is tells the session why, on stdout where Claude Code reads it, and the hook exits 0.
case_a_refusal_reaches_stdout() {
    local out rc=0 saved
    saved="$(cat "$REGISTRY")"
    jq 'del(.agents["'"$NAME"'"])' "$REGISTRY" > "$WORK/registry.cut" && cp "$WORK/registry.cut" "$REGISTRY"
    out="$(run_hook)"; rc=$?
    printf '%s' "$saved" > "$REGISTRY"
    [ "$rc" -eq 0 ] || { echo "  exit $rc"; return 1; }
    printf '%s\n' "$out" | grep -q "identity=FAIL" && printf '%s\n' "$out" | grep -q "Run /sot-session-start to join" || { echo "  no reason on stdout: $out"; return 1; }
}
check "a refusal's reason is on stdout and the hook exits 0" case_a_refusal_reaches_stdout

# The hook beside a start script of its own, or none: never a failure.
case_a_missing_start_script_exits_zero() {
    local d="$WORK/alone" out rc=0
    mkdir -p "$d"; cp "$HOOK" "$d/"
    out="$(run_hook "$d/comm-session-start-hook.sh")"; rc=$?
    [ "$rc" -eq 0 ] && [ -z "$out" ] || { echo "  rc=$rc out: $out"; return 1; }
}
check "with no start script beside it the hook prints nothing and exits 0" case_a_missing_start_script_exits_zero

case_a_failing_start_exits_zero_and_shows_why() {
    local d="$WORK/failing" out rc=0
    mkdir -p "$d"; cp "$HOOK" "$d/"
    printf '#!/usr/bin/env bash\necho "the start says why" >&2\nexit 7\n' > "$d/comm-session-start.sh"; chmod +x "$d/comm-session-start.sh"
    out="$(run_hook "$d/comm-session-start-hook.sh")"; rc=$?
    [ "$rc" -eq 0 ] || { echo "  exit $rc"; return 1; }
    printf '%s\n' "$out" | grep -q "the start says why" || { echo "  stderr did not reach stdout: $out"; return 1; }
}
check "a start that fails leaves the hook at exit 0 with its stderr on stdout" case_a_failing_start_exits_zero_and_shows_why

echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
