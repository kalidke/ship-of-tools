#!/usr/bin/env bash
# test-codex-watch-capsule-delivery.sh — pins `_codex_watch_capsule_inject`'s
# return-code contract against fixture `pty.input` replies: 0 ADVANCE
# (delivered, unconfirmed, or permanently refused -- never retyped), 1
# RETRY (text never recorded), 2 GONE. Stubs the one daemon call, no tmux.
#
# Usage: comm/core/tests/test-codex-watch-capsule-delivery.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"
# shellcheck source=../scripts/codex-watch.sh
source "$SCRIPTS_DIR/codex-watch.sh"

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

RESP_SENT='{"v":1,"id":1,"kind":"res","op":"pty.input","payload":{"ok":true,"runtime":"capsule","bytes":21,"enter_sent":true}}'
RESP_NOT_SENT='{"v":1,"id":1,"kind":"res","op":"pty.input","payload":{"ok":true,"runtime":"capsule","bytes":21,"enter_sent":false}}'
# Pre-record refusal by CODE: text never recorded -- retry.
RESP_NOT_READY='{"v":1,"id":1,"kind":"res","op":"pty.input","payload":{"error":"capsule row not ready (phase: booting)","code":"capsule_not_ready","phase":"booting"}}'
# Pre-record refusal by PHASE (attach): also retry.
RESP_ATTACH_FAILED='{"v":1,"id":1,"kind":"res","op":"pty.input","payload":{"error":"could not attach","code":"capsule_input_failed","phase":"attach","submitted":false}}'
# A stale Enter refusal: phase=="input" retries regardless of `submitted:true` (never read).
RESP_STALE='{"v":1,"id":1,"kind":"res","op":"pty.input","payload":{"error":"input refused as stale","code":"capsule_input_failed","phase":"input","submitted":true}}'
# Size refusal: text never recorded either, but PERMANENT -- advance.
RESP_SIZE='{"v":1,"id":1,"kind":"res","op":"pty.input","payload":{"error":"payload is 9000 bytes, exceeding the take queue cap of 8192 bytes","code":"capsule_input_failed","phase":"size","submitted":false}}'
# phase=="record": the record's own verdict is unknowable -- NOT in the
# retry whitelist, so this advances (delivered, unconfirmed).
RESP_UNKNOWN='{"v":1,"id":1,"kind":"res","op":"pty.input","payload":{"error":"input delivery unknown","code":"capsule_input_unknown","phase":"record","submitted":true}}'
RESP_GONE='{"v":1,"id":1,"kind":"res","op":"pty.input","payload":{"error":"unknown workspace: ws-gone","code":"unknown_workspace"}}'

# Stub the only daemon call. FILE-based counter (a plain variable will
# not survive the command-substitution subshell) proves "never retypes".
export SOT_WORKSPACE_ID="ws-test"
COUNT_FILE="$(mktemp)"
trap 'rm -f "$COUNT_FILE"' EXIT
_codex_watch_pty_input() { printf 'x' >> "$COUNT_FILE"; printf '%s' "$STUB_RESP"; }
reset_call_count() { : > "$COUNT_FILE"; }
call_count() { wc -c < "$COUNT_FILE" 2>/dev/null | tr -d ' '; }

inject_delivered_returns_zero_and_warns_nothing() {
    STUB_RESP="$RESP_SENT"; reset_call_count
    local out rc
    out="$(_codex_watch_capsule_inject "peer" "hello" 2>&1)"
    rc=$?
    [ "$rc" -eq 0 ] && [ -z "$out" ] && [ "$(call_count)" -eq 1 ]
}

inject_enter_not_sent_still_returns_zero_but_warns() {
    STUB_RESP="$RESP_NOT_SENT"; reset_call_count
    local out rc
    out="$(_codex_watch_capsule_inject "peer" "hello" 2>&1)"
    rc=$?
    [ "$rc" -eq 0 ] || return 1
    [ "$(call_count)" -eq 1 ] || return 1
    case "$out" in *"enter not sent"*"peer"*) return 0 ;; esac
    return 1
}

inject_not_ready_returns_one_for_retry_next_cycle() {
    STUB_RESP="$RESP_NOT_READY"; reset_call_count
    local rc
    _codex_watch_capsule_inject "peer" "hello" >/dev/null 2>&1
    rc=$?
    [ "$rc" -eq 1 ] && [ "$(call_count)" -eq 1 ]
}

inject_stale_enter_refusal_retries_despite_submitted_true() {
    STUB_RESP="$RESP_STALE"; reset_call_count
    local rc
    _codex_watch_capsule_inject "peer" "hello" >/dev/null 2>&1
    rc=$?
    [ "$rc" -eq 1 ] && [ "$(call_count)" -eq 1 ]
}

inject_no_reply_advances_and_never_retypes() {
    STUB_RESP=""; reset_call_count
    local out rc
    out="$(_codex_watch_capsule_inject "peer" "hello" 2>&1)"
    rc=$?
    [ "$rc" -eq 0 ] || return 1
    [ "$(call_count)" -eq 1 ] || return 1
    case "$out" in *"unconfirmed"*"peer"*) return 0 ;; esac
    return 1
}

inject_size_refusal_advances_permanently() {
    STUB_RESP="$RESP_SIZE"; reset_call_count
    local out rc
    out="$(_codex_watch_capsule_inject "peer" "hello" 2>&1)"
    rc=$?
    [ "$rc" -eq 0 ] || return 1
    [ "$(call_count)" -eq 1 ] || return 1
    case "$out" in *"too large"*"peer"*) return 0 ;; esac
    return 1
}

inject_unknown_outcome_advances_and_never_retypes() {
    STUB_RESP="$RESP_UNKNOWN"; reset_call_count
    local out rc
    out="$(_codex_watch_capsule_inject "peer" "hello" 2>&1)"
    rc=$?
    [ "$rc" -eq 0 ] || return 1
    [ "$(call_count)" -eq 1 ] || return 1
    case "$out" in *"unconfirmed"*"peer"*) return 0 ;; esac
    return 1
}

inject_gone_returns_two_and_never_exits_itself() {
    STUB_RESP="$RESP_GONE"; reset_call_count
    local out rc
    out="$(_codex_watch_capsule_inject "peer" "hello" 2>&1)"
    rc=$?
    # Reaching this line proves it did not call `exit` itself.
    [ "$rc" -eq 2 ] && [ "$(call_count)" -eq 1 ] && case "$out" in *"is gone"*) return 0 ;; esac
    return 1
}

check "inject returns 0 and warns nothing once delivered with enter sent" inject_delivered_returns_zero_and_warns_nothing
check "inject returns 0 but warns once when enter itself is unconfirmed" inject_enter_not_sent_still_returns_zero_but_warns
check "inject returns 1 (retry next cycle) on a pre-record (not-ready) refusal" inject_not_ready_returns_one_for_retry_next_cycle
check "inject retries a stale Enter refusal despite submitted:true" inject_stale_enter_refusal_retries_despite_submitted_true
check "an unknown outcome (no reply) advances and never retypes" inject_no_reply_advances_and_never_retypes
check "a size refusal advances permanently and never retypes" inject_size_refusal_advances_permanently
check "an unknown outcome (submitted:true) advances and never retypes" inject_unknown_outcome_advances_and_never_retypes
check "inject returns 2 for a gone row and never calls exit itself" inject_gone_returns_two_and_never_exits_itself

echo "---"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
