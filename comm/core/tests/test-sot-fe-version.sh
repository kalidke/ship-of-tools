#!/usr/bin/env bash
# test-sot-fe-version.sh — hermetic suite for `sot-fe version` (ADR 0030 §8
# decision 31, cross-referenced ADR 0043 decision 31: "every runtime answers
# what build it is"). No bats dependency, no real daemon: a STUB unix-socket
# daemon answers `version.query` / `workspace.list` with canned replies this
# file controls, so every case is deterministic and fast.
#
# The stub is a persistent `nc -klU` listener fed through a FIFO this script
# keeps a writer fd open on for its whole lifetime (so nc's own read side
# never sees a transient EOF between the two requests one `sot-fe version`
# call makes) plus a `tail -F`-driven watcher that inspects each request
# line's `.op` and writes back whichever canned reply that case pre-staged —
# content-addressed, so it needs no assumption about which of the two ops
# `sot-fe version` asks for first. Never touches a real ~/.sot-comm (a temp
# $SOT_COMM_HOME) or any real daemon socket.
#
# Usage: comm/core/tests/test-sot-fe-version.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"
SOT_FE="$SCRIPTS_DIR/sot-fe"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-fe-version-test-XXXXXX")"
if [ -z "$WORK" ] || [ ! -d "$WORK" ]; then
    echo "FATAL: mktemp did not produce a usable work directory (got: '$WORK')" >&2
    exit 1
fi

export SOT_COMM_HOME="$WORK/home"
mkdir -p "$SOT_COMM_HOME"

STUB_NC_PID=""
STUB_WATCHER_PID=""
cleanup() {
    [ -n "$STUB_WATCHER_PID" ] && kill "$STUB_WATCHER_PID" >/dev/null 2>&1
    [ -n "$STUB_NC_PID" ] && kill "$STUB_NC_PID" >/dev/null 2>&1
    exec 3>&- 2>/dev/null || true
    rm -rf "$WORK"
}
trap cleanup EXIT

PASS=0
FAIL=0
SKIP=0

# check DESC FN — mirrors test-join-disambiguation.sh's own runner exactly:
# FN prints diagnostics and returns 0 = pass, 2 = SKIP, anything else = fail.
check() {
    local desc="$1" fn="$2"
    local rc
    "$fn"
    rc=$?
    case "$rc" in
        0) echo "PASS: $desc"; PASS=$((PASS + 1)) ;;
        2) echo "SKIP: $desc"; SKIP=$((SKIP + 1)) ;;
        *) echo "FAIL: $desc"; FAIL=$((FAIL + 1)) ;;
    esac
}

contains() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }

# --- the stub daemon -----------------------------------------------------
#
# stage_reply OP JSON_LINE — pre-stage the canned reply the watcher sends
# back the next time it sees a request whose `.op` is OP. Must be called
# BEFORE start_stub_daemon (the watcher script embeds the staged replies at
# start time — this suite's cases are all "decide the whole conversation up
# front", never mid-flight reconfiguration).
declare -A STAGED_REPLY
stage_reply() {
    STAGED_REPLY["$1"]="$2"
}

# start_stub_daemon — bind $SOCK (fresh path each call) and arm the
# content-addressed watcher over the currently-staged replies. Blocks
# briefly to let nc actually bind before any caller connects.
SOCK=""
STUBN=0
start_stub_daemon() {
    STUBN=$((STUBN + 1))
    SOCK="$WORK/stub-$STUBN.sock"
    local fifo="$WORK/resp-$STUBN.fifo"
    local reqlog="$WORK/req-$STUBN.log"
    local repliesdir="$WORK/replies-$STUBN"
    mkfifo "$fifo"
    : > "$reqlog"
    mkdir -p "$repliesdir"

    # Freeze the currently-staged replies as plain files, one per op —
    # a DIRECT write, never shell-quoted code generation, so a reply
    # containing quotes/backslashes (real JSON) can never break the
    # backgrounded watcher below.
    local op
    for op in "${!STAGED_REPLY[@]}"; do
        printf '%s' "${STAGED_REPLY[$op]}" > "$repliesdir/$op.json"
    done

    # Keep a writer fd open on the fifo for nc's whole `-k` lifetime (see
    # this file's header doc) — opened BEFORE nc so nc's own open-for-read
    # never blocks waiting for a first writer.
    exec 3<>"$fifo"
    nc -klU "$SOCK" < "$fifo" >> "$reqlog" &
    STUB_NC_PID=$!

    ( tail -n +1 -F "$reqlog" 2>/dev/null | while IFS= read -r line; do
        op="$(printf '%s' "$line" | jq -r '.op // empty' 2>/dev/null)"
        replyfile="$repliesdir/$op.json"
        [ -n "$op" ] && [ -f "$replyfile" ] && printf '%s\n' "$(cat "$replyfile")" >&3
    done ) &
    STUB_WATCHER_PID=$!

    # Bounded wait for the socket to actually exist (nc binds it near-
    # instantly; this just guards a slow-scheduling CI box).
    local deadline=$((SECONDS + 5))
    while [ ! -S "$SOCK" ]; do
        [ "$SECONDS" -lt "$deadline" ] || { echo "stub daemon socket never appeared: $SOCK" >&2; break; }
        sleep 0.05
    done
}

