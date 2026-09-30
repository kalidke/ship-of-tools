#!/usr/bin/env bash
# test-send-routes-to-relay.sh — the two comm verbs route to each other, so a
# session never has to know which one reaches a peer (ADR 0048).
#
#   1. Registry MISS on a directed `comm-send.sh @h "msg"`: this box cannot
#      name @h, which is the ROUTINE case for a session on a machine that
#      shares no $HOME (it can never have a row here). It execs
#      `comm-relay.sh send @h "msg"` instead of refusing "no such handle" —
#      the refusal that forced the caller to pick the routing verb itself.
#   2. Registry HIT still files locally and never touches the relay. The two
#      triggers are mutually exclusive (hit -> file, miss -> wire), which is
#      why the pair needs no recursion guard.
#   3. A --broadcast never execs: it fans out over registry keys, and an exec
#      mid fan-out would abandon every remaining target.
#   4. The two doors ask the SAME question (`.host` empty-or-absent), so a row
#      that exists with no host cannot ping-pong between them. That case runs
#      the REAL relay under `timeout` -- with divergent predicates it never
#      returns, so the hang IS the assertion.
#
# No bats dependency. HERMETIC, same seams as test-relay-file-first.sh: a temp
# $SOT_COMM_HOME, a per-case $SOT_COMM_SELF_FILE, a pinned $SOT_COMM_TEST_HOST,
# and a COPY of the scripts dir whose comm-relay.sh is a stub that records its
# argv — never the real ~/.sot-comm, never a real daemon, never the real relay.
#
# Usage: comm/core/tests/test-send-routes-to-relay.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-comm-send-routes-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
# 0031 B1: the record a daemon writes at startup. Without it no script
# appends locally, and every filing here would go to a daemon instead.
mkdir -p "$SOT_COMM_HOME/inbox"
# A fake findmnt first on PATH reports a local filesystem, so the record and
# every script's identity are `local <machine-id>` whatever $WORK sits on (a
# function stub would not survive: comm-lib.sh defines _sot_findmnt itself).
mkdir -p "$WORK/findmnt-bin"
printf '#!/bin/sh\necho "ext4 rw,relatime /dev/fake"\n' > "$WORK/findmnt-bin/findmnt"
chmod +x "$WORK/findmnt-bin/findmnt"
export PATH="$WORK/findmnt-bin:$PATH"
bash -c 'source "$1"; sot_inbox_lock_identity "$INBOX_DIR"' _ "$SCRIPTS_DIR/comm-lib.sh" > "$SOT_COMM_HOME/inbox-lock-manager"
mkdir -p "$SOT_COMM_HOME"
trap 'rm -rf "${WORK:?}"' EXIT

# The scripts under test, with the relay replaced by a recorder. comm-send.sh
# resolves its siblings through its OWN dir, so a copy is enough to intercept.
BIN="$WORK/bin"
cp -r "$SCRIPTS_DIR" "$BIN"
STUB_LOG="$WORK/relay-argv.txt"
cat > "$BIN/comm-relay.sh" <<STUB
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$STUB_LOG"
echo "stub relay: \$*"
STUB
chmod +x "$BIN/comm-relay.sh"
SEND="$BIN/comm-send.sh"
JOIN="$BIN/comm-join.sh"

# A second copy with the REAL relay in place, for the ping-pong case: a stub
# that records argv can never execute the loop it is meant to rule out.
BIN_REAL="$WORK/bin-real"
cp -r "$SCRIPTS_DIR" "$BIN_REAL"
SEND_REAL="$BIN_REAL/comm-send.sh"

SENDER_HOST="testhost"
SENDER="t-sender"
LOCAL_PEER="t-local"
SELF_SENDER="$WORK/self-sender.txt"
SELF_PEER="$WORK/self-peer.txt"

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

# Both rows on THIS host would make comm-send.sh attempt a poke; the peer is
# pinned to another host so the poke is skipped by host and no daemon is ever
# dialled (this suite must not reach a real one).
setup_rows() {
    ( cd "$WORK" && SOT_COMM_SELF_FILE="$SELF_PEER" SOT_COMM_TEST_HOST="otherbox" \
        "$JOIN" --name "$LOCAL_PEER" ) >/dev/null 2>&1 || return 1
    ( cd "$WORK" && SOT_COMM_SELF_FILE="$SELF_SENDER" SOT_COMM_TEST_HOST="$SENDER_HOST" \
        "$JOIN" --name "$SENDER" ) >/dev/null 2>&1 || return 1
    : > "$STUB_LOG"
}

SEND_OUT=""; SEND_ERR=""; SEND_RC=0
run_send() {
    SEND_OUT="$(cd "$WORK" && SOT_COMM_SELF_FILE="$SELF_SENDER" SOT_COMM_TEST_HOST="$SENDER_HOST" \
        "$SEND" "$@" 2>"$WORK/err.txt")"
    SEND_RC=$?
    SEND_ERR="$(cat "$WORK/err.txt" 2>/dev/null)"
    return 0
}

case_a_registry_miss_execs_the_relay() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    run_send "@stranger-otherbox" "over the wire"
    contains "$SEND_ERR" "no such handle" \
        && { echo "  the dead-end refusal survived: '$SEND_ERR'"; return 1; }
    local argv; argv="$(cat "$STUB_LOG" 2>/dev/null)"
    [ "$argv" = 'send @stranger-otherbox over the wire' ] \
        || { echo "  the relay was called with '$argv', want 'send @stranger-otherbox over the wire'"; return 1; }
    contains "$SEND_OUT" "stub relay" \
        || { echo "  the relay's own verdict did not reach the caller: '$SEND_OUT'"; return 1; }
    return 0
}

