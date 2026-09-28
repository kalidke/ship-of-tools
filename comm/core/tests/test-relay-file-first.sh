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
#   3. A cross-box verdict is the FILER'S RECEIPT and nothing else (ADR 0048):
#      `filed -> @h (by <filer>, relay)` exit 0 when a receipt carrying this
#      sender's own frame id arrives. A receipt is ONLY ever positive -- no
#      filer can honestly say "not me", since it cannot know about the others
#      and every attached frontend would say it about a handle it does not
#      host -- so silence is the negative: NOT CONFIRMED, with the attached
#      roster as a diagnostic. An ack with no `id` says the hub predates
#      receipts, and an empty roster is `no such handle`, both decided on the
#      ack. The name-suffix GUESS this replaces (a target whose name ends in
#      an attached `fe@<host>`) is gone from the script; a case below is the
#      regression guard that the suffix now means nothing at all.
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
# A one-shot fake daemon that READS the request before it answers, so the
# receipt it returns carries the sender's own minted `id` -- the thing the
# verdict is attributed by (ADR 0048). Two processes over one socket: `nc`
# writes whatever appears on the reply FIFO and dumps the request to a file;
# the handler waits for the `agent.send` line, extracts its id, and writes the
# canned lines. The redirection ORDER matters (nc opens the fifo for reading,
# the handler for writing): each unblocks the other, and reversing either one
# deadlocks on the open.
#
# ACK_ID_MODE: echo (a receipt-capable hub), drop (a hub that predates them),
# or wrong (a receipt for somebody else's frame -- the attributability guard).
# A receipt is positive-only, so the only parameter left is WHO filed.
FAKE_PID=""; HANDLER_PID=""
fake_daemon() {  # SOCKET RECEIVERS_JSON [RECEIPT_JSON_TEMPLATE] [ACK_ID_MODE]
    command -v nc >/dev/null 2>&1 || return 1
    local sock="$1" recv="$2" rcpt="${3:-}" mode="${4:-echo}"
    local fifo="$WORK/reply.fifo" req="$WORK/req.txt"
    rm -f "$fifo" "$req"; mkfifo "$fifo" || return 1
    : > "$req"
    # `-N` (shutdown the socket on stdin EOF) is what makes the no-receipt
    # case end on EOF instead of sitting out the sender's whole window: this
    # netcat flavor does NOT close on EOF without it. A flavor that lacks the
    # flag fails to bind, and the case SKIPs rather than hanging.
    ( nc -N -lU "$sock" < "$fifo" > "$req" 2>/dev/null ) &
    FAKE_PID=$!
    (
        exec > "$fifo"
        local tries=0 id=""
        while [ "$tries" -lt 100 ]; do
            id="$(grep -h '"op":"agent.send"' "$req" 2>/dev/null | head -1 \
                  | jq -r '.payload.id // ""' 2>/dev/null || true)"
            [ -n "$id" ] && break
            sleep 0.05; tries=$((tries + 1))
        done
        local ack_id="$id"
        case "$mode" in
            drop)  ack_id="" ;;
            wrong) ack_id="$id"; id="$id-not-yours" ;;
        esac
        if [ -n "$ack_id" ]; then
            jq -nc --argjson r "$recv" --arg i "$ack_id" \
                '{v:1,id:1,kind:"resp",op:"agent.send",payload:{ok:true,receivers:$r,id:$i}}'
        else
            jq -nc --argjson r "$recv" \
                '{v:1,id:1,kind:"resp",op:"agent.send",payload:{ok:true,receivers:$r}}'
        fi
        if [ -n "$rcpt" ]; then
            printf '%s\n' "$rcpt" | jq -c --arg i "$id" '.payload.id = $i'
        fi
    ) &
    HANDLER_PID=$!
    local tries=0
    while [ "$tries" -lt 50 ]; do
        [ -S "$sock" ] && return 0
        sleep 0.1; tries=$((tries + 1))
    done
    fake_daemon_stop
    return 1
}
fake_daemon_stop() {
    [ -n "$FAKE_PID" ] && kill "$FAKE_PID" 2>/dev/null
    [ -n "$HANDLER_PID" ] && kill "$HANDLER_PID" 2>/dev/null
    FAKE_PID=""; HANDLER_PID=""
    rm -f "$WORK/reply.fifo"
    return 0
}

