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
#   3. A script appends locally only when flock(1) exists, this is Linux, and
#      its lock identity for the inbox equals the daemon's record; NFSv3, an
#      unknown mount, a mismatched export, a host mounting the daemon's local
#      disk, no record, a `none` record, no flock(1) and no perl all go to the
#      wire —
#      the fake daemon gets exactly one `comm.file` and the inbox is unchanged.
#      The wire is this box's own daemon, else the relay endpoint, and one that
#      does not answer is FAILED with no second route tried.
#   4. The wait is ONE number, 10, in both languages, and no stale/patience/
#      reclaim constant is spelled at all — the lease is deleted and this
#      keeps it deleted.
#   5. The lock identity: the same fixture set comm_inbox.rs's unit test reads
#      gives the same strings here; a broadcast copy says so on the wire.
#   6. A wire send (comm-relay.sh's send_frame) is one `comm.file` frame with
#      no id, and prints the hub's answer: `filed -> @h`, or `FAILED -> @h:`
#      and the daemon's own sentence, code or no code, or the no-answer
#      sentence; `not_here` alone falls back to `agent.send`. The read window
#      outlasts the hub's lock wait, so a line filed after it is not FAILED.
#   7. A hub-filed line and a locally-filed line read the same through
#      comm-poll.sh, and both advance the one cursor.
#
# No bats dependency. HERMETIC: a temp $SOT_COMM_HOME, per-case self files, a
# pinned $SOT_COMM_TEST_HOST, and a COPY of the scripts dir whose comm-lib.sh
# gets its endpoints and mount lookup APPENDED (a sourced file's later
# definitions win) so no real daemon is ever dialled and the route does not
# depend on this box's disks. Never the real ~/.sot-comm.
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

# ---- no daemon, a fixture mount (test only) ---------------------------------
sot_daemon_endpoint() { return 1; }
sot_relay_endpoint() { [ -n "${1:-}" ] || return 1; printf '%s\n' "$1"; }  # an explicit one is used as given
_sot_findmnt() { printf '%s\n' "${FAKE_MNT-nfs4 rw,vers=4.2,local_lock=none filer.example:/export/home}"; }
_sot_machine_id() { printf '0123456789abcdef0123456789abcdef'; }
STUB
RECORD="nfs4 filer.example:/export/home"
printf '%s\n' "$RECORD" > "$SOT_COMM_HOME/inbox-lock-manager"
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

# S2 — a torn tail (a writer that died mid-line) stays its own line, and the
# new line stays whole and parses.
case_a_torn_tail_is_ended_before_the_new_line() {
    local out rc=0 f="$INBOX/t-torn.jsonl"
    printf '%s' '{"from":"died",' > "$f"
    out="$(append_one t-torn '{"from":"a","msg":"whole"}')" || rc=$?
    [ "$rc" -eq 0 ] || { echo "  rc $rc ($out)"; return 1; }
    [ "$(sed -n 1p "$f")" = '{"from":"died",' ] || { echo "  the torn line changed: $(sed -n 1p "$f")"; return 1; }
    [ "$(sed -n 2p "$f" | jq -r .msg)" = whole ] || { echo "  the new line: $(sed -n 2p "$f")"; return 1; }
    [ "$(wc -l < "$f")" -eq 2 ] || { echo "  $(wc -l < "$f") lines, want 2"; return 1; }
    return 0
}

# S2 — a write the file-size limit cuts off mid-line (`ulimit -f 1` is 1024
# bytes; the file holds 1000; SIGXFSZ ignored so perl's write sees EFBIG):
# FAILED, and the file is byte-identical. The fsync itself is read, not tested.
case_a_write_cut_short_leaves_the_file_byte_identical() {
    local out rc=0 f="$INBOX/t-fsize.jsonl" long
    printf '{"msg":"%s"}\n' "$(printf '%0989d' 0)" > "$f"
    [ "$(wc -c < "$f")" -eq 1000 ] || { echo "  setup: $(wc -c < "$f") bytes"; return 1; }
    cp "$f" "$WORK/fsize.before"
    long="$(printf '%0200d' 0)"
    out="$( ulimit -f 1; trap '' XFSZ; append_one t-fsize "{\"from\":\"a\",\"msg\":\"$long\"}" )" || rc=$?
    [ "$rc" -eq 1 ] || { echo "  rc $rc, want 1 ($out)"; return 1; }
    contains "$out" "the append failed: " || { echo "  out: $out"; return 1; }
    cmp -s "$f" "$WORK/fsize.before" || { echo "  the file changed: $(wc -c < "$f") bytes"; return 1; }
    return 0
}

