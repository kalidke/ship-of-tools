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
# 0031 B1: the record a daemon writes at startup. Without it no script
# appends locally, and every filing here would go to a daemon instead.
mkdir -p "$SOT_COMM_HOME/inbox"
bash -c 'source "$1"; sot_inbox_lock_identity "$INBOX_DIR"' _ "$SCRIPTS_DIR/comm-lib.sh" > "$SOT_COMM_HOME/inbox-lock-manager"
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

RES_OUT=""; RES_RC=0
# sot_wake_row run against the fake daemon, out of the copy's own comm-lib.sh.
resolve() {
    RES_OUT="$(bash -c 'source "$1/comm-lib.sh"; ENDPOINT=fixture; sot_wake_row "$2"' \
        _ "$BIN" "$1" 2>/dev/null)"
    RES_RC=$?
    return 0
}

SEND_OUT=""; SEND_ERR=""; SEND_RC=0
run_send() {
    SEND_OUT="$(cd "$WORK" && SOT_COMM_SELF_FILE="$SELF_SENDER" SOT_COMM_TEST_HOST="$HOST_PIN" \
        "$SEND" "$@" 2>"$WORK/err.txt")"
    SEND_RC=$?
    SEND_ERR="$(cat "$WORK/err.txt" 2>/dev/null)"
    return 0
}

# S1 — exactly one live row declares the handle.
case_one_declaring_row_resolves() {
    setup_rows "ws-live" || { echo "  setup: could not join both rows"; return 1; }
    resolve "$PEER"
    [ "$RES_RC" -eq 0 ] || { echo "  rc $RES_RC, want 0 (out: '$RES_OUT')"; return 1; }
    [ "$RES_OUT" = "ws-live" ] || { echo "  resolved to '$RES_OUT', want ws-live"; return 1; }
    return 0
}

# S2 — the daemon answered and nobody declares it. A refusal, not a guess.
case_no_declaring_row_refuses() {
    setup_rows "ws-live" || { echo "  setup: could not join both rows"; return 1; }
    printf '%s' '{"v":1,"id":1,"kind":"res","op":"workspace.list","payload":{"workspaces":[{"workspace_id":"ws-other","agent_handle":"someone-else"}]}}' \
        > "$SOT_TEST_LIST_FIXTURE"
    resolve "$PEER"
    [ "$RES_RC" -eq 1 ] || { echo "  rc $RES_RC, want 1 (out: '$RES_OUT')"; return 1; }
    [ -z "$RES_OUT" ] || { echo "  refused but still printed '$RES_OUT'"; return 1; }
    return 0
}

# S3 — two rows declare one handle, which set_agent_handle really allows: it
# writes one row and clears no other. Refuse; never take the first.
case_two_declaring_rows_refuse_instead_of_taking_the_first() {
    setup_rows "ws-live" || { echo "  setup: could not join both rows"; return 1; }
    list_declares "ws-live" "ws-second"
    resolve "$PEER"
    [ "$RES_RC" -eq 3 ] || { echo "  rc $RES_RC, want 3 (out: '$RES_OUT')"; return 1; }
    [ -z "$RES_OUT" ] || { echo "  ambiguity was resolved by guessing '$RES_OUT'"; return 1; }
    return 0
}

# S4 — no answer at all is not evidence that no row declares it.
case_no_answer_is_not_no_row() {
    setup_rows "ws-live" || { echo "  setup: could not join both rows"; return 1; }
    : > "$SOT_TEST_LIST_FIXTURE"
    resolve "$PEER"
    [ "$RES_RC" -eq 2 ] || { echo "  rc $RES_RC, want 2 (out: '$RES_OUT')"; return 1; }
    return 0
}

# S5 — a malformed reply is rc 2, never rc 1: read as "no live row" it would
# make the watcher exit on a transport hiccup.
case_a_malformed_reply_is_not_no_row() {
    setup_rows "ws-live" || { echo "  setup: could not join both rows"; return 1; }
    printf '%s' '{"v":1,"id":1,"kind":"res","op":"workspace.list","payload":{"workspaces":"not-an-array"}}' \
        > "$SOT_TEST_LIST_FIXTURE"
    resolve "$PEER"
    [ "$RES_RC" -eq 2 ] || { echo "  rc $RES_RC, want 2 (out: '$RES_OUT')"; return 1; }
    return 0
}

