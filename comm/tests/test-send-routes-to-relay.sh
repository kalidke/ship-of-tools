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
#   5. `filed` is for a live handle: a listed peer whose registry `last_seen`
#      is not under 600 s old is FAILED by the script itself, with the
#      daemon's own sentence, before anything is built or appended, and no
#      daemon is asked (R1, R2); a broadcast files its live copies and prints a
#      FAILED line for each other (R4); the filed line is the verdict alone
#      (R5); a stamp with a trailing newline is no heartbeat (R6); and every
#      script append and every `last_seen` file is on a pinned list (R7).
#
# No bats dependency. HERMETIC, same seams as test-relay-file-first.sh: a temp
# $SOT_COMM_HOME, a per-case $SOT_COMM_SELF_FILE, a pinned $SOT_COMM_TEST_HOST,
# and a COPY of the scripts dir whose comm-relay.sh is a stub that records its
# argv — never the real ~/.sot-comm, never a real daemon, never the real relay.
#
# Usage: comm/tests/test-send-routes-to-relay.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-comm-send-routes-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
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
    # temp file per lap). The relay endpoint below (a stub `sotd`'s answer to
    # `topology relay-endpoint`) is a socket nothing listens on, so the ONE
    # honest lap ends in the relay's no-answer FAILED line.
    jq --arg t "$LOCAL_PEER" '.agents[$t] = {}' "$SOT_COMM_HOME/registry.json" \
        > "$SOT_COMM_HOME/registry.json.tmp" \
        && mv "$SOT_COMM_HOME/registry.json.tmp" "$SOT_COMM_HOME/registry.json"
    local out rc
    mkdir -p "$WORK/relay-answer"
    printf '#!/bin/sh\n[ "$1 $2" = "topology relay-endpoint" ] && { echo "unix:%s/no-such-daemon.sock"; exit 0; }\nexit 97\n' \
        "$WORK" > "$WORK/relay-answer/sotd"; chmod +x "$WORK/relay-answer/sotd"
    out="$(cd "$WORK" && SOT_COMM_SELF_FILE="$SELF_SENDER" SOT_COMM_TEST_HOST="$SENDER_HOST" \
        SOTD_BIN="$WORK/relay-answer/sotd" timeout 5 "$SEND_REAL" "@$LOCAL_PEER" "round and round" 2>&1)"
    rc=$?
    [ "$rc" -ne 124 ] || { echo "  the two verbs ping-ponged until the timeout killed them"; return 1; }
    [ "$rc" -eq 1 ] || { echo "  exited $rc, want 1 (out: '$out')"; return 1; }
    local n; n="$(printf '%s\n' "$out" | grep -c "FAILED -> @$LOCAL_PEER: the daemon did not answer at " || true)"
    [ "$n" -eq 1 ] || { echo "  $n no-answer refusals, want exactly 1 (out: '$out')"; return 1; }
    return 0
}

# ---- 5. `filed` is for a live handle ----
# A stub bridge stands in for the daemon (comm.file only, logging each frame):
# its answer is the payload in $HUB/answer.
HUB="$WORK/hub"
stub_daemon() {  # PAYLOAD
    rm -rf "${HUB:?}"; mkdir -p "$HUB"; printf '%s' "$1" > "$HUB/answer"
    { printf '#!/bin/sh\nd=%s\n' "$HUB"; cat <<'NCSTUB'
[ "$1" = stdio-bridge ] && [ "$2" = --endpoint ] || exit 97
case "$3" in unix:*) ;; *) exit 97 ;; esac
while IFS= read -r line; do
    case "$line" in
        *'"op":"comm.file"'*)
            printf '%s\n' "$line" >> "$d/comm-file.log"
            printf '{"v":1,"id":1,"kind":"res","op":"comm.file","payload":%s}\n' "$(cat "$d/answer")"
            exit 0 ;;
    esac
done
NCSTUB
    } > "$HUB/sotd"; chmod +x "$HUB/sotd"
    export SOTD_BIN="$HUB/sotd" SOT_SOCKET="$WORK/hub.sock"; PATH="$HUB:$PATH"
}
end_stub_daemon() { unset SOT_SOCKET SOTD_BIN; PATH="${PATH#"$HUB":}"; }
# The peer's heartbeat one hour old: nothing says it is alive.
make_stale() {  # HANDLE
    jq --arg t "$1" --arg ls "$(date -u -d '1 hour ago' +%Y-%m-%dT%H:%M:%SZ)" '.agents[$t].last_seen = $ls' \
        "$SOT_COMM_HOME/registry.json" > "$SOT_COMM_HOME/registry.json.tmp" \
        && mv "$SOT_COMM_HOME/registry.json.tmp" "$SOT_COMM_HOME/registry.json"
}
unchanged() { [ ! -e "$SOT_COMM_HOME/inbox/$LOCAL_PEER.jsonl" ] || [ ! -s "$SOT_COMM_HOME/inbox/$LOCAL_PEER.jsonl" ]; }