# S-a — a tail of NUL bytes (NFS after a client crash) is not a line end.
case_a_nul_tail_is_ended_before_the_new_line() {
    local out rc=0 f="$INBOX/t-nul.jsonl"
    printf '{"from":"old"}\n\0\0\0' > "$f"
    out="$(append_one t-nul '{"from":"a","msg":"whole"}')" || rc=$?
    [ "$rc" -eq 0 ] || { echo "  rc $rc ($out)"; return 1; }
    [ "$(tail -n 1 "$f" | jq -r .msg)" = whole ] || { echo "  the new line: $(tail -n 1 "$f" | od -c | head -3)"; return 1; }
    return 0
}

# One append through the guard with the wire faked. $1 = own mount as
# findmnt prints it ("" = unknown), $2 = the record ("-" = absent), $3 = own
# daemon endpoint, $4 = relay endpoint ("" = none), $5 = the fake daemon's
# answer ("" = silence), $6 = 0 for no flock(1), $7 = the line. Each wire
# attempt is one `<endpoint> <frame>` line in $WORK/wire.log.
OK_ANSWER='{"v":1,"id":1,"kind":"res","op":"comm.file","payload":{"ok":true}}'
DIRECTED='{"from":"t-sender","to":"t-peer","repo":"r","msg":"/slash first","ts":"t"}'
route_append() {
    rm -f "$SOT_COMM_HOME/inbox-lock-manager"
    [ "$2" = - ] || printf '%s\n' "$2" > "$SOT_COMM_HOME/inbox-lock-manager"
    printf '%s\n' "${7:-$DIRECTED}" | FAKE_MNT="$1" OWN="$3" RELAY="$4" ANSWER="$5" FLOCK="${6:-1}" \
        WIRE="$WORK/wire.log" bash -c '
            source "$1/comm-lib.sh"
            if [ "$FLOCK" = 0 ]; then _sot_have_flock() { return 1; }; fi
            sot_daemon_endpoint() { [ -n "$OWN" ] && printf "%s" "$OWN"; }
            sot_relay_endpoint() { [ -n "$RELAY" ] && printf "%s" "$RELAY"; }
            sot_oneshot_request() { printf "%s %s\n" "$ENDPOINT" "$1" >> "$WIRE"; [ -n "$ANSWER" ] && printf "%s" "$ANSWER"; }
            sot_inbox_append t-peer' _ "$BIN"
    local rc=$?
    printf '%s\n' "$RECORD" > "$SOT_COMM_HOME/inbox-lock-manager"
    return "$rc"
}
fresh_route() { rm -f "$WORK/wire.log"; printf '%s\n' '{"msg":"before"}' > "$INBOX/t-peer.jsonl"; }
wire_count() { [ -e "$WORK/wire.log" ] && wc -l < "$WORK/wire.log" || echo 0; }

case_a_shared_nfs4_lock_manager_appends_locally() {
    local out rc
    fresh_route
    out="$(route_append "nfs4 rw,vers=4.2,local_lock=none A:/x" "nfs4 A:/x" unix:/own ssh:hub "$OK_ANSWER")"; rc=$?
    [ "$rc" -eq 0 ] || { echo "  rc $rc ($out)"; return 1; }
    [ "$(wire_count)" -eq 0 ] || { echo "  went to the wire"; return 1; }
    [ "$(wc -l < "$INBOX/t-peer.jsonl")" -eq 2 ] || { echo "  not appended locally"; return 1; }
    return 0
}

