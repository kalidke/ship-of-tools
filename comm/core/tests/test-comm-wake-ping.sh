#!/usr/bin/env bash
# test-comm-wake-ping.sh — `comm-wake.sh <handle> --deliver ping`'s notice
# path: one fixed line typed for a whole batch of new directed messages
# (never the message text itself — the session reads that with
# comm-poll.sh), the selftest-only variant, the prompt-free gate, ping
# coalescing (an unread ping suppresses a second one; a moved cursor lets
# the next one through), workspace-id derivation from $SOT_COMM_SELF_FILE,
# and the agent-liveness exit.
#
# Cursor starts at the inbox's END (same rule `full` mode already pins,
# proven by test-codex-watch-capsule-loop.sh): every case's inbox starts
# EMPTY and new lines are appended from inside the `sleep` stub, one poll
# cycle at a time — never pre-populated before the watcher starts.
#
# Each case runs `_comm_wake_main` in its own script file under a fresh
# sandbox $SOT_COMM_HOME, invoked via `bash script.sh` (not `bash -c` string
# splicing) so env is passed by export, not quoting -- stubs
# `_comm_wake_find_agent_pid`/`_comm_wake_pty_screen`/`_comm_wake_pty_input`/
# `sleep`/`kill` as needed, same seams the existing codex-watch tests stub.
#
# Usage: comm/core/tests/test-comm-wake-ping.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"
export WAKE="$SCRIPTS_DIR/comm-wake.sh"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-comm-wake-ping-test-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "mktemp failed" >&2; exit 1; }
trap 'rm -rf "$WORK"' EXIT

PASS=0
FAIL=0
check() {
    local desc="$1" fn="$2"
    if "$fn"; then
        echo "PASS: $desc"; PASS=$((PASS + 1))
    else
        echo "FAIL: $desc"; FAIL=$((FAIL + 1))
    fi
}

# A default agent-liveness stub used by every case except the one that
# exercises it for real: no owner found, so the liveness gate never fires.
NO_OWNER_STUB='_comm_wake_find_agent_pid() { return 1; }'

case_three_new_directed_lines_type_the_ping_once() {
    local d="$WORK/three-lines"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state"
    : > "$d/inbox/watchee.jsonl"
    local calls="$d/pty-input.calls" attempts="$d/pty-input.log"
    : > "$calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
$NO_OWNER_STUB
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["banner","❯"]}}'; }
_comm_wake_pty_input() {
    printf x >> "$calls"
    printf '%s' "\$2" | base64 -d >> "$attempts"; printf '\n' >> "$attempts"
    printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'
}
turns=0
sleep() {
    turns=\$((turns + 1))
    if [ "\$turns" -eq 1 ]; then
        printf '{"from":"peer","to":"me","msg":"one"}\n{"from":"peer","to":"me","msg":"two"}\n{"from":"peer","to":"me","msg":"three"}\n' >> "$d/inbox/watchee.jsonl"
    fi
    [ "\$turns" -le 2 ] || exit 0
}
_comm_wake_main watchee --deliver ping
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 1 ] || { echo "  pty.input called $n time(s), want exactly 1"; cat "$attempts" 2>/dev/null; return 1; }
    # Exact match (not a substring grep): $d's own path (.../three-lines/...)
    # legitimately contains "three", so a loose grep for the message words
    # would false-positive on the sandbox path embedded in the ping text.
    local expected="[sot-comm] new message for @watchee — run $d/bin/comm-poll.sh"
    [ "$(cat "$attempts" 2>/dev/null)" = "$expected" ] || { echo "  typed text was '$(cat "$attempts" 2>/dev/null)', want '$expected'"; return 1; }
    return 0
}

case_selftest_only_batch_types_the_selftest_text() {
    local d="$WORK/selftest-only"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state"
    : > "$d/inbox/watchee.jsonl"
    local calls="$d/pty-input.calls" attempts="$d/pty-input.log"
    : > "$calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
$NO_OWNER_STUB
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["❯"]}}'; }
_comm_wake_pty_input() {
    printf x >> "$calls"
    printf '%s' "\$2" | base64 -d >> "$attempts"; printf '\n' >> "$attempts"
    printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'
}
turns=0
sleep() {
    turns=\$((turns + 1))
    if [ "\$turns" -eq 1 ]; then
        printf '{"from":"__selftest__","to":"me","msg":"ping"}\n{"from":"__selftest__","to":"me","msg":"ping2"}\n' >> "$d/inbox/watchee.jsonl"
    fi
    [ "\$turns" -le 2 ] || exit 0
}
_comm_wake_main watchee --deliver ping
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 1 ] || { echo "  pty.input called $n time(s), want exactly 1"; return 1; }
    grep -q "wake selftest OK" "$attempts" || { echo "  typed text was not the selftest notice: $(cat "$attempts")"; return 1; }
    return 0
}