# One `agent.receipt` evt line -- id filled in by the handler above, filer
# stamped by the daemon in production. Two fields, no negative form.
receipt_evt() {  # FILER
    jq -nc --arg who "$1" \
        '{v:1,id:1,kind:"evt",op:"agent.receipt",payload:{id:"", filer:$who}}'
}

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

case_a_receipt_is_the_only_delivery() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local sock="$WORK/fake-receipt.sock"
    fake_daemon "$sock" '["fe@'"$PEER_HOST"'"]' \
        "$(receipt_evt "fe@$PEER_HOST")" \
        || { echo "  nc -lU unavailable; cannot stand up a fake daemon"; return 2; }
    relay_send "unix:$sock" send "@peer-$PEER_HOST" "over the wire"
    fake_daemon_stop
    [ "$RELAY_RC" -eq 0 ] || { echo "  exited $RELAY_RC, want 0 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_OUT" "filed -> @peer-$PEER_HOST (by fe@$PEER_HOST" \
        || { echo "  verdict was '$RELAY_OUT', want a filed line naming the filer"; return 1; }
    return 0
}

case_an_unanswered_send_is_not_confirmed_and_names_who_was_attached() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local sock="$WORK/fake-silent.sock"
    # Ack only, no receipt: an rc9-era frontend appends the frame and says
    # nothing about it. The roster survives HERE and only here, as a
    # diagnostic naming who was attached and did not answer.
    fake_daemon "$sock" '["fe@'"$PEER_HOST"'"]' \
        || { echo "  nc -lU unavailable; cannot stand up a fake daemon"; return 2; }
    relay_send "unix:$sock" send "@peer-$PEER_HOST" "over the wire"
    fake_daemon_stop
    [ "$RELAY_RC" -eq 1 ] || { echo "  exited $RELAY_RC, want 1 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_ERR" "NOT CONFIRMED" \
        || { echo "  stderr was '$RELAY_ERR', want NOT CONFIRMED"; return 1; }
    contains "$RELAY_ERR" "Attached: fe@$PEER_HOST" \
        || { echo "  the diagnostic does not name who was attached: '$RELAY_ERR'"; return 1; }
    # The exact strings the guess printed, swept where they were printed
    # FROM (a grep for text that only ever lived in a comment can only pass).
    contains "$RELAY_ERR" "may file it" \
        && { echo "  the old guess text survived: '$RELAY_ERR'"; return 1; }
    contains "$RELAY_ERR" "No confirmed path to a session on another frontend" \
        && { echo "  the old dead-end text survived: '$RELAY_ERR'"; return 1; }
    contains "$RELAY_OUT" "filed" \
        && { echo "  an unproven send still printed a filed line: '$RELAY_OUT'"; return 1; }
    contains "$RELAY_OUT" "relayed" \
        && { echo "  an unproven send still printed a relayed line: '$RELAY_OUT'"; return 1; }
    return 0
}