# The hub's record is two lines, its lock manager then its writer; a script
# compares line 1 only.
case_a_two_line_record_whose_line_1_matches_appends_locally() {
    local out rc
    fresh_route
    out="$(route_append "nfs4 rw,vers=4.2,local_lock=none A:/x" "nfs4 A:/x"$'\n'"hub-a" unix:/own ssh:hub "$OK_ANSWER")"; rc=$?
    [ "$rc" -eq 0 ] || { echo "  rc $rc ($out)"; return 1; }
    [ "$(wire_count)" -eq 0 ] || { echo "  went to the wire"; return 1; }
    [ "$(wc -l < "$INBOX/t-peer.jsonl")" -eq 2 ] || { echo "  not appended locally"; return 1; }
    return 0
}

# Every case that cannot prove one lock manager goes to the wire.
case_anything_unproven_goes_to_the_wire() {
    local mnt rec flock out rc
    while IFS='|' read -r mnt rec flock; do
        fresh_route
        out="$(route_append "$mnt" "$rec" unix:/own ssh:hub "$OK_ANSWER" "$flock")"; rc=$?
        [ "$rc" -eq 0 ] || { echo "  [$mnt|$rec|$flock] rc $rc ($out)"; return 1; }
        [ "$(wire_count)" -eq 1 ] || { echo "  [$mnt|$rec|$flock] $(wire_count) wire frames, want 1"; return 1; }
        jq -e '.op == "comm.file"' <<<"$(cut -d' ' -f2- "$WORK/wire.log")" >/dev/null \
            || { echo "  [$mnt|$rec|$flock] not comm.file"; return 1; }
        [ "$(cat "$INBOX/t-peer.jsonl")" = '{"msg":"before"}' ] || { echo "  [$mnt|$rec|$flock] appended locally"; return 1; }
    done <<'CASES'
nfs rw,vers=3 A:/x|nfs4 A:/x|1
|nfs4 A:/x|1
nfs4 rw,vers=4.2,local_lock=none B:/x|nfs4 A:/x|1
nfs4 rw,vers=4.2,local_lock=none hub.example:/home|local 0123456789abcdef0123456789abcdef|1
nfs4 rw,vers=4.2,local_lock=none A:/x|-|1
nfs4 rw,vers=4.2,local_lock=none A:/x|none|1
fuse.sshfs rw u@far.example:/x|none|1
nfs4 rw,vers=4.2,local_lock=none A:/x|nfs4 A:/x|0
nfs4 rw,vers=4.2,local_lock=flock A:/x|nfs4 A:/x|1
CASES
    return 0
}