case_prompt_not_free_waits_then_types_once_free() {
    local d="$WORK/prompt-gate"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state"
    : > "$d/inbox/watchee.jsonl"
    local calls="$d/pty-input.calls" screen_calls="$d/screen.calls"
    : > "$calls"; : > "$screen_calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
$NO_OWNER_STUB
_comm_wake_pty_screen() {
    printf x >> "$screen_calls"
    local n; n=\$(wc -c < "$screen_calls")
    if [ "\$n" -ge 2 ]; then
        printf '%s' '{"payload":{"lines":["banner","❯"]}}'
    else
        printf '%s' '{"payload":{"lines":["Allow this action? (y/n)"]}}'
    fi
}
_comm_wake_pty_input() { printf x >> "$calls"; printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'; }
turns=0
sleep() {
    turns=\$((turns + 1))
    if [ "\$turns" -eq 1 ]; then
        printf '{"from":"peer","to":"me","msg":"hello"}\n' >> "$d/inbox/watchee.jsonl"
    fi
    [ "\$turns" -le 3 ] || exit 0
}
_comm_wake_main watchee --deliver ping
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 1 ] || { echo "  pty.input called $n time(s) while gated on the prompt, want exactly 1 (once free)"; return 1; }
    local sc; sc="$(wc -c < "$screen_calls" 2>/dev/null || echo 0)"
    [ "$sc" -ge 2 ] || { echo "  the prompt was never re-checked after being not-free"; return 1; }
    return 0
}

case_outstanding_ping_suppresses_a_second_until_cursor_moves() {
    local d="$WORK/coalesce"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state" "$d/read"
    : > "$d/inbox/watchee.jsonl"
    local calls="$d/pty-input.calls"
    : > "$calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
$NO_OWNER_STUB
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["❯"]}}'; }
_comm_wake_pty_input() { printf x >> "$calls"; printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'; }
turns=0
sleep() {
    turns=\$((turns + 1))
    case "\$turns" in
        1) printf '{"from":"peer","to":"me","msg":"first"}\n' >> "$d/inbox/watchee.jsonl" ;;
        2) printf '{"from":"peer","to":"me","msg":"second"}\n' >> "$d/inbox/watchee.jsonl" ;;
        3) touch "$d/read/watchee.cursor" ;;
    esac
    [ "\$turns" -le 4 ] || exit 0
}
_comm_wake_main watchee --deliver ping
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 2 ] || { echo "  pty.input called $n time(s), want exactly 2 (one for the first line, none while outstanding, one more once the cursor moved)"; return 1; }
    return 0
}

case_workspace_id_derived_from_self_file_basename() {
    local d="$WORK/derived-ws"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state" "$d/self"
    : > "$d/inbox/watchee.jsonl"
    : > "$d/self/testhost__ws-derived.txt"
    local calls="$d/pty-input.calls" wsid_seen="$d/wsid.seen"
    : > "$calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
unset SOT_WORKSPACE_ID
export SOT_COMM_HOME="$d" SOT_COMM_SELF_FILE="$d/self/testhost__ws-derived.txt"
sot_daemon_endpoint() { printf fixture; }
$NO_OWNER_STUB
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["❯"]}}'; }
_comm_wake_pty_input() {
    printf x >> "$calls"
    printf '%s' "\$1" > "$wsid_seen"
    printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'
}
turns=0
sleep() {
    turns=\$((turns + 1))
    if [ "\$turns" -eq 1 ]; then
        printf '{"from":"peer","to":"me","msg":"hello"}\n' >> "$d/inbox/watchee.jsonl"
    fi
    [ "\$turns" -le 2 ] || exit 0
}
_comm_wake_main watchee --deliver ping
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 1 ] || { echo "  pty.input called $n time(s), want exactly 1 (workspace id must have resolved -- rc 3 would call it 0 times)"; return 1; }
    [ "$(cat "$wsid_seen" 2>/dev/null)" = "ws-derived" ] || { echo "  workspace id passed to pty.input was '$(cat "$wsid_seen" 2>/dev/null)', want 'ws-derived'"; return 1; }
    return 0
}

case_agent_pid_gone_exits_zero_and_removes_the_marker() {
    local d="$WORK/agent-gone"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state"
    : > "$d/inbox/watchee.jsonl"
    local marker="$d/state/watchee.watch"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_find_agent_pid() { printf '99999\n'; }
kill() { [ "\$1" = "-0" ] && return 1; command kill "\$@"; }
sleep() { echo "sleep must not be called once the owner is gone" >&2; exit 9; }
_comm_wake_main watchee --deliver ping
EOF
    bash "$d/run.sh" 2>/dev/null
    local rc=$?
    [ "$rc" -eq 0 ] || { echo "  exited $rc, want 0 (owner gone)"; return 1; }
    [ ! -f "$marker" ] || { echo "  the liveness marker was left behind after the owner was gone"; return 1; }
    return 0
}

check "three new directed lines type the ping notice exactly once" case_three_new_directed_lines_type_the_ping_once
check "a batch that is only __selftest__ frames types the selftest notice" case_selftest_only_batch_types_the_selftest_text
check "a not-free prompt withholds the ping and types it once the prompt frees up" case_prompt_not_free_waits_then_types_once_free
check "an outstanding (unread) ping suppresses a second until the cursor moves" case_outstanding_ping_suppresses_a_second_until_cursor_moves
check "the workspace id derives from SOT_COMM_SELF_FILE's basename" case_workspace_id_derived_from_self_file_basename
check "the owning agent gone ends the watcher and removes its marker" case_agent_pid_gone_exits_zero_and_removes_the_marker

echo "---"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