# S6 — the defect itself: the registry still carries the id the join stamped,
# the session has since continued in another row, and the poke must follow the
# daemon rather than the stamp.
case_the_poke_follows_the_daemon_not_the_stamped_field() {
    setup_rows "ws-old" || { echo "  setup: could not join both rows"; return 1; }
    list_declares "ws-live"
    run_send "@$PEER" "aim at the row that exists"
    [ "$SEND_RC" -eq 0 ] || { echo "  exited $SEND_RC (out: '$SEND_OUT' err: '$SEND_ERR')"; return 1; }
    contains "$SEND_OUT" "+woken" || { echo "  receipt was '$SEND_OUT'"; return 1; }
    local typed; typed="$(cat "$SOT_TEST_INPUT_LOG" 2>/dev/null)"
    [ "$typed" = "ws-live" ] \
        || { echo "  the poke was typed into '$typed', want ws-live (the row the daemon names)"; return 1; }
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

# R1-R3 — the append IS the delivery: a refusal costs the poke, never the frame.
case_refusal_still_files() {   # LABEL LIST_FIXTURE EXPECTED_CLAUSE
    setup_rows "ws-live" || { echo "  setup failed"; return 1; }
    printf '%s' "$2" > "$SOT_TEST_LIST_FIXTURE"
    run_send "@$PEER" "refused-$1"
    [ "$SEND_RC" -eq 0 ] || { echo "  exited $SEND_RC ($SEND_ERR)"; return 1; }
    contains "$SEND_OUT" "filed -> @$PEER" || { echo "  NOT FILED: $SEND_OUT"; return 1; }
    contains "$SEND_OUT" "$3" || { echo "  receipt '$SEND_OUT' lacks '$3'"; return 1; }
    [ ! -s "$SOT_TEST_INPUT_LOG" ] || { echo "  typed into a row anyway"; return 1; }
    [ "$(jq -r 'select(.msg == "refused-'"$1"'") | .from' "$SOT_COMM_HOME/inbox/$PEER.jsonl")" = "$SENDER" ] \
        || { echo "  inbox has no line for refused-$1"; return 1; }
}

# R1 — the resolver's own "no live row" refusal (rc 1).
case_resolver_no_row_still_files() {
    case_refusal_still_files "no-row" \
        '{"v":1,"id":1,"kind":"res","op":"workspace.list","payload":{"workspaces":[]}}' \
        "no live row declares"
}

# R2 — two rows declaring one handle refuse the poke, not the frame (rc 3).
case_resolver_two_rows_still_files() {
    list_declares "ws-live" "ws-second"
    case_refusal_still_files "two-rows" "$(cat "$SOT_TEST_LIST_FIXTURE")" "two or more rows declare"
}

# R3 — no usable answer at all (rc 2).
case_resolver_no_answer_still_files() {
    case_refusal_still_files "no-answer" "" "the daemon did not answer"
}

check "one live row declaring the handle resolves to it" case_one_declaring_row_resolves
check "no live row declaring the handle refuses instead of guessing" case_no_declaring_row_refuses
check "two rows declaring one handle refuse, never the first" case_two_declaring_rows_refuse_instead_of_taking_the_first
check "an unanswered workspace.list is not evidence of no row" case_no_answer_is_not_no_row
check "a malformed workspace.list is not evidence of no row" case_a_malformed_reply_is_not_no_row
check "the poke follows the daemon, not the workspace_id stamped at join" case_the_poke_follows_the_daemon_not_the_stamped_field
check "the daemon's refusal for a row it does not have reads as gone, not busy" case_a_row_the_daemon_does_not_have_reads_as_gone
check "a resolver's 'no live row' refusal still files the frame" case_resolver_no_row_still_files
check "a resolver's 'two or more rows' refusal still files the frame" case_resolver_two_rows_still_files
check "an unanswered resolver still files the frame" case_resolver_no_answer_still_files

echo "---"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