# perl makes the append, so a box without it cannot append locally: with
# flock(1), a matching record and a PATH holding every tool but perl, the
# send is one comm.file frame and the inbox is unchanged.
case_no_perl_goes_to_the_wire() {
    local d out rc
    mkdir -p "$WORK/noperl"
    for d in ${PATH//:/ }; do ln -s "$d"/* "$WORK/noperl/" 2>/dev/null; done
    rm -f "$WORK/noperl"/perl*
    ! PATH="$WORK/noperl" command -v perl >/dev/null 2>&1 || { echo "  perl is still on the PATH"; return 1; }
    PATH="$WORK/noperl" command -v flock >/dev/null 2>&1 || { echo "  flock left the PATH"; return 1; }
    fresh_route
    out="$(PATH="$WORK/noperl" route_append "nfs4 rw,vers=4.2,local_lock=none A:/x" "nfs4 A:/x" unix:/own ssh:hub "$OK_ANSWER")"; rc=$?
    [ "$rc" -eq 0 ] || { echo "  rc $rc ($out)"; return 1; }
    [ "$(wire_count)" -eq 1 ] || { echo "  $(wire_count) wire frames, want 1"; return 1; }
    [ "$(cat "$INBOX/t-peer.jsonl")" = '{"msg":"before"}' ] || { echo "  appended locally"; return 1; }
    return 0
}

# The wire is this box's own daemon, else the relay; one route, chosen once.
case_the_wire_is_the_own_daemon_else_the_relay_and_only_one() {
    local out rc
    fresh_route; route_append "nfs rw,vers=3 A:/x" "nfs4 A:/x" unix:/own ssh:hub "$OK_ANSWER" >/dev/null
    [ "$(cut -d' ' -f1 "$WORK/wire.log")" = unix:/own ] || { echo "  own daemon not chosen: $(cat "$WORK/wire.log")"; return 1; }
    fresh_route; route_append "nfs rw,vers=3 A:/x" "nfs4 A:/x" "" ssh:hub "$OK_ANSWER" >/dev/null
    [ "$(cut -d' ' -f1 "$WORK/wire.log")" = ssh:hub ] || { echo "  relay not chosen: $(cat "$WORK/wire.log")"; return 1; }
    fresh_route; out="$(route_append "nfs rw,vers=3 A:/x" "nfs4 A:/x" unix:/own ssh:hub "")"; rc=$?
    [ "$rc" -eq 1 ] && [ "$out" = "the daemon did not answer at unix:/own" ] || { echo "  silence: rc $rc ($out)"; return 1; }
    [ "$(wire_count)" -eq 1 ] || { echo "  a second route was tried: $(cat "$WORK/wire.log")"; return 1; }
    fresh_route; out="$(route_append "nfs rw,vers=3 A:/x" "nfs4 A:/x" unix:/own ssh:hub \
        '{"v":1,"id":1,"kind":"res","op":"comm.file","payload":{"error":"no live session holds @t-peer","code":"no_live_session"}}')"; rc=$?
    [ "$rc" -eq 1 ] && [ "$out" = "no live session holds @t-peer" ] || { echo "  refusal: rc $rc ($out)"; return 1; }
    fresh_route; out="$(route_append "nfs rw,vers=3 A:/x" "nfs4 A:/x" "" "" "$OK_ANSWER")"; rc=$?
    [ "$rc" -eq 1 ] && contains "$out" "no daemon is reachable" || { echo "  no endpoint: rc $rc ($out)"; return 1; }
    [ "$(cat "$INBOX/t-peer.jsonl")" = '{"msg":"before"}' ] || { echo "  appended locally"; return 1; }
    return 0
}

# The frame: from/to/text and whether the line was a broadcast copy (to:"").
case_the_wire_frame_carries_the_broadcast_flag() {
    fresh_route; route_append "" - unix:/own "" "$OK_ANSWER" >/dev/null
    jq -e '.payload == {from:"t-sender",to:"t-peer",text:"/slash first",broadcast:false}' \
        <<<"$(cut -d' ' -f2- "$WORK/wire.log")" >/dev/null || { echo "  directed: $(cat "$WORK/wire.log")"; return 1; }
    fresh_route; route_append "" - unix:/own "" "$OK_ANSWER" 1 \
        '{"from":"t-sender","to":"","repo":"r","msg":"all","ts":"t"}' >/dev/null
    jq -e '.payload == {from:"t-sender",to:"t-peer",text:"all",broadcast:true}' \
        <<<"$(cut -d' ' -f2- "$WORK/wire.log")" >/dev/null || { echo "  broadcast: $(cat "$WORK/wire.log")"; return 1; }
    return 0
}

# Identity parity: the fixture set comm_inbox.rs's unit test reads, through
# the REAL identity function with findmnt pointed at each fixture.
case_the_lock_identity_matches_the_shared_fixtures() {
    local fx="$SCRIPT_DIR/fixtures/inbox-lock-identity" file path want got n=0
    command -v findmnt >/dev/null 2>&1 || { echo "  no findmnt on this box"; return 1; }
    while IFS=$'\t' read -r file path want; do
        case "$file" in ''|'#'*) continue ;; esac
        got="$(FX="$fx" MI="$fx/$file" bash -c '
            source "$1"
            _sot_findmnt() { command findmnt -F "$MI" "$@"; }
            _sot_machine_id() { local m; read -r m < "$FX/machine-id"; printf "%s" "$m"; }
            sot_inbox_lock_identity "$2"' _ "$SCRIPTS_DIR/comm-lib.sh" "$path")"
        [ "$got" = "$want" ] || { echo "  $file: got [$got], want [$want]"; return 1; }
        n=$((n + 1))
    done < "$fx/cases.tsv"
    [ "$n" -eq 7 ] || { echo "  $n fixtures, want 7"; return 1; }
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
    # The Rust filer: the same knob, the same 10, no other wait outside its
    # tests, and no lease word outside a comment.
    local rs="$SCRIPT_DIR/../../../rust/backend/src/comm_inbox.rs"
    [ "$(grep -c 'pub const INBOX_LOCK_WAIT_DEFAULT_SECS: u64 = 10;' "$rs")" -eq 1 ] \
        || { echo "  comm_inbox.rs does not default the wait to 10 exactly once"; return 1; }
    grep -q 'pub const INBOX_LOCK_WAIT_ENV: &str = "SOT_INBOX_LOCK_WAIT_SECS";' "$rs" \
        || { echo "  comm_inbox.rs does not read the one knob"; return 1; }
    sed '/^#\[cfg(test)\]/,$d' "$rs" | grep -nE 'from_secs\([0-9]' && { echo "  a wait literal in the filer"; return 1; }
    grep -vE '^[[:space:]]*//' "$rs" | grep -niE 'stale|patience|reclaim|lease' && { echo "  a lease constant is spelled in comm_inbox.rs"; return 1; }
    return 0
}

