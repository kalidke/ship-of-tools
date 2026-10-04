#!/usr/bin/env bash
# test-sot-fe-reauth.sh — hermetic suite for `sot-fe reauth` (ADR 0046
# decision 6). The verb moves only the row it runs in: it takes the account
# alone and sends $SOT_WORKSPACE_ID, because the conversation it resumes is
# this session's own and no other row's. A second argument, or no row id, is
# refused before an endpoint is resolved and nothing is sent.
#
# No real daemon: a STUB unix-socket daemon (the same harness as
# test-sot-fe-version.sh) answers `workspace.reauth` with a canned reply and
# logs every request line, so each case can count what was sent. Never touches
# a real ~/.sot-comm (a temp $SOT_COMM_HOME) or any real daemon socket.
#
# Usage: comm/core/tests/test-sot-fe-reauth.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-fe-reauth-test-XXXXXX")"
if [ -z "$WORK" ] || [ ! -d "$WORK" ]; then
    echo "FATAL: mktemp did not produce a usable work directory (got: '$WORK')" >&2
    exit 1
fi

export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
SOT_FE="$SCRIPTS_DIR/sot-fe"
mkdir -p "$SOT_COMM_HOME"

STUB_NC_PID=""
STUB_WATCHER_PID=""
cleanup() {
    [ -n "$STUB_WATCHER_PID" ] && kill "$STUB_WATCHER_PID" >/dev/null 2>&1
    [ -n "$STUB_NC_PID" ] && kill "$STUB_NC_PID" >/dev/null 2>&1
    exec 3>&- 2>/dev/null || true
    rm -rf "${WORK:?}"
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
    # The watcher is a subshell whose `tail -F | while` children outlive a
    # kill of the subshell alone and keep this script's stdout open — a
    # caller piping the suite (`| tail`) then never sees EOF. Kill the
    # children first.
    [ -n "$STUB_WATCHER_PID" ] && pkill -TERM -P "$STUB_WATCHER_PID" >/dev/null 2>&1
    [ -n "$STUB_WATCHER_PID" ] && kill "$STUB_WATCHER_PID" >/dev/null 2>&1
    [ -n "$STUB_NC_PID" ] && kill "$STUB_NC_PID" >/dev/null 2>&1
    wait "$STUB_WATCHER_PID" 2>/dev/null
    wait "$STUB_NC_PID" 2>/dev/null
    exec 3>&- 2>/dev/null || true
    STUB_NC_PID=""
    STUB_WATCHER_PID=""
    STAGED_REPLY=()
}

# --- helpers ---------------------------------------------------------------

# run_reauth ENVSPEC... -- ARGS...: run sot-fe reauth against the stub with only the given SOT_/CLAUDE_ vars.
RA_OUT=""; RA_RC=0
run_reauth() {
    local envs=()
    while [ "$1" != "--" ]; do envs+=("$1"); shift; done; shift
    RA_OUT="$(env -u SOT_WORKSPACE_ID -u CLAUDE_CODE_SESSION_ID "${envs[@]}" "$SOT_FE" reauth "$@" --endpoint "unix:$SOCK" --timeout 5 2>&1)"
    RA_RC=$?
}
sent_reauth() { grep -c '"op":"workspace.reauth"' "$WORK/req-$STUBN.log"; }

ACCEPT='{"v":1,"id":2,"kind":"res","op":"workspace.reauth","payload":{"code":"reauth_accepted","account":"acct2","workspace_id":"ws-self-1a2b"}}'

# --- cases -----------------------------------------------------------------

case_second_argument_refused() {
    stage_reply workspace.reauth "$ACCEPT"; start_stub_daemon
    run_reauth SOT_WORKSPACE_ID=ws-self-1a2b CLAUDE_CODE_SESSION_ID=sess-1 -- ws-other-9f9f acct2
    local n; n="$(sent_reauth)"; stop_stub_daemon
    [ "$RA_RC" = 2 ] && contains "$RA_OUT" "moves only the row it runs in" && [ "$n" = 0 ] \
        || { echo "rc=$RA_RC sent=$n out=$RA_OUT"; return 1; }
}

case_request_names_own_row() {
    stage_reply workspace.reauth "$ACCEPT"; start_stub_daemon
    run_reauth SOT_WORKSPACE_ID=ws-self-1a2b CLAUDE_CODE_SESSION_ID=sess-1 -- acct2
    local n w r a
    n="$(sent_reauth)"
    w="$(jq -r 'select(.op=="workspace.reauth") | .payload.workspace_id' "$WORK/req-$STUBN.log")"
    r="$(jq -r 'select(.op=="workspace.reauth") | .payload.resume' "$WORK/req-$STUBN.log")"
    a="$(jq -r 'select(.op=="workspace.reauth") | .payload.account' "$WORK/req-$STUBN.log")"
    stop_stub_daemon
    [ "$RA_RC" = 0 ] && [ "$n" = 1 ] && [ "$w" = ws-self-1a2b ] && [ "$r" = sess-1 ] && [ "$a" = acct2 ] \
        || { echo "rc=$RA_RC sent=$n ws=$w resume=$r account=$a out=$RA_OUT"; return 1; }
}

case_unset_row_id_refused() {
    stage_reply workspace.reauth "$ACCEPT"; start_stub_daemon
    run_reauth CLAUDE_CODE_SESSION_ID=sess-1 -- acct2
    local n; n="$(sent_reauth)"; stop_stub_daemon
    [ "$RA_RC" = 2 ] && contains "$RA_OUT" "SOT_WORKSPACE_ID is unset" && [ "$n" = 0 ] \
        || { echo "rc=$RA_RC sent=$n out=$RA_OUT"; return 1; }
}

check "a second argument is refused and nothing is sent"               case_second_argument_refused
check "the request names the caller's own row"                         case_request_names_own_row
check "an unset SOT_WORKSPACE_ID is refused and nothing is sent"       case_unset_row_id_refused

echo ""
echo "$PASS passed, $FAIL failed, $SKIP skipped"
[ "$FAIL" -eq 0 ]
