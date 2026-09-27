#!/usr/bin/env bash
# test-relay-file-first.sh — the relay's ack means the frame is FILED (messaging
# ruling, 2026-09-26). Three facts:
#
#   1. A target this box's registry names never touches the wire. comm-relay.sh
#      hands it to comm-send.sh, which appends to that handle's inbox under the
#      shared $SOT_COMM_HOME — so it works with the daemon DOWN, prints
#      `filed -> @name`, and exits 0. `relayed` never meant this: it reported
#      the daemon's own success, which is why callers were told "only a reply
#      proves the path" — a design defect pushed onto every sender.
#   2. A handle nothing can file for is a FAILURE: `no such handle` on stderr,
#      exit 1. It used to print `relayed` plus a warning and exit 0.
#   3. The one cross-box GUESS left -- a Windows-hosted handle runs no bridge, so
#      its frontend files for it as `fe@<host>` and a target whose name ENDS in
#      that host is probably its own -- is a FAILURE too: `NOT CONFIRMED` on
#      stderr, exit 1. It used to print `filed via the frontend (unconfirmed)`
#      and exit 0, which is a false success: a misspelled handle ending in the
#      same host matched identically and reported the same delivery. An unproven
#      send does not get to report one until the filer returns a receipt.
#
# No bats dependency. HERMETIC, same seams as test-leave-stops-bridge.sh: a
# temp $SOT_COMM_HOME, a per-case $SOT_COMM_SELF_FILE, a pinned
# $SOT_COMM_TEST_HOST, and where a daemon is needed a canned one-shot fake over
# a unix socket — never the real ~/.sot-comm and never the real daemon.
#
# Usage: comm/core/tests/test-relay-file-first.sh
# Exit: 0 if every case PASSes or SKIPs cleanly, 1 if any FAILs.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"
JOIN="$SCRIPTS_DIR/comm-join.sh"
RELAY="$SCRIPTS_DIR/comm-relay.sh"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-comm-relay-file-first-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
export SOT_COMM_HOME="$WORK/home"
mkdir -p "$SOT_COMM_HOME"
trap 'rm -rf "$WORK"' EXIT

SENDER_HOST="testhost"
PEER_HOST="otherbox"
SENDER="t-sender"
TARGET="t-target-$PEER_HOST"
SELF_SENDER="$WORK/self-sender.txt"
SELF_TARGET="$WORK/self-target.txt"

PASS=0; FAIL=0; SKIP=0
check() {
    local desc="$1" fn="$2" rc
    "$fn"; rc=$?
    case "$rc" in
        0) echo "PASS: $desc"; PASS=$((PASS + 1)) ;;
        2) echo "SKIP: $desc"; SKIP=$((SKIP + 1)) ;;
        *) echo "FAIL: $desc"; FAIL=$((FAIL + 1)) ;;
    esac
}
contains() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }

# Two real rows in one temp registry, joined through the real join path: the
# target lives on ANOTHER host, so the sender's poke is skipped by host and no
# pty.input is ever attempted (this suite must not reach a real daemon).
setup_rows() {
    ( cd "$WORK" && SOT_COMM_SELF_FILE="$SELF_TARGET" SOT_COMM_TEST_HOST="$PEER_HOST" \
        "$JOIN" --name "$TARGET" ) >/dev/null 2>&1 || return 1
    ( cd "$WORK" && SOT_COMM_SELF_FILE="$SELF_SENDER" SOT_COMM_TEST_HOST="$SENDER_HOST" \
        "$JOIN" --name "$SENDER" ) >/dev/null 2>&1 || return 1
    jq -e --arg a "$SENDER" --arg b "$TARGET" '.agents | has($a) and has($b)' \
        "$SOT_COMM_HOME/registry.json" >/dev/null 2>&1
}

