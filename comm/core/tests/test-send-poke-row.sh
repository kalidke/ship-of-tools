#!/usr/bin/env bash
# test-send-poke-row.sh — comm-send.sh's POKE aims at the row the daemon names
# at the moment of asking, and a row the daemon does not have reads as GONE.
#
#   1. `sot_wake_row` asks the daemon which LIVE row declares a handle and
#      refuses on 0 or 2+ instead of guessing: a poke aimed by a guess types
#      into whatever row the guess happens to name.
#   2. The poke uses that answer, never the `workspace_id` the registry
#      stamped once at join and nothing ever refreshes — a session that
#      continued in another row kept waking the row it used to be in.
#   3. The daemon's refusal for a workspace it does not have is reported as
#      gone, never as "not at a free prompt". That wording was a default, not
#      a signal from a row — there was no row.
#
# No bats dependency. HERMETIC, same seams as test-send-routes-to-relay.sh: a
# temp $SOT_COMM_HOME, a per-case $SOT_COMM_SELF_FILE, a pinned
# $SOT_COMM_TEST_HOST, and a COPY of the scripts dir. The copy's comm-lib.sh
# gets `sot_daemon_endpoint` and `sot_oneshot_request` APPENDED — a sourced
# file's later definitions win — so the fake daemon sits at the transport and
# `sot_wake_row`, `sot_row_gone`, `sot_pty_input_gated` and `sot_prompt_free`
# all stay under test. Never the real ~/.sot-comm, never a real daemon.
#
# Both rows join on the SAME pinned host, because the poke is only attempted
# for a peer this box hosts (the sibling suite pins its peer to another host
# precisely so no daemon is ever dialled).
#
# Usage: comm/core/tests/test-send-poke-row.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-comm-send-poke-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
export SOT_COMM_HOME="$WORK/home"
mkdir -p "$SOT_COMM_HOME"
trap 'rm -rf "$WORK"' EXIT

BIN="$WORK/bin"
cp -r "$SCRIPTS_DIR" "$BIN"
cat >> "$BIN/comm-lib.sh" <<'STUB'

# ---- the fake daemon (test only) ------------------------------------------
sot_daemon_endpoint() { printf 'fixture'; }
sot_oneshot_request() {
    case "$2" in
        workspace.list) cat "$SOT_TEST_LIST_FIXTURE" ;;
        pty.screen)     cat "$SOT_TEST_SCREEN_FIXTURE" ;;
        pty.input)      printf '%s' "$1" | jq -r '.payload.workspace_id' >> "$SOT_TEST_INPUT_LOG"
                        cat "$SOT_TEST_INPUT_FIXTURE" ;;
    esac
}
STUB
SEND="$BIN/comm-send.sh"
JOIN="$BIN/comm-join.sh"

HOST_PIN="testhost"
SENDER="t-sender"
PEER="t-peer"
SELF_SENDER="$WORK/self-sender.txt"
SELF_PEER="$WORK/self-peer.txt"

# Exported unconditionally: the stub above runs under `set -u` in whichever
# script in the copy reaches it, so an unset fixture path would abort a join
# rather than fail a case.
export SOT_TEST_LIST_FIXTURE="$WORK/list.json"
export SOT_TEST_SCREEN_FIXTURE="$WORK/screen.json"
export SOT_TEST_INPUT_FIXTURE="$WORK/input.json"
export SOT_TEST_INPUT_LOG="$WORK/input.log"

FREE_PROMPT='{"v":1,"id":1,"kind":"res","op":"pty.screen","payload":{"lines":["banner","❯"],"cursor":{"row":1,"col":2}}}'
ROW_GONE='{"v":1,"id":1,"kind":"res","op":"pty.screen","payload":{"error":"unknown workspace","code":"unknown_workspace"}}'
INPUT_OK='{"v":1,"id":1,"kind":"res","op":"pty.input","payload":{"ok":true,"enter_sent":true}}'