# R1: stale, no daemon anywhere: the script refuses with the daemon's sentence.
case_a_stale_peer_is_failed_by_the_script_and_appends_nothing() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    make_stale "$LOCAL_PEER"; rm -f "${SOT_COMM_HOME:?}/inbox/$LOCAL_PEER.jsonl"
    run_send "@$LOCAL_PEER" "to a gone session"
    [ "$SEND_RC" -eq 1 ] || { echo "  exited $SEND_RC, want 1 (out: '$SEND_OUT' err: '$SEND_ERR')"; return 1; }
    [ "$SEND_ERR" = "FAILED -> @$LOCAL_PEER: no live session holds @$LOCAL_PEER" ] || { echo "  stderr was '$SEND_ERR'"; return 1; }
    contains "$SEND_OUT" "filed" && { echo "  printed filed: '$SEND_OUT'"; return 1; }
    unchanged || { echo "  the script appended itself"; return 1; }
    [ -s "$STUB_LOG" ] && { echo "  the relay ran: '$(cat "$STUB_LOG")'"; return 1; }
    return 0
}

# R2: stale, with a daemon that would say ok: the refusal is the same, and the daemon is never asked.
case_a_stale_peer_asks_no_daemon() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    make_stale "$LOCAL_PEER"; rm -f "${SOT_COMM_HOME:?}/inbox/$LOCAL_PEER.jsonl"
    stub_daemon '{"ok":true}'
    run_send "@$LOCAL_PEER" "a row runs it"
    end_stub_daemon
    [ "$SEND_RC" -eq 1 ] || { echo "  exited $SEND_RC, want 1 (out: '$SEND_OUT' err: '$SEND_ERR')"; return 1; }
    [ "$SEND_ERR" = "FAILED -> @$LOCAL_PEER: no live session holds @$LOCAL_PEER" ] || { echo "  stderr was '$SEND_ERR'"; return 1; }
    contains "$SEND_OUT" "filed" && { echo "  printed filed: '$SEND_OUT'"; return 1; }
    unchanged || { echo "  the script appended itself"; return 1; }
    [ ! -s "$HUB/comm-file.log" ] || { echo "  the stub daemon was asked: $(cat "$HUB/comm-file.log")"; return 1; }
    return 0
}

case_a_broadcast_files_the_live_copy_and_fails_the_stale_one() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    ( cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-gone.txt" SOT_COMM_TEST_HOST="otherbox" \
        "$JOIN" --name t-gone-peer ) >/dev/null 2>&1 || { echo "  setup: could not join a third row"; return 1; }
    jq 'del(.agents["t-broken-peer"])' "$SOT_COMM_HOME/registry.json" > "$SOT_COMM_HOME/registry.json.tmp" \
        && mv "$SOT_COMM_HOME/registry.json.tmp" "$SOT_COMM_HOME/registry.json"   # the S3 case's third row
    make_stale t-gone-peer; rm -f "${SOT_COMM_HOME:?}/inbox/t-gone-peer.jsonl"
    run_send --broadcast "to the live and the gone"
    [ "$SEND_RC" -eq 1 ] || { echo "  exited $SEND_RC, want 1 (out: '$SEND_OUT' err: '$SEND_ERR')"; return 1; }
    contains "$SEND_OUT" "filed -> @$LOCAL_PEER" || { echo "  the live copy was not filed: '$SEND_OUT'"; return 1; }
    contains "$SEND_ERR" "FAILED -> @t-gone-peer: no live session holds @t-gone-peer" || { echo "  no FAILED line for the stale copy: '$SEND_ERR'"; return 1; }
    contains "$SEND_OUT" "Broadcast to 1 of 2 agent(s)." || { echo "  the count: '$SEND_OUT'"; return 1; }
    [ ! -e "$SOT_COMM_HOME/inbox/t-gone-peer.jsonl" ] || { echo "  the stale copy was appended"; return 1; }
    grep -q 'to the live and the gone' "$SOT_COMM_HOME/inbox/$LOCAL_PEER.jsonl" || { echo "  the live copy is missing"; return 1; }
    return 0
}

case_the_filed_line_is_the_verdict_alone() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    jq --arg t "$LOCAL_PEER" --arg now "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        '.agents[$t] += {state:"working", summary:"n", status_at:$now, last_seen:$now}' "$SOT_COMM_HOME/registry.json" \
        > "$SOT_COMM_HOME/registry.json.tmp" && mv "$SOT_COMM_HOME/registry.json.tmp" "$SOT_COMM_HOME/registry.json"
    run_send "@$LOCAL_PEER" "a working peer"
    [ "$SEND_RC" -eq 0 ] || { echo "  exited $SEND_RC (out: '$SEND_OUT' err: '$SEND_ERR')"; return 1; }
    [ "$SEND_OUT" = "  filed -> @$LOCAL_PEER" ] || { echo "  verdict was '$SEND_OUT'"; return 1; }
    return 0
}