# relay_send ENDPOINT ARGS... -> run comm-relay.sh as the sender handle.
# stdout and stderr are captured separately (the verdict is stdout, the refusal
# is stderr) and the exit code is returned.
RELAY_OUT=""; RELAY_ERR=""; RELAY_RC=0
relay_send() {
    local ep="$1"; shift
    RELAY_OUT="$(cd "$WORK" && SOT_COMM_SELF_FILE="$SELF_SENDER" SOT_COMM_TEST_HOST="$SENDER_HOST" \
        SOT_RELAY_ENDPOINT="$ep" "$RELAY" "$@" 2>"$WORK/err.txt")"
    RELAY_RC=$?
    RELAY_ERR="$(cat "$WORK/err.txt" 2>/dev/null)"
    return 0
}

# A one-shot fake daemon: answers the first connection with a canned
# `agent.send` response naming RECEIVERS, then exits. This is what makes the
# wire cases deterministic without a real sotd.
FAKE_PID=""
fake_daemon() {  # SOCKET RECEIVERS_JSON
    command -v nc >/dev/null 2>&1 || return 1
    local sock="$1" recv="$2"
    printf '%s\n' "{\"v\":1,\"id\":1,\"kind\":\"resp\",\"op\":\"agent.send\",\"payload\":{\"ok\":true,\"receivers\":$recv}}" \
        > "$WORK/reply.txt"
    nc -lU "$sock" < "$WORK/reply.txt" > /dev/null 2>&1 &
    FAKE_PID=$!
    local tries=0
    while [ "$tries" -lt 50 ]; do
        [ -S "$sock" ] && return 0
        sleep 0.1; tries=$((tries + 1))
    done
    kill "$FAKE_PID" 2>/dev/null; FAKE_PID=""
    return 1
}
fake_daemon_stop() { [ -n "$FAKE_PID" ] && kill "$FAKE_PID" 2>/dev/null; FAKE_PID=""; }

# _patch_target_row JSON -- merge JSON into TARGET's own registry row (the
# entry a sender's `send @target` reads to resolve the handle, and now also
# to annotate it -- messaging ruling, 2026-09-26).
_patch_target_row() {
    jq --arg t "$TARGET" --argjson f "$1" '.agents[$t] += $f' "$SOT_COMM_HOME/registry.json" \
        > "$SOT_COMM_HOME/registry.json.tmp" && mv "$SOT_COMM_HOME/registry.json.tmp" "$SOT_COMM_HOME/registry.json"
}

case_registry_target_is_filed_with_the_daemon_down() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local inbox="$SOT_COMM_HOME/inbox/$TARGET.jsonl"
    : > "$inbox"
    # Nothing listens on this socket: the point of the case. A registry target
    # needs no daemon, because its inbox is a file this box can append to.
    relay_send "unix:$WORK/no-such-daemon.sock" send "@$TARGET" "hello from the file-first path"
    [ "$RELAY_RC" -eq 0 ] || { echo "  exited $RELAY_RC, want 0 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_OUT" "filed -> @$TARGET" \
        || { echo "  verdict was '$RELAY_OUT', want 'filed -> @$TARGET'"; return 1; }
    contains "$RELAY_OUT" "relayed" \
        && { echo "  the verdict still says 'relayed': '$RELAY_OUT'"; return 1; }
    local n; n="$(grep -c 'file-first path' "$inbox" 2>/dev/null || echo 0)"
    [ "$n" -eq 1 ] || { echo "  the target's inbox holds $n copies of the message, want exactly 1"; return 1; }
    # The `to` stamp is what keeps a directed frame distinguishable from a
    # broadcast for the recipient's Stop hook and its ping watcher.
    jq -e --arg t "$TARGET" 'select(.to == $t)' "$inbox" >/dev/null 2>&1 \
        || { echo "  the filed line is not stamped to @$TARGET"; return 1; }
    return 0
}

case_unknown_handle_exits_one_while_an_fe_row_is_attached() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local sock="$WORK/fake-unknown.sock"
    fake_daemon "$sock" '["fe@'"$PEER_HOST"'"]' \
        || { echo "  nc -lU unavailable; cannot stand up a fake daemon"; return 2; }
    # `stranger` is in no registry and its name does not end in -otherbox, so
    # the attached frontend is NOT its filer: nothing can file for it.
    relay_send "unix:$sock" send "@stranger" "into the void"
    fake_daemon_stop
    [ "$RELAY_RC" -eq 1 ] || { echo "  exited $RELAY_RC, want 1 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_ERR" "no such handle: stranger" \
        || { echo "  stderr was '$RELAY_ERR', want 'no such handle: stranger'"; return 1; }
    contains "$RELAY_OUT" "relayed" \
        && { echo "  a failed send still printed a relayed line: '$RELAY_OUT'"; return 1; }
    return 0
}