PASS=0; FAIL=0
check() {
    local desc="$1" fn="$2" rc
    "$fn"; rc=$?
    case "$rc" in
        0) echo "PASS: $desc"; PASS=$((PASS + 1)) ;;
        *) echo "FAIL: $desc"; FAIL=$((FAIL + 1)) ;;
    esac
}
contains() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }

# A workspace.list reply declaring @t-peer on each id given, one row each.
list_declares() {
    printf '%s\n' "$@" | jq -Rn --arg h "$PEER" \
        '{v:1,id:1,kind:"res",op:"workspace.list",
          payload:{workspaces:[inputs | select(length > 0)
                               | {workspace_id: ., agent_handle: $h}]}}' \
        > "$SOT_TEST_LIST_FIXTURE"
}

# $1 = the workspace_id stamped into the PEER's registry entry at join. That
# field is the one the poke used to aim by, so every case names it explicitly.
setup_rows() {
    local peer_ws="$1"
    rm -f "$SOT_COMM_HOME/registry.json"
    ( cd "$WORK" && SOT_COMM_SELF_FILE="$SELF_PEER" SOT_COMM_TEST_HOST="$HOST_PIN" \
        SOT_WORKSPACE_ID="$peer_ws" "$JOIN" --name "$PEER" ) >/dev/null 2>&1 || return 1
    ( cd "$WORK" && SOT_COMM_SELF_FILE="$SELF_SENDER" SOT_COMM_TEST_HOST="$HOST_PIN" \
        SOT_WORKSPACE_ID="ws-sender" "$JOIN" --name "$SENDER" ) >/dev/null 2>&1 || return 1
    : > "$SOT_TEST_INPUT_LOG"
    printf '%s' "$INPUT_OK"    > "$SOT_TEST_INPUT_FIXTURE"
    printf '%s' "$FREE_PROMPT" > "$SOT_TEST_SCREEN_FIXTURE"
    list_declares "$peer_ws"
}

SEND_OUT=""; SEND_ERR=""; SEND_RC=0
run_send() {
    SEND_OUT="$(cd "$WORK" && SOT_COMM_SELF_FILE="$SELF_SENDER" SOT_COMM_TEST_HOST="$HOST_PIN" \
        "$SEND" "$@" 2>"$WORK/err.txt")"
    SEND_RC=$?
    SEND_ERR="$(cat "$WORK/err.txt" 2>/dev/null)"
    return 0
}

# S7 — obligation (c). The registry row and the list fixture name the SAME id,
# so the run reaches the gate whether the poke aims by the registry field or
# by the resolver, and what is under test is only what the refusal is called.
case_a_row_the_daemon_does_not_have_reads_as_gone() {
    setup_rows "ws-live" || { echo "  setup: could not join both rows"; return 1; }
    printf '%s' "$ROW_GONE" > "$SOT_TEST_SCREEN_FIXTURE"
    run_send "@$PEER" "into a row that is not there"
    [ "$SEND_RC" -eq 0 ] || { echo "  exited $SEND_RC (out: '$SEND_OUT' err: '$SEND_ERR')"; return 1; }
    contains "$SEND_OUT" "filed -> @$PEER" \
        || { echo "  the frame was not filed: '$SEND_OUT'"; return 1; }
    contains "$SEND_OUT" "is gone (the daemon has no such row)" \
        || { echo "  receipt was '$SEND_OUT'"; return 1; }
    if contains "$SEND_OUT" "not at a free prompt"; then
        echo "  a destroyed row was reported busy: '$SEND_OUT'"; return 1
    fi
    [ ! -s "$SOT_TEST_INPUT_LOG" ] \
        || { echo "  something was typed into a row the daemon does not have"; return 1; }
    return 0
}

check "the daemon's refusal for a row it does not have reads as gone, not busy" case_a_row_the_daemon_does_not_have_reads_as_gone

echo "---"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