# R6: a fresh stamp plus a newline is a string of 21 characters, no heartbeat.
case_a_stamp_with_a_trailing_newline_is_no_heartbeat() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    rm -f "${SOT_COMM_HOME:?}/inbox/$LOCAL_PEER.jsonl"
    jq --arg t "$LOCAL_PEER" --arg ls "$(date -u +%Y-%m-%dT%H:%M:%SZ)"$'\n' '.agents[$t].last_seen = $ls' \
        "$SOT_COMM_HOME/registry.json" > "$SOT_COMM_HOME/registry.json.tmp" \
        && mv "$SOT_COMM_HOME/registry.json.tmp" "$SOT_COMM_HOME/registry.json"
    run_send "@$LOCAL_PEER" "a newline in the stamp"
    [ "$SEND_RC" -eq 1 ] && contains "$SEND_ERR" "FAILED -> @$LOCAL_PEER: no live session holds @$LOCAL_PEER" \
        || { echo "  exited $SEND_RC (out: '$SEND_OUT' err: '$SEND_ERR')"; return 1; }
    unchanged || { echo "  the script appended itself"; return 1; }
    return 0
}

# R7: the invariant "filed only after a liveness check" reaches every place that appends to an inbox
# or writes or reads a last_seen; a new member must be added here, with its check, on purpose. Over the
# tracked files of the checkout that are not pages, not Rust (filer.rs has that pin) and not tests.
case_every_append_and_last_seen_file_is_pinned() {
    local repo got want
    repo="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)" || { echo "  not in a git checkout"; return 1; }
    pin() {  # GREP-ARGS... -- WANT...
        local args=() rc=0
        while [ "$1" != -- ]; do args+=("$1"); shift; done; shift
        got="$(git -C "$repo" grep -l "${args[@]}" -- ':(exclude)*.md' ':(exclude)*.rs' ':(exclude,glob)**/tests/**' ':(exclude,glob)**/test/**')" || rc=$?
        [ "$rc" -le 1 ] || { echo "  git grep failed ($rc) for ${args[*]}"; return 1; }
        got="$(printf '%s\n' "$got" | sort | tr '\n' ' ')"
        want="$(printf '%s\n' "$@" | sort | tr '\n' ' ')"
        [ "$got" = "$want" ] || { echo "  files matching ${args[*]}: $got, want $want"; return 1; }
    }
    pin -w sot_inbox_append -- comm/lib/comm-lib-identity.sh comm/lib/comm-lib-inbox.sh comm/mail/comm-send.sh || return 1
    pin -w _sot_append_whole -- comm/lib/comm-lib-inbox.sh comm/lib/comm-lib-registry.sh || return 1
    # a redirection into an inbox file: both are `: >>` touches, which add no line
    pin -E '>>?[[:space:]]*"?[^[:space:]]*(INBOX|inbox)[^[:space:]]*\.jsonl' -- agents/spawn/comm-spawn.sh comm/registry/comm-join.sh || return 1
    pin -w last_seen -- agents/spawn/comm-spawn.sh comm/lib/comm-lib-registry-lock.sh comm/lib/comm-lib-registry.sh \
        comm/mail/comm-send.sh comm/registry/comm-join.sh comm/registry/comm-list.sh comm/work_state/comm-status.sh \
        comm/work_state/hooks/comm-status-heartbeat.sh || return 1
    return 0
}

check "a registry miss on a directed send execs the relay with the same args" case_a_registry_miss_execs_the_relay
check "a registry hit files locally and never calls the relay" case_a_registry_hit_files_locally_and_never_calls_the_relay
check "a broadcast never execs the relay" case_a_broadcast_never_execs_the_relay
check "a row that exists with no host terminates in one refusal, never a ping-pong" case_a_hostless_row_terminates_instead_of_ping_ponging
check "a broadcast with a failed copy counts only the filed ones and exits 1" case_a_broadcast_with_a_failed_copy_is_not_success
check "R1: a stale peer is FAILED by the script with the daemon's sentence, and nothing is appended" case_a_stale_peer_is_failed_by_the_script_and_appends_nothing
check "R2: a stale peer asks no daemon" case_a_stale_peer_asks_no_daemon
check "R4: a broadcast files the live copy and fails the stale one" case_a_broadcast_files_the_live_copy_and_fails_the_stale_one
check "R5: the filed line is the verdict alone" case_the_filed_line_is_the_verdict_alone
check "R6: a last_seen with a trailing newline is no heartbeat" case_a_stamp_with_a_trailing_newline_is_no_heartbeat
check "R7: every script append and every last_seen file is on its pinned list" case_every_append_and_last_seen_file_is_pinned

echo "---"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