# T5 — a stub `nc` stands in for the hub on a unix: endpoint: each connection
# is one run of it. Its `comm.file` answer is the payload in $HUB/answer
# ("" = silence), given after $HUB/wait seconds; an `agent.send` is acked and
# receipted by fe@far. Every frame it reads is logged by op.
HUB="$WORK/hub"
write_hub_stub() {  # PAYLOAD [WAIT]
    rm -rf "$HUB"; mkdir -p "$HUB"
    printf '%s' "$1" > "$HUB/answer"; printf '%s' "${2:-0}" > "$HUB/wait"
    { printf '#!/bin/sh\nd=%s\n' "$HUB"; cat <<'STUB'
while IFS= read -r line; do
    case "$line" in
        *'"op":"comm.file"'*)
            printf '%s\n' "$line" >> "$d/comm-file.log"; sleep "$(cat "$d/wait")"
            [ -s "$d/answer" ] && printf '{"v":1,"id":1,"kind":"res","op":"comm.file","payload":%s}\n' "$(cat "$d/answer")"
            exit 0 ;;
        *'"op":"agent.send"'*)
            printf '%s\n' "$line" >> "$d/agent-send.log"
            id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
            printf '{"v":1,"id":1,"kind":"res","op":"agent.send","payload":{"ok":true,"receivers":["fe@far"],"id":"%s"}}\n' "$id"
            printf '{"v":1,"id":1,"kind":"evt","op":"agent.receipt","payload":{"id":"%s","filer":"fe@far"}}\n' "$id"
            exit 0 ;;
    esac
done
STUB
    } > "$HUB/nc"; chmod +x "$HUB/nc"
}
# wire_send [VAR=VALUE...] — `comm-relay.sh send @t-far`, a handle this box's
# registry does not name, so the send goes to the wire.
wire_send() {
    SEND_OUT="$(cd "$WORK" && PATH="$HUB:$PATH" SOT_COMM_SELF_FILE="$WORK/self-sender.txt" \
        SOT_COMM_TEST_HOST="$HOST_PIN" SOT_RELAY_ENDPOINT="unix:$WORK/hub.sock" \
        env "$@" "$BIN/comm-relay.sh" send @t-far "/to the far box" 2>"$WORK/err.txt")"
    SEND_RC=$?
    SEND_ERR="$(cat "$WORK/err.txt" 2>/dev/null)"
    return 0
}

# S4 — a directed wire send with no daemon found is that send's FAILED line.
case_a_wire_send_with_no_daemon_is_failed() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local out rc=0 err
    out="$(cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-sender.txt" SOT_COMM_TEST_HOST="$HOST_PIN" \
        env -u SOT_RELAY_ENDPOINT -u SOT_SOCKET "$BIN/comm-relay.sh" send @t-far "no daemon" 2>"$WORK/err.txt")" || rc=$?
    err="$(cat "$WORK/err.txt" 2>/dev/null)"
    [ "$rc" -eq 1 ] || { echo "  rc $rc, want 1 (out: $out err: $err)"; return 1; }
    contains "$err" "FAILED -> @t-far: no sotd daemon found; " || { echo "  err: $err"; return 1; }
    contains "$err$out" "ERROR:" && { echo "  an ERROR line: $err"; return 1; }
    return 0
}

