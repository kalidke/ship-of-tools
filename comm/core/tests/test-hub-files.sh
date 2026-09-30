#!/usr/bin/env bash
# test-hub-files.sh — the inbox append (0031 B1): one lock, both writers,
# fail-closed.
#
#   1. Two writers through `sot_inbox_append` on one inbox give every line
#      whole: none torn, none interleaved, none lost.
#   2. The lock is the kernel's: a holder killed with -9 frees it at once, so
#      the next send files with nothing to time out; a FROZEN holder makes the
#      next send wait its bound and then report FAILED, never `filed`, and when
#      the holder resumes the inbox has no torn line.
#   3. Where flock(1) is absent the script never appends unlocked: the frame
#      goes to this box's own daemon as `comm.file`, and with no daemon, or a
#      refusal, the send is FAILED.
#   4. The wait is ONE number, 10, and no stale/patience/reclaim constant is
#      spelled at all — the lease is deleted and this keeps it deleted.
#
# No bats dependency. HERMETIC: a temp $SOT_COMM_HOME, per-case self files, a
# pinned $SOT_COMM_TEST_HOST, and a COPY of the scripts dir whose comm-lib.sh
# gets `sot_daemon_endpoint` APPENDED (a sourced file's later definitions win)
# so no real daemon is ever dialled. Never the real ~/.sot-comm.
#
# Usage: comm/core/tests/test-hub-files.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-comm-hub-files-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
export SOT_COMM_HOME="$WORK/home"
mkdir -p "$SOT_COMM_HOME/inbox"
HOLDERS=()
trap 'for p in "${HOLDERS[@]}"; do kill -9 "$p" 2>/dev/null; done; rm -rf "$WORK"' EXIT

BIN="$WORK/bin"
cp -r "$SCRIPTS_DIR" "$BIN"
cat >> "$BIN/comm-lib.sh" <<'STUB'

# ---- no daemon (test only) --------------------------------------------------
sot_daemon_endpoint() { return 1; }
STUB
SEND="$BIN/comm-send.sh"
JOIN="$BIN/comm-join.sh"
LIB="$BIN/comm-lib.sh"
INBOX="$SOT_COMM_HOME/inbox"

HOST_PIN="testhost"
SENDER="t-sender"
PEER="t-peer"

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