case_a_frontend_filer_exits_one_and_claims_nothing() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local sock="$WORK/fake-fe.sock"
    fake_daemon "$sock" '["fe@'"$PEER_HOST"'"]' \
        || { echo "  nc -lU unavailable; cannot stand up a fake daemon"; return 2; }
    # A handle on the frontend's own box (its name ends in -<host>) with no
    # bridge of its own: that frontend PROBABLY files for it. Probably is not a
    # delivery -- nothing here proves it landed, and `peer-$PEER_HOST` and a
    # typo'd `peeeer-$PEER_HOST` match this branch identically -- so it FAILS.
    relay_send "unix:$sock" send "@peer-$PEER_HOST" "over the wire"
    fake_daemon_stop
    [ "$RELAY_RC" -eq 1 ] || { echo "  exited $RELAY_RC, want 1 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_ERR" "NOT CONFIRMED" \
        || { echo "  stderr was '$RELAY_ERR', want a NOT CONFIRMED verdict"; return 1; }
    contains "$RELAY_ERR" "$PEER_HOST" \
        || { echo "  stderr does not name the frontend that might file it: '$RELAY_ERR'"; return 1; }
    # The point of the change: no line anywhere may read as a delivery.
    contains "$RELAY_OUT" "filed" \
        && { echo "  an unproven send still printed a filed line: '$RELAY_OUT'"; return 1; }
    contains "$RELAY_OUT" "relayed" \
        && { echo "  an unproven send still printed a relayed line: '$RELAY_OUT'"; return 1; }
    return 0
}