case_the_name_suffix_means_nothing_now() {
    # The regression guard for the deleted GUESS: `$TARGET` ends in the
    # attached frontend's own host, which used to be reported as a delivery
    # (and matched a misspelling identically). With no receipt it gets exactly
    # the same NOT CONFIRMED as any other unanswered send -- the suffix is not
    # read anywhere.
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local sock="$WORK/fake-suffix.sock"
    fake_daemon "$sock" '["fe@'"$PEER_HOST"'"]' \
        || { echo "  nc -lU unavailable; cannot stand up a fake daemon"; return 2; }
    relay_send "unix:$sock" send "@peeeer-$PEER_HOST" "over the wire"
    fake_daemon_stop
    [ "$RELAY_RC" -eq 1 ] || { echo "  exited $RELAY_RC, want 1 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_ERR" "NOT CONFIRMED" \
        || { echo "  stderr was '$RELAY_ERR', want NOT CONFIRMED"; return 1; }
    contains "$RELAY_ERR" "may file it" \
        && { echo "  the old guess text survived: '$RELAY_ERR'"; return 1; }
    return 0
}

case_a_receipt_for_another_frame_is_not_a_verdict() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local sock="$WORK/fake-wrongid.sock"
    # Attributability (ADR 0048): a receipt whose id is not this sender's own
    # must not be read as its verdict, or two concurrent sends to one handle
    # can swap them and one success vouches for a failure.
    fake_daemon "$sock" '["fe@'"$PEER_HOST"'"]' \
        "$(receipt_evt "fe@$PEER_HOST")" wrong \
        || { echo "  nc -lU unavailable; cannot stand up a fake daemon"; return 2; }
    relay_send "unix:$sock" send "@peer-$PEER_HOST" "over the wire"
    fake_daemon_stop
    [ "$RELAY_RC" -eq 1 ] || { echo "  exited $RELAY_RC, want 1 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_ERR" "NOT CONFIRMED" \
        || { echo "  stderr was '$RELAY_ERR', want NOT CONFIRMED"; return 1; }
    contains "$RELAY_OUT" "filed" \
        && { echo "  another frame's receipt was read as a delivery: '$RELAY_OUT'"; return 1; }
    return 0
}

case_a_hub_with_no_id_says_so_instead_of_waiting() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local sock="$WORK/fake-oldhub.sock"
    # An older daemon drops the `id` from its ack: no receipt can ever be
    # attributed, and the sender says that on the ACK rather than spending
    # five seconds waiting for an answer nobody can give.
    fake_daemon "$sock" '["fe@'"$PEER_HOST"'"]' "" drop \
        || { echo "  nc -lU unavailable; cannot stand up a fake daemon"; return 2; }
    relay_send "unix:$sock" send "@peer-$PEER_HOST" "over the wire"
    fake_daemon_stop
    [ "$RELAY_RC" -eq 1 ] || { echo "  exited $RELAY_RC, want 1 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_ERR" "predates filer receipts" \
        || { echo "  stderr was '$RELAY_ERR', want the predates-receipts verdict"; return 1; }
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

case_an_empty_roster_is_no_such_handle() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local sock="$WORK/fake-empty.sock"
    # Nobody at all is attached to the hub: there is nowhere for the frame to
    # land and nothing that could ever receipt it. Decided on the ACK -- this
    # case must not spend the five-second receipt window.
    fake_daemon "$sock" '[]' \
        || { echo "  nc -lU unavailable; cannot stand up a fake daemon"; return 2; }
    relay_send "unix:$sock" send "@peer-$PEER_HOST" "into the void"
    fake_daemon_stop
    [ "$RELAY_RC" -eq 1 ] || { echo "  exited $RELAY_RC, want 1 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_ERR" "no such handle: peer-$PEER_HOST" \
        || { echo "  stderr was '$RELAY_ERR', want 'no such handle'"; return 1; }
    contains "$RELAY_ERR" "NOT CONFIRMED" \
        && { echo "  an empty roster waited for a receipt: '$RELAY_ERR'"; return 1; }
    return 0
}

check "a registry target is filed with the daemon down" case_registry_target_is_filed_with_the_daemon_down
check "a filer's receipt is the delivery, and names the filer" case_a_receipt_is_the_only_delivery
check "an ack with no receipt is NOT CONFIRMED and names who was attached" case_an_unanswered_send_is_not_confirmed_and_names_who_was_attached
check "a target whose name ends in an attached frontend's host gets no credit for it" case_the_name_suffix_means_nothing_now
check "a receipt carrying another frame's id is not this send's verdict" case_a_receipt_for_another_frame_is_not_a_verdict
check "a hub whose ack drops the id says it predates receipts" case_a_hub_with_no_id_says_so_instead_of_waiting
check "an ack with an empty receivers list is 'no such handle', decided on the ack" case_an_empty_roster_is_no_such_handle
check "a working recipient is annotated with its stamped age and turn-boundary wording" case_annotation_working_recipient
check "a recipient stopped on an open question is annotated 'needs its own user', quoted and truncated" case_annotation_recipient_needs_its_own_user
check "a stale heartbeat overrides a fresh 'working' stamp" case_annotation_stale_heartbeat_overrides_working
check "a row missing the annotation fields entirely sends fine with no annotation" case_annotation_absent_for_a_row_missing_the_fields
check "a malformed row (wrong types, bad timestamps) sends fine with no annotation" case_annotation_absent_for_a_malformed_row

echo "---"
echo "PASS=$PASS FAIL=$FAIL SKIP=$SKIP"
[ "$FAIL" -eq 0 ]