stop_stub_daemon() {
    [ -n "$STUB_WATCHER_PID" ] && kill "$STUB_WATCHER_PID" >/dev/null 2>&1
    [ -n "$STUB_NC_PID" ] && kill "$STUB_NC_PID" >/dev/null 2>&1
    wait "$STUB_WATCHER_PID" 2>/dev/null
    wait "$STUB_NC_PID" 2>/dev/null
    exec 3>&- 2>/dev/null || true
    STUB_NC_PID=""
    STUB_WATCHER_PID=""
    STAGED_REPLY=()
}

# run_version [ARGS...] — invoke `sot-fe version --endpoint unix:$SOCK` with
# a short timeout (the stub replies near-instantly; a real hang here means a
# genuine bug, not a slow daemon). Sets VER_OUT / VER_RC.
VER_OUT=""; VER_RC=0
run_version() {
    VER_OUT="$("$SOT_FE" version --endpoint "unix:$SOCK" --timeout 5 "$@" 2>&1)"
    VER_RC=$?
}

# --- cases -----------------------------------------------------------------

case_matching_pair_prints_matches() {
    stage_reply "version.query" '{"v":1,"id":2,"kind":"res","op":"version.query","payload":{"daemon":{"app_version":"0.6.0-dev+abc1234","protocol":1,"lane_build":"abc1234def"},"clients":[{"client_id":"fe-1","app_version":"0.6.0-dev+abc1234","protocol":1,"connected_at":1}]}}'
    stage_reply "workspace.list" '{"v":1,"id":3,"kind":"res","op":"workspace.list","payload":{"workspaces":[{"workspace_id":"ws1","slug":"research","label":"","project_root":"/p","tmux_session":"t","kernel_running":false,"is_default":false,"runtime":"capsule","state_dir":"/sd","phase":"ready"}]}}'
    start_stub_daemon
    run_version
    stop_stub_daemon

    [ "$VER_RC" -eq 0 ] || { echo "  expected exit 0, got $VER_RC. Output:\n$VER_OUT"; return 1; }
    contains "$VER_OUT" "backend daemon" || { echo "  missing backend daemon row: $VER_OUT"; return 1; }
    contains "$VER_OUT" "abc1234def" || { echo "  missing daemon lane_build: $VER_OUT"; return 1; }
    contains "$VER_OUT" "frontend fe-1" || { echo "  missing attached-client row: $VER_OUT"; return 1; }
    contains "$VER_OUT" "row research" || { echo "  missing capsule row: $VER_OUT"; return 1; }
    contains "$VER_OUT" "matches" || { echo "  expected a 'matches' verdict for a ready row: $VER_OUT"; return 1; }
    contains "$VER_OUT" "MISMATCH" && { echo "  unexpected MISMATCH for a ready row: $VER_OUT"; return 1; }
    return 0
}

case_foreign_row_prints_mismatch() {
    stage_reply "version.query" '{"v":1,"id":2,"kind":"res","op":"version.query","payload":{"daemon":{"app_version":"0.6.0-dev+abc1234","protocol":1,"lane_build":"abc1234def"},"clients":[]}}'
    stage_reply "workspace.list" '{"v":1,"id":3,"kind":"res","op":"workspace.list","payload":{"workspaces":[{"workspace_id":"ws2","slug":"scratch","label":"","project_root":"/p2","tmux_session":"t2","kernel_running":false,"is_default":false,"runtime":"capsule","state_dir":"/sd2","phase":"foreign"}]}}'
    start_stub_daemon
    run_version
    stop_stub_daemon

    [ "$VER_RC" -eq 0 ] || { echo "  expected exit 0, got $VER_RC. Output:\n$VER_OUT"; return 1; }
    contains "$VER_OUT" "row scratch" || { echo "  missing capsule row: $VER_OUT"; return 1; }
    contains "$VER_OUT" "foreign" || { echo "  missing foreign phase in the row: $VER_OUT"; return 1; }
    contains "$VER_OUT" "MISMATCH" || { echo "  expected a MISMATCH verdict for a foreign row: $VER_OUT"; return 1; }
    return 0
}