# ---- recipient annotation (messaging ruling, 2026-09-26): one factual
# clause about the TARGET, read off the same registry entry that resolved
# the handle -- comm-lib.sh's sot_recipient_note. ----
case_annotation_working_recipient() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local now t3m
    now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    t3m="$(date -u -d '3 minutes ago' +%Y-%m-%dT%H:%M:%SZ)"
    _patch_target_row "$(jq -nc --arg st "$t3m" --arg ls "$now" \
        '{state:"working", summary:"n", status_at:$st, last_seen:$ls}')"
    relay_send "unix:$WORK/no-such-daemon.sock" send "@$TARGET" "ping"
    [ "$RELAY_RC" -eq 0 ] || { echo "  exited $RELAY_RC (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_OUT" "working, stamped 3m ago -- reply expected at its turn boundary" \
        || { echo "  verdict was '$RELAY_OUT'"; return 1; }
    return 0
}
case_annotation_recipient_needs_its_own_user() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local now t6m longq
    now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    t6m="$(date -u -d '6 minutes ago' +%Y-%m-%dT%H:%M:%SZ)"
    # 87 chars -- past the 60-char truncation, so the full text must not survive.
    longq="which port do you want, the long question that goes past sixty characters for sure?"
    _patch_target_row "$(jq -nc --arg st "$t6m" --arg ls "$now" --arg q "$longq" \
        '{state:"blocked", summary:$q, status_at:$st, last_seen:$ls}')"
    relay_send "unix:$WORK/no-such-daemon.sock" send "@$TARGET" "ping"
    [ "$RELAY_RC" -eq 0 ] || { echo "  exited $RELAY_RC (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_OUT" 'needs its own user, stamped 6m ago: "' \
        || { echo "  verdict was '$RELAY_OUT'"; return 1; }
    contains "$RELAY_OUT" "$(printf '%s' "$longq" | cut -c1-60)" \
        || { echo "  the truncated question is missing: '$RELAY_OUT'"; return 1; }
    contains "$RELAY_OUT" "$longq" \
        && { echo "  the FULL (untruncated) question leaked: '$RELAY_OUT'"; return 1; }
    return 0
}
case_annotation_stale_heartbeat_overrides_working() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local recent stale20m
    recent="$(date -u -d '1 minute ago' +%Y-%m-%dT%H:%M:%SZ)"
    stale20m="$(date -u -d '20 minutes ago' +%Y-%m-%dT%H:%M:%SZ)"
    # state says "working" and its own stamp is fresh -- but the HEARTBEAT
    # (last_seen) is 20 minutes stale, past SOT_COMM_STALE_SECS (default
    # 600s): a stamp from a dead session is the misleading one, so this
    # overrides the working clause rather than sitting beside it.
    _patch_target_row "$(jq -nc --arg st "$recent" --arg ls "$stale20m" \
        '{state:"working", summary:"n", status_at:$st, last_seen:$ls}')"
    relay_send "unix:$WORK/no-such-daemon.sock" send "@$TARGET" "ping"
    [ "$RELAY_RC" -eq 0 ] || { echo "  exited $RELAY_RC (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_OUT" "no heartbeat for 20m -- may be gone" \
        || { echo "  verdict was '$RELAY_OUT'"; return 1; }
    contains "$RELAY_OUT" "working, stamped" \
        && { echo "  the working clause survived the stale override: '$RELAY_OUT'"; return 1; }
    return 0
}
case_annotation_absent_for_a_row_missing_the_fields() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    # A freshly-joined row (setup_rows itself) has no state/summary/status_at
    # at all (comm-join.sh:117-122) -- exactly the missing-fields case, no
    # patch needed. The send still succeeds; there is simply nothing to say.
    relay_send "unix:$WORK/no-such-daemon.sock" send "@$TARGET" "ping"
    [ "$RELAY_RC" -eq 0 ] || { echo "  exited $RELAY_RC (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_OUT" "filed -> @$TARGET" || { echo "  verdict was '$RELAY_OUT'"; return 1; }
    contains "$RELAY_OUT" "stamped" && { echo "  a guessed annotation appeared: '$RELAY_OUT'"; return 1; }
    contains "$RELAY_OUT" "no heartbeat" && { echo "  a guessed annotation appeared: '$RELAY_OUT'"; return 1; }
    return 0
}
case_annotation_absent_for_a_malformed_row() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    # `state` is a number, both timestamps are unparseable -- the exact
    # 'silence beats a confident wrong summary' case.
    _patch_target_row '{"state":123,"summary":"n","status_at":"not-a-date","last_seen":"also-not-a-date"}'
    relay_send "unix:$WORK/no-such-daemon.sock" send "@$TARGET" "ping"
    [ "$RELAY_RC" -eq 0 ] || { echo "  exited $RELAY_RC (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_OUT" "filed -> @$TARGET" || { echo "  verdict was '$RELAY_OUT'"; return 1; }
    contains "$RELAY_OUT" "stamped" && { echo "  a guessed annotation appeared: '$RELAY_OUT'"; return 1; }
    contains "$RELAY_OUT" "no heartbeat" && { echo "  a guessed annotation appeared: '$RELAY_OUT'"; return 1; }
    return 0
}

check "a registry target is filed with the daemon down" case_registry_target_is_filed_with_the_daemon_down
check "an unknown handle exits 1 with 'no such handle' while an fe@ row is attached" case_unknown_handle_exits_one_while_an_fe_row_is_attached
check "a frontend filer exits 1 and claims no delivery" case_a_frontend_filer_exits_one_and_claims_nothing
check "a working recipient is annotated with its stamped age and turn-boundary wording" case_annotation_working_recipient
check "a recipient stopped on an open question is annotated 'needs its own user', quoted and truncated" case_annotation_recipient_needs_its_own_user
check "a stale heartbeat overrides a fresh 'working' stamp" case_annotation_stale_heartbeat_overrides_working
check "a row missing the annotation fields entirely sends fine with no annotation" case_annotation_absent_for_a_row_missing_the_fields
check "a malformed row (wrong types, bad timestamps) sends fine with no annotation" case_annotation_absent_for_a_malformed_row

echo "---"
echo "PASS=$PASS FAIL=$FAIL SKIP=$SKIP"
[ "$FAIL" -eq 0 ]