setup_rows() {
    rm -f "$SOT_COMM_HOME/registry.json" "$INBOX"/*
    ( cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-peer.txt" SOT_COMM_TEST_HOST="$HOST_PIN" \
        "$JOIN" --name "$PEER" ) >/dev/null 2>&1 || return 1
    ( cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-sender.txt" SOT_COMM_TEST_HOST="$HOST_PIN" \
        "$JOIN" --name "$SENDER" ) >/dev/null 2>&1 || return 1
    : > "$INBOX/$PEER.jsonl"
}

SEND_OUT=""; SEND_ERR=""; SEND_RC=0
run_send() {
    SEND_OUT="$(cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-sender.txt" SOT_COMM_TEST_HOST="$HOST_PIN" \
        "$SEND" "$@" 2>"$WORK/err.txt")"
    SEND_RC=$?
    SEND_ERR="$(cat "$WORK/err.txt" 2>/dev/null)"
    return 0
}

# One line through the copy's own helper. $1 = handle, $2 = the JSON line.
append_one() {
    printf '%s\n' "$2" | bash -c 'source "$1/comm-lib.sh"; sot_inbox_append "$2"' _ "$BIN" "$1"
}

# Every line of $1 is one JSON object; prints the count, fails on any other.
whole_lines() {
    local f="$1" n bad
    n="$(wc -l < "$f")"
    bad="$(jq -c 'select(type != "object")' "$f" 2>&1 >/dev/null | wc -l)"
    [ "$(jq -c . "$f" 2>/dev/null | wc -l)" -eq "$n" ] && [ "$bad" -eq 0 ] || return 1
    [ -z "$(tail -c1 "$f")" ] || return 1   # LF-terminated, no partial tail
    printf '%s' "$n"
}

# A holder of the peer's inbox lock, in the background: takes the lock, then
# runs $1 (`exec sleep 60` holds it; the frozen case writes half a line and
# stops itself). `exec` so the recorded pid IS the lock holder — a child that
# inherited fd 9 would keep the lock past the kill.
start_holder() {
    local body="$1"
    rm -f "$WORK/ready"
    bash -c 'exec 9>> "$1/$2.lock"; flock 9; exec 8>> "$1/$2.jsonl"; touch "$3"; '"$body" \
        _ "$INBOX" "$PEER" "$WORK/ready" &
    HOLDER=$!
    HOLDERS+=("$HOLDER")
    local i=0
    while [ ! -e "$WORK/ready" ] && [ "$i" -lt 100 ]; do sleep 0.05; i=$((i + 1)); done
    [ -e "$WORK/ready" ]
}

# T3 (shell arm) — two writers, 200 lines each, one inbox.
case_two_writers_give_400_whole_lines() {
    local h="t-two" f="$INBOX/t-two.jsonl" w n
    rm -f "$f"
    for w in a b; do
        ( for i in $(seq 1 200); do
              append_one "$h" "{\"from\":\"$w\",\"to\":\"$h\",\"repo\":\"r\",\"msg\":\"$w-$i $(printf 'x%.0s' $(seq 1 64))\",\"ts\":\"t\"}" \
                  >/dev/null || echo "refused $w-$i" >> "$WORK/two.err"
          done ) &
    done
    wait
    [ ! -s "$WORK/two.err" ] || { echo "  refusals: $(head -3 "$WORK/two.err")"; return 1; }
    n="$(whole_lines "$f")" || { echo "  a line is not one JSON object"; return 1; }
    [ "$n" -eq 400 ] || { echo "  $n lines, want 400"; return 1; }
    [ "$(jq -r '.msg | split(" ")[0]' "$f" | sort -u | wc -l)" -eq 400 ] \
        || { echo "  a line was lost or doubled"; return 1; }
    return 0
}

# T12 (shell arm) — a killed holder costs nothing: the OS released the lock.
case_a_killed_holder_frees_the_lock_at_once() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    start_holder 'exec sleep 60' || { echo "  holder never took the lock"; return 1; }
    kill -9 "$HOLDER"; wait "$HOLDER" 2>/dev/null
    local t0=$SECONDS
    run_send "@$PEER" "after the kill"
    [ "$SEND_RC" -eq 0 ] || { echo "  rc $SEND_RC: $SEND_ERR"; return 1; }
    contains "$SEND_OUT" "filed -> @$PEER" || { echo "  out: $SEND_OUT"; return 1; }
    [ $((SECONDS - t0)) -lt 3 ] || { echo "  waited $((SECONDS - t0))s for a dead holder"; return 1; }
    [ "$(whole_lines "$INBOX/$PEER.jsonl")" = 1 ] || { echo "  inbox not one whole line"; return 1; }
    return 0
}

# T12 (shell arm) — a frozen holder makes the sender wait, then FAILED.
case_a_frozen_holder_makes_the_send_wait_then_fail() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    start_holder 'printf "%s" "{\"from\":\"holder\"," >&8; kill -STOP $$; printf "%s\n" "\"msg\":\"resumed\"}" >&8' \
        || { echo "  holder never took the lock"; return 1; }
    local t0=$SECONDS
    SOT_INBOX_LOCK_WAIT_SECS=1 run_send "@$PEER" "while frozen"
    [ "$SEND_RC" -eq 1 ] || { echo "  rc $SEND_RC, want 1 (out: $SEND_OUT)"; return 1; }
    contains "$SEND_OUT" "filed" && { echo "  said filed: $SEND_OUT"; return 1; }
    contains "$SEND_ERR" "FAILED -> @$PEER: the inbox lock for @$PEER was held for 1s — nothing was appended" \
        || { echo "  err: $SEND_ERR"; return 1; }
    [ $((SECONDS - t0)) -ge 1 ] || { echo "  did not wait"; return 1; }
    kill -CONT "$HOLDER"; wait "$HOLDER" 2>/dev/null
    [ "$(whole_lines "$INBOX/$PEER.jsonl")" = 1 ] || { echo "  torn after resume"; return 1; }
    [ "$(jq -r .msg "$INBOX/$PEER.jsonl")" = "resumed" ] || { echo "  the frozen send appended"; return 1; }
    run_send "@$PEER" "after the resume"
    [ "$SEND_RC" -eq 0 ] && [ "$(whole_lines "$INBOX/$PEER.jsonl")" = 2 ] \
        || { echo "  the next send did not file (rc $SEND_RC: $SEND_ERR)"; return 1; }
    return 0
}

# An append that fails under the lock is FAILED with the error, not `filed`.
case_a_failed_write_is_failed_not_filed() {
    local out rc=0
    mkdir -p "$INBOX/t-dir.jsonl"   # a directory: the append cannot open it
    out="$(append_one t-dir '{"from":"a","msg":"m"}')" || rc=$?
    rmdir "$INBOX/t-dir.jsonl"
    [ "$rc" -eq 1 ] || { echo "  rc $rc, want 1"; return 1; }
    contains "$out" "the append failed: " || { echo "  out: $out"; return 1; }
    return 0
}

# No flock(1): the frame goes to this box's daemon as comm.file, never an
# unlocked append. $1 = the daemon's answer ("" = no daemon at all).
no_flock_append() {
    printf '%s\n' '{"from":"t-sender","to":"t-peer","repo":"r","msg":"/slash first","ts":"t"}' \
        | ANSWER="$1" FRAME_LOG="$WORK/frame.log" bash -c '
            source "$1/comm-lib.sh"
            _sot_have_flock() { return 1; }
            if [ -n "$ANSWER" ]; then
                sot_daemon_endpoint() { printf "unix:/fixture"; }
                sot_oneshot_request() { printf "%s\n" "$1" > "$FRAME_LOG"; [ "$2" = comm.file ] && printf "%s" "$ANSWER"; }
            fi
            sot_inbox_append t-peer' _ "$BIN"
}
case_no_flock_routes_through_the_daemon() {
    local out rc
    rm -f "$INBOX/t-peer.jsonl" "$WORK/frame.log"
    out="$(no_flock_append '{"v":1,"id":1,"kind":"res","op":"comm.file","payload":{"ok":true}}')"; rc=$?
    [ "$rc" -eq 0 ] || { echo "  ok answer: rc $rc ($out)"; return 1; }
    jq -e '.op == "comm.file" and .payload == {from:"t-sender",to:"t-peer",text:"/slash first"}' \
        "$WORK/frame.log" >/dev/null || { echo "  frame: $(cat "$WORK/frame.log")"; return 1; }
    out="$(no_flock_append '{"v":1,"id":1,"kind":"res","op":"comm.file","payload":{"error":"no live session holds @t-peer","code":"no_live_session"}}')"; rc=$?
    [ "$rc" -eq 1 ] && [ "$out" = "no live session holds @t-peer" ] || { echo "  refusal: rc $rc ($out)"; return 1; }
    out="$(no_flock_append '')"; rc=$?
    [ "$rc" -eq 1 ] && contains "$out" "no daemon here" || { echo "  no daemon: rc $rc ($out)"; return 1; }
    [ ! -e "$INBOX/t-peer.jsonl" ] || { echo "  appended unlocked"; return 1; }
    return 0
}

# T13 — the wait is ONE number in both languages; no lease constant survives.
case_the_wait_is_one_number_and_no_lease_survives() {
    local lib="$SCRIPTS_DIR/comm-lib.sh" names
    [ "$(grep -c 'SOT_INBOX_LOCK_WAIT_SECS="${SOT_INBOX_LOCK_WAIT_SECS:-10}"' "$lib")" -eq 1 ] \
        || { echo "  comm-lib.sh does not default the wait to 10 exactly once"; return 1; }
    names="$(grep -ohE 'SOT_INBOX_LOCK_[A-Z_]+' "$lib" | sort -u)"
    [ "$names" = "SOT_INBOX_LOCK_WAIT_SECS" ] || { echo "  lock knobs: $names"; return 1; }
    grep -niE 'inbox.{0,40}(stale|patience|reclaim|lease)|(stale|patience|reclaim|lease).{0,40}inbox' "$lib" \
        | grep -v '^[0-9]*: *#' && { echo "  a lease constant is spelled in comm-lib.sh"; return 1; }
    grep -n 'flock -w' "$lib" | grep -qv 'SOT_INBOX_LOCK_WAIT_SECS' && { echo "  a flock wait not read from the one knob"; return 1; }
    return 0
}

check "two writers through the lock give 400 whole lines" case_two_writers_give_400_whole_lines
check "a holder killed with -9 frees the lock at once and the send files" case_a_killed_holder_frees_the_lock_at_once
check "a frozen holder makes the send wait its bound and report FAILED, never filed" case_a_frozen_holder_makes_the_send_wait_then_fail
check "an append that fails under the lock is FAILED with its error" case_a_failed_write_is_failed_not_filed
check "with no flock(1) the frame goes to the daemon as comm.file, never unlocked" case_no_flock_routes_through_the_daemon
check "the wait is one number, 10, and no lease constant is spelled" case_the_wait_is_one_number_and_no_lease_survives

echo "---"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