case_a_registry_hit_files_locally_and_never_calls_the_relay() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local inbox="$SOT_COMM_HOME/inbox/$LOCAL_PEER.jsonl"
    : > "$inbox"
    run_send "@$LOCAL_PEER" "a local append"
    [ "$SEND_RC" -eq 0 ] || { echo "  exited $SEND_RC (out: '$SEND_OUT' err: '$SEND_ERR')"; return 1; }
    contains "$SEND_OUT" "filed -> @$LOCAL_PEER" \
        || { echo "  verdict was '$SEND_OUT'"; return 1; }
    local n; n="$(grep -c 'a local append' "$inbox" 2>/dev/null || echo 0)"
    [ "$n" -eq 1 ] || { echo "  the inbox holds $n copies, want exactly 1"; return 1; }
    [ -s "$STUB_LOG" ] \
        && { echo "  a registry hit reached the relay: '$(cat "$STUB_LOG")'"; return 1; }
    return 0
}

case_a_broadcast_never_execs_the_relay() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    run_send --broadcast "to everyone with a row"
    [ "$SEND_RC" -eq 0 ] || { echo "  exited $SEND_RC (out: '$SEND_OUT' err: '$SEND_ERR')"; return 1; }
    [ -s "$STUB_LOG" ] \
        && { echo "  a broadcast reached the relay: '$(cat "$STUB_LOG")'"; return 1; }
    grep -q 'to everyone with a row' "$SOT_COMM_HOME/inbox/$LOCAL_PEER.jsonl" \
        || { echo "  the broadcast did not reach the peer's inbox"; return 1; }
    return 0
}

# S3 — a broadcast counts only the copies that were filed: one that cannot be
# appended prints its FAILED line, the count says N of M, and the exit is 1.
case_a_broadcast_with_a_failed_copy_is_not_success() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    ( cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-broken.txt" SOT_COMM_TEST_HOST="otherbox" \
        "$JOIN" --name t-broken-peer ) >/dev/null 2>&1 || { echo "  setup: could not join a third row"; return 1; }
    local broken="$SOT_COMM_HOME/inbox/t-broken-peer.jsonl"
    rm -f "${broken:?}"; mkdir -p "$broken"   # a directory: that copy cannot be appended
    run_send --broadcast "to everyone, one copy fails"
    rmdir "$broken"
    [ "$SEND_RC" -eq 1 ] || { echo "  exited $SEND_RC, want 1 (out: '$SEND_OUT' err: '$SEND_ERR')"; return 1; }
    contains "$SEND_ERR" "FAILED -> @t-broken-peer: the append failed: " \
        || { echo "  no FAILED line for the broken copy: '$SEND_ERR'"; return 1; }
    contains "$SEND_OUT" "Broadcast to 1 of 2 agent(s)." || { echo "  the count: '$SEND_OUT'"; return 1; }
    grep -q 'one copy fails' "$SOT_COMM_HOME/inbox/$LOCAL_PEER.jsonl" \
        || { echo "  the copy that could be filed is missing"; return 1; }
    return 0
}

case_a_hostless_row_terminates_instead_of_ping_ponging() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    # A row that EXISTS with no `host`: a hit for "does a row exist", a miss
    # for "can this box file and poke it". While those two questions were
    # asked by different doors, comm-send.sh handed this to the relay, the
    # relay handed it straight back, and the pair span forever (leaking one
    # temp file per lap). The endpoint below is a socket nothing listens on,
    # so the ONE honest lap ends in the relay's no-answer FAILED line.
    jq --arg t "$LOCAL_PEER" '.agents[$t] = {}' "$SOT_COMM_HOME/registry.json" \
        > "$SOT_COMM_HOME/registry.json.tmp" \
        && mv "$SOT_COMM_HOME/registry.json.tmp" "$SOT_COMM_HOME/registry.json"
    local out rc
    out="$(cd "$WORK" && SOT_COMM_SELF_FILE="$SELF_SENDER" SOT_COMM_TEST_HOST="$SENDER_HOST" \
        SOT_RELAY_ENDPOINT="unix:$WORK/no-such-daemon.sock" \
        timeout 5 "$SEND_REAL" "@$LOCAL_PEER" "round and round" 2>&1)"
    rc=$?
    [ "$rc" -ne 124 ] || { echo "  the two verbs ping-ponged until the timeout killed them"; return 1; }
    [ "$rc" -eq 1 ] || { echo "  exited $rc, want 1 (out: '$out')"; return 1; }
    local n; n="$(printf '%s\n' "$out" | grep -c "FAILED -> @$LOCAL_PEER: the daemon did not answer at " || true)"
    [ "$n" -eq 1 ] || { echo "  $n no-answer refusals, want exactly 1 (out: '$out')"; return 1; }
    return 0
}

check "a registry miss on a directed send execs the relay with the same args" case_a_registry_miss_execs_the_relay
check "a registry hit files locally and never calls the relay" case_a_registry_hit_files_locally_and_never_calls_the_relay
check "a broadcast never execs the relay" case_a_broadcast_never_execs_the_relay
check "a row that exists with no host terminates in one refusal, never a ping-pong" case_a_hostless_row_terminates_instead_of_ping_ponging
check "a broadcast with a failed copy counts only the filed ones and exits 1" case_a_broadcast_with_a_failed_copy_is_not_success

echo "---"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