case_legacy_daemon_predating_version_query_prints_unknown_and_exits_0() {
    # The shape a daemon that predates version.query answers with:
    # server.rs's generic `other =>` catch-all, the SAME op echoed back on
    # a `res` frame (never a transport failure) — `sot-fe version` must
    # treat this as "daemon predates this op", not a failure.
    stage_reply "version.query" '{"v":1,"id":2,"kind":"res","op":"version.query","payload":{"error":"unknown op: version.query"}}'
    stage_reply "workspace.list" '{"v":1,"id":3,"kind":"res","op":"workspace.list","payload":{"workspaces":[]}}'
    start_stub_daemon
    run_version
    stop_stub_daemon

    [ "$VER_RC" -eq 0 ] || { echo "  expected exit 0 against a legacy daemon, got $VER_RC. Output:\n$VER_OUT"; return 1; }
    contains "$VER_OUT" "unknown" || { echo "  expected 'unknown' for the daemon's version/build: $VER_OUT"; return 1; }
    return 0
}

case_local_daemon_label_used_with_no_explicit_endpoint() {
    # `sot-fe version` with NO --endpoint must still resolve somewhere —
    # this suite has no real local daemon, so drive it via $SOT_FE_ENDPOINT
    # instead (resolve_endpoint's own env fallback), which is exactly the
    # "no --endpoint flag" case the local-vs-backend label distinguishes.
    stage_reply "version.query" '{"v":1,"id":2,"kind":"res","op":"version.query","payload":{"daemon":{"app_version":"0.6.0","protocol":1,"lane_build":"xyz"},"clients":[]}}'
    stage_reply "workspace.list" '{"v":1,"id":3,"kind":"res","op":"workspace.list","payload":{"workspaces":[]}}'
    start_stub_daemon
    VER_OUT="$(SOT_FE_ENDPOINT="unix:$SOCK" "$SOT_FE" version --timeout 5 2>&1)"
    VER_RC=$?
    stop_stub_daemon

    [ "$VER_RC" -eq 0 ] || { echo "  expected exit 0, got $VER_RC. Output:\n$VER_OUT"; return 1; }
    contains "$VER_OUT" "local daemon" || { echo "  expected the 'local daemon' label with no --endpoint flag: $VER_OUT"; return 1; }
    contains "$VER_OUT" "backend daemon" && { echo "  unexpected 'backend daemon' label with no --endpoint flag: $VER_OUT"; return 1; }
    return 0
}

case_comm_scripts_row_reads_the_installed_version_file() {
    stage_reply "version.query" '{"v":1,"id":2,"kind":"res","op":"version.query","payload":{"daemon":{"app_version":"0.6.0","protocol":1,"lane_build":"xyz"},"clients":[]}}'
    stage_reply "workspace.list" '{"v":1,"id":3,"kind":"res","op":"workspace.list","payload":{"workspaces":[]}}'
    printf 'deadbeef9\n' > "$SOT_COMM_HOME/VERSION"
    start_stub_daemon
    run_version
    stop_stub_daemon

    contains "$VER_OUT" "comm scripts" || { echo "  missing comm scripts row: $VER_OUT"; return 1; }
    contains "$VER_OUT" "deadbeef9" || { echo "  comm scripts row didn't read \$SOT_COMM_HOME/VERSION: $VER_OUT"; return 1; }
    return 0
}

# --- run ---------------------------------------------------------------

check "a matching pair prints the daemon/client rows and a 'matches' capsule verdict" case_matching_pair_prints_matches
check "a foreign-phase capsule row prints a MISMATCH verdict"                          case_foreign_row_prints_mismatch
check "a daemon that predates version.query prints 'unknown' and still exits 0"        case_legacy_daemon_predating_version_query_prints_unknown_and_exits_0
check "no --endpoint flag labels the daemon row 'local daemon', not 'backend daemon'"  case_local_daemon_label_used_with_no_explicit_endpoint
check "the comm scripts row reads the installed \$SOT_COMM_HOME/VERSION file"          case_comm_scripts_row_reads_the_installed_version_file

echo ""
echo "$PASS passed, $FAIL failed, $SKIP skipped"
[ "$FAIL" -eq 0 ]
