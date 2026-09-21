#!/usr/bin/env bash
# test-session-start-survived.sh — hermetic regression suite for the
# bootstrap's survival check (comm-session-start.sh --context): a watcher
# counts as SURVIVED only when it is alive AND was armed by THIS session.
# Field history (2026-09-06 / 09-08 / 09-10, three boxes): an orphan watcher
# from a dead session passed a liveness-only check and left the new session
# deaf while the bootstrap said "nothing to do".
#
# Since 2026-09-20 it also covers the OTHER half: a survived watcher says
# nothing about the relay bridge, and the block must never tell a session
# with a dead bridge not to re-listen (a peer session sat deaf for half an
# hour on exactly that sentence).
#
# Runs against a temp $SOT_COMM_HOME with a pinned self-file — never touches
# the real ~/.sot-comm. Usage: comm/core/tests/test-session-start-survived.sh
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-survived-test-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "mktemp failed" >&2; exit 1; }
SLEEPERS=()
cleanup() { sot_bridge_stop "${NAME:-}" 2>/dev/null || true; for p in "${SLEEPERS[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null; done; rm -rf "$WORK"; }
trap cleanup EXIT

export SOT_COMM_HOME="$WORK/home"
export SOT_COMM_SELF_FILE="$WORK/self.txt"
export SOT_COMM_TEST_HOST="testhost"
unset SOT_COMM_NAME
mkdir -p "$SOT_COMM_HOME/state"
ln -s "$SCRIPTS_DIR" "$SOT_COMM_HOME/bin"
# shellcheck source=../scripts/comm-lib.sh
source "$SCRIPTS_DIR/comm-lib.sh"
ensure_home
NAME="survived-test"
eval "$("$SCRIPTS_DIR/comm-context.sh" 2>/dev/null | grep -E '^(REPO|PROJECT_ROOT)=')"
sot_write_self_file "$SOT_COMM_SELF_FILE" "$NAME" "$REPO" "$PROJECT_ROOT" || { echo "self-file write failed" >&2; exit 1; }
[ "$("$SCRIPTS_DIR/comm-context.sh" | sed -n 's/^NAME=//p')" = "$NAME" ] || { echo "context did not resolve the pinned self-file" >&2; exit 1; }
# The registry must attribute the handle to OUR project root (_owns_handle).
jq -n --arg n "$NAME" --arg r "$PROJECT_ROOT" '{agents:{($n):{state:"idle", root:$r, repo:"x"}}}' > "$REGISTRY"
MARKER="$SOT_COMM_HOME/state/$NAME.watch"

sleeper() { sleep 300 >/dev/null 2>&1 & local p=$!; SLEEPERS+=("$p"); echo "$p"; }
ctx() { CLAUDE_CODE_SESSION_ID="$1" bash "$SCRIPTS_DIR/comm-session-start.sh" --context 2>"$WORK/err" | head -n1; }

PASS=0; FAIL=0
check() { local d="$1"; shift; if "$@"; then PASS=$((PASS+1)); echo "PASS $d"; else FAIL=$((FAIL+1)); echo "FAIL $d"; fi; }

case_own_live_watcher_survives() {
    local p; p="$(sleeper)"; printf '%s\nsess-A\n' "$p" > "$MARKER"
    [[ "$(ctx sess-A)" == "SURVIVED handle=$NAME listener="* ]] || { echo "    got '$(ctx sess-A)'"; return 1; }
    kill -0 "$p" 2>/dev/null || { echo "    own watcher was killed"; return 1; }
}
case_orphan_from_another_session_is_not_survived_and_reaped() {
    local p; p="$(sleeper)"; printf '%s\nsess-OLD\n' "$p" > "$MARKER"
    local out; out="$(ctx sess-NEW)"
    [[ "$out" == "NOT SURVIVED handle=$NAME"* ]] || { echo "    got '$out'"; return 1; }
    grep -q "ORPHAN watcher pid=$p" "$WORK/err" || { echo "    no reap notice: $(cat "$WORK/err")"; return 1; }
    sleep 0.3; ! kill -0 "$p" 2>/dev/null || { echo "    orphan still alive"; return 1; }
}
case_legacy_marker_without_session_line_keeps_liveness_answer() {
    local p; p="$(sleeper)"; printf '%s' "$p" > "$MARKER"
    [[ "$(ctx sess-B)" == "SURVIVED handle=$NAME listener="* ]] || { echo "    got '$(ctx sess-B)'"; return 1; }
}
case_dead_pid_is_not_survived() {
    local p; p="$(sleeper)"; kill "$p"; wait "$p" 2>/dev/null; printf '%s\nsess-A\n' "$p" > "$MARKER"
    [[ "$(ctx sess-A)" == "NOT SURVIVED handle=$NAME"* ]] || { echo "    got '$(ctx sess-A)'"; return 1; }
}
case_no_session_id_in_env_trusts_liveness() {
    local p; p="$(sleeper)"; printf '%s\nsess-OLD\n' "$p" > "$MARKER"
    local out; out="$(env -u CLAUDE_CODE_SESSION_ID bash "$SCRIPTS_DIR/comm-session-start.sh" --context 2>/dev/null | head -n1)"
    [[ "$out" == "SURVIVED handle=$NAME listener="* ]] || { echo "    got '$out'"; return 1; }
}

# --- the bridge half (2026-09-20) -------------------------------------
# A bridge-shaped loop the real predicate accepts (argv[3]=sot-bridge,
# argv[5]=handle), pointed at a relay that just sleeps: no daemon needed.
printf '#!/usr/bin/env bash\nsleep 300\n' > "$WORK/fake-relay.sh"; chmod +x "$WORK/fake-relay.sh"
full() { CLAUDE_CODE_SESSION_ID="$1" bash "$SCRIPTS_DIR/comm-session-start.sh" --context 2>"$WORK/err"; }

case_live_bridge_is_reported_up_and_keeps_the_do_not_relisten_line() {
    local p; p="$(sleeper)"; printf '%s\nsess-A\n' "$p" > "$MARKER"
    sot_bridge_start "$NAME" "$WORK/fake-relay.sh"
    sot_bridge_running_for "$NAME" || { echo "    fixture bridge did not start"; return 1; }
    local out; out="$(full sess-A)"
    [[ "$(printf '%s' "$out" | head -n1)" == "SURVIVED handle=$NAME listener=up" ]] \
        || { echo "    got '$(printf '%s' "$out" | head -n1)'"; return 1; }
    printf '%s' "$out" | grep -q "Do not re-join, re-listen, or re-poll" \
        || { echo "    a live bridge must keep the do-not-re-listen line"; return 1; }
    sot_bridge_stop "$NAME"
}
case_dead_bridge_is_reported_down_and_never_says_do_not_relisten() {
    local p; p="$(sleeper)"; printf '%s\nsess-A\n' "$p" > "$MARKER"
    sot_bridge_stop "$NAME" 2>/dev/null || true
    local out; out="$(full sess-A)"
    [[ "$(printf '%s' "$out" | head -n1)" == "SURVIVED handle=$NAME listener=down" ]] \
        || { echo "    got '$(printf '%s' "$out" | head -n1)'"; return 1; }
    printf '%s' "$out" | grep -q "re-listen" \
        && { echo "    told a deaf session not to re-listen"; return 1; }
    printf '%s' "$out" | grep -q "comm-listen.sh --name $NAME now" \
        || { echo "    no handle-explicit instruction to restart the listener"; return 1; }
    ! sot_bridge_running_for "$NAME" || { echo "    the query path started a bridge"; return 1; }
}

# The bootstrap path (no flags) is the half that must HEAL, not only
# report -- Codex review of PR 254: both cases above call --context, so a
# regression that turned `_ensure_bridge` back into a query would leave
# them green.
boot() { CLAUDE_CODE_SESSION_ID="$1" timeout 30 bash "$SCRIPTS_DIR/comm-session-start.sh" 2>"$WORK/err" | head -n1; }

case_bootstrap_restarts_a_dead_bridge() {
    local p; p="$(sleeper)"; printf '%s\nsess-A\n' "$p" > "$MARKER"
    sot_bridge_stop "$NAME" 2>/dev/null || true
    local out; out="$(boot sess-A)"
    [[ "$out" == "SURVIVED handle=$NAME listener=restarted" ]] || { echo "    got '$out' (err: $(head -c 300 "$WORK/err"))"; return 1; }
    sot_bridge_running_for "$NAME" || { echo "    reported restarted with no bridge running"; return 1; }
    sot_bridge_stop "$NAME"
}
case_two_racing_starts_leave_one_bridge() {
    sot_bridge_stop "$NAME" 2>/dev/null || true
    sot_bridge_start "$NAME" "$WORK/fake-relay.sh" & local a=$!
    sot_bridge_start "$NAME" "$WORK/fake-relay.sh" & local b=$!
    wait "$a" "$b" 2>/dev/null
    sleep 0.3
    local n; n="$(pgrep -u "$(id -un)" -f "fake-relay.sh $NAME\$" 2>/dev/null | wc -l)"
    [ "$n" -le 1 ] || { echo "    $n bridge loops survived a concurrent start"; return 1; }
    sot_bridge_stop "$NAME"
}

check "own live watcher survives, untouched" case_own_live_watcher_survives
check "orphan armed by another session: NOT SURVIVED and reaped" case_orphan_from_another_session_is_not_survived_and_reaped
check "legacy marker (pid only) keeps the liveness-only answer" case_legacy_marker_without_session_line_keeps_liveness_answer
check "dead pid is not survived" case_dead_pid_is_not_survived
check "no session id in the environment: liveness alone decides" case_no_session_id_in_env_trusts_liveness
check "a live bridge reads listener=up and keeps the do-not-re-listen line" case_live_bridge_is_reported_up_and_keeps_the_do_not_relisten_line
check "a dead bridge reads listener=down and is never told not to re-listen" case_dead_bridge_is_reported_down_and_never_says_do_not_relisten
check "the bootstrap path restarts a dead bridge" case_bootstrap_restarts_a_dead_bridge
check "two racing starts leave at most one bridge" case_two_racing_starts_leave_one_bridge
echo; echo "$PASS passed, $FAIL failed"; [ "$FAIL" -eq 0 ]