# T5 on a faked Windows box: `uname` says MINGW, this box's own daemon is a
# fake `sotd.exe` under a fake LOCALAPPDATA, and a fake `powershell.exe` is
# the pipe transport (fakes from test-join-disambiguation.sh's Windows
# discovery case). It answers the connect probe, logs each oneshot's argv and
# the frames on its stdin, and answers `comm.file` with $WINHUB/answer
# ("" = silence). A copy of the scripts WITHOUT this file's endpoint stubs, so
# the real Windows discovery runs.
WINBIN="$WORK/winbin"; WINFAKE="$WORK/winfake"; WINAPP="$WORK/winappdata"; WINHUB="$WORK/winhub"
cp -r "$SCRIPTS_DIR" "$WINBIN"
mkdir -p "$WINFAKE" "$WINAPP/sot/bin" "$WINHUB"
printf '#!/bin/sh\necho "MINGW64_NT-10.0-19045"\n' > "$WINFAKE/uname"
cat > "$WINAPP/sot/bin/sotd.exe" <<'FAKESOTD'
#!/bin/sh
if [ "$1" = session-socket-path ] && [ "$2" = local ]; then printf '%s\n' '\\.\pipe\sot-fakeuser-local'; exit 0; fi
exit 1
FAKESOTD
{ printf '#!/bin/sh\nd=%s\n' "$WINHUB"; cat <<'FAKEPS'
case " $* " in *" -File "*) ;; *) exit 0 ;; esac
printf '%s\n' "$*" >> "$d/argv.log"
while IFS= read -r line; do
    printf '%s\n' "$line" >> "$d/stdin.log"
    case "$line" in
        *'"op":"hello"'*) ;;
        *'"op":"comm.file"'*)
            [ -s "$d/answer" ] && printf '{"v":1,"id":1,"kind":"res","op":"comm.file","payload":%s}\n' "$(cat "$d/answer")"
            exit 0 ;;
        *) exit 0 ;;
    esac
done
FAKEPS
} > "$WINFAKE/powershell.exe"
chmod +x "$WINFAKE/uname" "$WINFAKE/powershell.exe" "$WINAPP/sot/bin/sotd.exe"
win_send() {  # ANSWER
    rm -f "$WINHUB"/*.log; printf '%s' "$1" > "$WINHUB/answer"
    SEND_OUT="$(cd "$WORK" && unset OS OSTYPE SOT_SOCKET SOTD_BIN && PATH="$WINFAKE:$PATH" LOCALAPPDATA="$WINAPP" \
        SOT_COMM_SELF_FILE="$WORK/self-sender.txt" SOT_COMM_TEST_HOST="$HOST_PIN" SOT_SEND_TIMEOUT=3 \
        SOT_INBOX_LOCK_WAIT_SECS=1 "$WINBIN/comm-send.sh" @t-peer "/win text on stdin only" 2>"$WORK/err.txt")"
    SEND_RC=$?
    SEND_ERR="$(cat "$WORK/err.txt" 2>/dev/null)"
}

case_a_windows_send_is_one_comm_file_over_the_pipe() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local frame
    win_send '{"ok":true}'
    [ "$SEND_RC" -eq 0 ] || { echo "  ok: rc $SEND_RC (out: $SEND_OUT err: $SEND_ERR)"; return 1; }
    contains "$SEND_OUT" "filed -> @t-peer" || { echo "  ok: out: $SEND_OUT"; return 1; }
    [ ! -s "$INBOX/t-peer.jsonl" ] || { echo "  the send appended locally"; return 1; }
    frame="$(grep '"op":"comm.file"' "$WINHUB/stdin.log")"
    [ "$(printf '%s\n' "$frame" | wc -l)" -eq 1 ] || { echo "  comm.file frames: $frame"; return 1; }
    printf '%s' "$frame" | jq -e '(.payload | has("id") | not) and .payload.to == "t-peer" and .payload.text == "/win text on stdin only"' >/dev/null \
        || { echo "  the frame: $frame"; return 1; }
    grep -q 'win text on stdin only' "$WINHUB/argv.log" && { echo "  the text reached argv"; return 1; }

    win_send '{"error":"no live session holds @t-peer","code":"no_live_session"}'
    [ "$SEND_RC" -eq 1 ] || { echo "  refusal: rc $SEND_RC"; return 1; }
    contains "$SEND_ERR" "FAILED -> @t-peer: no live session holds @t-peer" || { echo "  refusal: err: $SEND_ERR"; return 1; }

    win_send ''
    [ "$SEND_RC" -eq 1 ] || { echo "  silence: rc $SEND_RC"; return 1; }
    contains "$SEND_ERR" "FAILED -> @t-peer: the daemon did not answer at pipe:" || { echo "  silence: err: $SEND_ERR"; return 1; }
    [ ! -s "$INBOX/t-peer.jsonl" ] || { echo "  a send appended locally"; return 1; }
    return 0
}

case_a_wire_send_prints_the_hubs_answer() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local answer rc want got
    while IFS='|' read -r answer rc want; do
        write_hub_stub "$answer"; wire_send
        got="$SEND_OUT$SEND_ERR"
        [ "$SEND_RC" -eq "$rc" ] && [ "$got" = "$want" ] \
            || { echo "  [$answer] rc $SEND_RC, got '$got', want $rc '$want'"; return 1; }
        [ ! -e "$HUB/agent-send.log" ] || { echo "  [$answer] fell back to agent.send"; return 1; }
        jq -e '.op == "comm.file" and .payload == {from:"t-sender",to:"t-far",text:"/to the far box",broadcast:false}' \
            "$HUB/comm-file.log" >/dev/null && [ "$(wc -l < "$HUB/comm-file.log")" -eq 1 ] \
            || { echo "  [$answer] frame: $(cat "$HUB/comm-file.log")"; return 1; }
    done <<CASES
{"ok":true}|0|filed -> @t-far
{"error":"not a handle: t-far","code":"bad_handle"}|1|FAILED -> @t-far: not a handle: t-far
{"error":"no live session holds @t-far","code":"no_live_session"}|1|FAILED -> @t-far: no live session holds @t-far
{"error":"the append failed: disk full","code":"file_failed"}|1|FAILED -> @t-far: the append failed: disk full
{"error":"unknown op: comm.file"}|1|FAILED -> @t-far: unknown op: comm.file
|1|FAILED -> @t-far: the daemon did not answer at unix:$WORK/hub.sock
CASES
    # not_here, and only it, falls back to the not-mine leg (deleted in B2).
    write_hub_stub '{"error":"no box knows that handle: t-far","code":"not_here"}'; wire_send
    [ "$SEND_RC" -eq 0 ] && [ "$SEND_OUT" = "filed -> @t-far (by fe@far, relay)" ] \
        || { echo "  not_here: rc $SEND_RC ($SEND_OUT$SEND_ERR)"; return 1; }
    [ "$(wc -l < "$HUB/agent-send.log")" -eq 1 ] || { echo "  not_here: no agent.send fallback"; return 1; }
    # The guard's own route reads the same answer with the same helper, and
    # for it not_here is FAILED: this box's registry named the handle.
    fresh_route
    got="$(route_append "nfs rw,vers=3 A:/x" "nfs4 A:/x" unix:/own "" \
        '{"v":1,"id":1,"kind":"res","op":"comm.file","payload":{"error":"no box knows that handle: t-peer","code":"not_here"}}')"; rc=$?
    [ "$rc" -eq 1 ] && [ "$got" = "no box knows that handle: t-peer" ] || { echo "  guard not_here: rc $rc ($got)"; return 1; }
    return 0
}

# The hub may wait its whole lock bound before it files; a read window no
# longer than that would call a filed line FAILED and the sender would resend
# it. A 1s lock wait and a caller's 1s send timeout: the window is still the
# lock wait plus 10s, so an answer at 2s is filed.
case_the_read_window_outlasts_the_hubs_lock_wait() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    write_hub_stub '{"ok":true}' 2
    wire_send SOT_INBOX_LOCK_WAIT_SECS=1 SOT_SEND_TIMEOUT=1
    [ "$SEND_RC" -eq 0 ] && [ "$SEND_OUT" = "filed -> @t-far" ] \
        || { echo "  rc $SEND_RC ($SEND_OUT$SEND_ERR)"; return 1; }
    return 0
}

# T6 — the hub's line (file_frame's shape, repo "daemon") and a local one.
case_a_hub_line_and_a_local_line_read_alike() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local out n
    append_one "$PEER" '{"from":"t-sender","to":"t-peer","repo":"r","msg":"local line","ts":"2026-09-30T00:00:01Z"}' \
        || { echo "  the local append failed"; return 1; }
    printf '%s\n' '{"from":"t-sender","to":"t-peer","repo":"daemon","msg":"hub line","ts":"2026-09-30T00:00:02Z"}' \
        >> "$INBOX/$PEER.jsonl"
    out="$(cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-peer.txt" SOT_COMM_TEST_HOST="$HOST_PIN" "$BIN/comm-poll.sh" 2>&1)"
    n="$(printf '%s\n' "$out" | grep -c -E '^\[2026-09-30T00:00:0[12]Z\] \[t-sender:(r|daemon)\] (local|hub) line$')"
    [ "$n" -eq 2 ] || { echo "  $n of 2 lines rendered alike: $out"; return 1; }
    out="$(cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-peer.txt" SOT_COMM_TEST_HOST="$HOST_PIN" "$BIN/comm-poll.sh" 2>&1)"
    contains "$out" "No new messages." || { echo "  the cursor did not pass both: $out"; return 1; }
    return 0
}

check "two writers through the lock give 400 whole lines" case_two_writers_give_400_whole_lines
check "a holder killed with -9 frees the lock at once and the send files" case_a_killed_holder_frees_the_lock_at_once
check "a frozen holder makes the send wait its bound and report FAILED, never filed" case_a_frozen_holder_makes_the_send_wait_then_fail
check "an append that fails under the lock is FAILED with its error" case_a_failed_write_is_failed_not_filed
check "a script whose lock identity equals the record appends locally" case_a_shared_nfs4_lock_manager_appends_locally
check "a two-line record whose line 1 matches appends locally" case_a_two_line_record_whose_line_1_matches_appends_locally
check "S2: a torn tail stays its own line and the new line stays whole" case_a_torn_tail_is_ended_before_the_new_line
check "S2: a write cut short by the file-size limit is FAILED and leaves the file byte-identical" case_a_write_cut_short_leaves_the_file_byte_identical
check "S-a: a NUL-filled tail is ended before the new line" case_a_nul_tail_is_ended_before_the_new_line
check "S4: a directed wire send with no daemon found is FAILED -> @h, exit 1" case_a_wire_send_with_no_daemon_is_failed
check "T5 (faked Windows): a send is one comm.file frame over the pipe, never a local append" case_a_windows_send_is_one_comm_file_over_the_pipe
check "v3, unknown, a mismatched export, the hub's disk over NFS, no or a none record, and no flock(1) all go to the wire" case_anything_unproven_goes_to_the_wire
check "no perl on the PATH goes to the wire, never a local append" case_no_perl_goes_to_the_wire
check "the wire is this box's daemon, else the relay; one that does not answer is FAILED with no second route" case_the_wire_is_the_own_daemon_else_the_relay_and_only_one
check "the wire frame says whether the line was a broadcast copy" case_the_wire_frame_carries_the_broadcast_flag
check "the lock identity matches the fixture set the Rust test reads" case_the_lock_identity_matches_the_shared_fixtures
check "the wait is one number, 10, in both languages, and no lease constant is spelled" case_the_wait_is_one_number_and_no_lease_survives
check "T5: a wire send is one comm.file frame and prints the hub's answer; not_here alone falls back" case_a_wire_send_prints_the_hubs_answer
check "the comm.file read window outlasts the hub's lock wait: a line filed after it is filed" case_the_read_window_outlasts_the_hubs_lock_wait
check "T6: a hub-filed and a locally-filed line read alike and both advance the cursor" case_a_hub_line_and_a_local_line_read_alike

echo "---"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
