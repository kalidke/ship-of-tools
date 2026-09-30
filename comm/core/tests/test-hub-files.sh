#!/usr/bin/env bash
# test-hub-files.sh — the inbox append (0031 B1): one lock, both writers,
# fail-closed.
#
#   1. Two writers through `sot_inbox_append` on one inbox give every line
#      whole: none torn, none interleaved, none lost.
#      A writer killed mid-line leaves a partial that the next writer CUTS
#      back to the last newline; a reader never counts it.
#   2. The lock is the kernel's: a holder killed with -9 frees it at once, so
#      the next send files with nothing to time out; a FROZEN holder makes the
#      next send wait its bound and then report FAILED, never `filed`, and when
#      the holder resumes the inbox has no torn line.
#   3. A script appends locally only when flock(1) and perl exist, this is
#      Linux, and its lock identity for the inbox equals the daemon's record;
#      NFSv3 or an unknown mount is `none@<machine-id>`, local only against a
#      record this machine wrote. Another machine's `none@…`, a mismatched
#      export, a host mounting the daemon's local disk, no record, a bare
#      `none` record, no flock(1) and no perl all go to the wire —
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
#   8. Every inbox lock descriptor is opened read-write (an NFS client
#      refuses a shared lock without read access); the cursor hashes the
#      bytes the reader held, so a line filed after a cut-back is shown; a
#      hashed cursor one past the end steps back one; a poll shows its batch
#      after letting go of the lock, so a slow display never holds off a writer.
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
trap 'for p in "${HOLDERS[@]}"; do kill -9 "$p" 2>/dev/null; done; rm -rf "${WORK:?}"' EXIT

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
    rm -f "${SOT_COMM_HOME:?}/registry.json" "${INBOX:?}"/* "${SOT_COMM_HOME:?}"/read/*.cursor
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
    rm -f "${WORK:?}/ready"
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
    rm -f "${f:?}"
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

# S2 — a torn tail (a writer that died mid-line) is CUT back to the last
# newline, the cut is noted on the sender's stderr, and the new line is whole.
case_a_torn_tail_is_cut_before_the_new_line() {
    local out rc=0 f="$INBOX/t-torn.jsonl"
    printf '%s\n%s' '{"a":1}' '{"from":"died",' > "$f"
    out="$(append_one t-torn '{"from":"a","msg":"whole"}' 2>&1)" || rc=$?
    [ "$rc" -eq 0 ] || { echo "  rc $rc ($out)"; return 1; }
    contains "$out" "note: cut 15 bytes of an unterminated line a dead writer left in @t-torn's inbox" \
        || { echo "  no cut note: $out"; return 1; }
    [ "$(wc -l < "$f")" -eq 2 ] && [ "$(whole_lines "$f")" = 2 ] || { echo "  not two whole lines: $(cat "$f")"; return 1; }
    [ "$(sed -n 1p "$f")" = '{"a":1}' ] || { echo "  line 1 changed: $(sed -n 1p "$f")"; return 1; }
    [ "$(sed -n 2p "$f" | jq -r .msg)" = whole ] || { echo "  the new line: $(sed -n 2p "$f")"; return 1; }
    printf '%s' 'no newline at all' > "$f"
    out="$(append_one t-torn '{"from":"a","msg":"only"}' 2>&1)" || { echo "  rc: $out"; return 1; }
    [ "$(wc -l < "$f")" -eq 1 ] && [ "$(jq -r .msg "$f")" = only ] || { echo "  not cut to 0: $(cat "$f")"; return 1; }
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

# S-a — a tail of NUL bytes (NFS after a client crash) is cut like any other;
# a file of only NULs is cut to empty.
case_a_nul_tail_is_cut_before_the_new_line() {
    local out rc=0 f="$INBOX/t-nul.jsonl"
    printf '{"from":"old"}\n\0\0\0' > "$f"
    out="$(append_one t-nul '{"from":"a","msg":"whole"}' 2>&1)" || rc=$?
    [ "$rc" -eq 0 ] || { echo "  rc $rc ($out)"; return 1; }
    contains "$out" "note: cut 3 bytes" || { echo "  no cut note: $out"; return 1; }
    [ "$(whole_lines "$f")" = 2 ] || { echo "  not two whole lines: $(od -c "$f" | head -3)"; return 1; }
    printf '\0\0\0\0' > "$f"
    out="$(append_one t-nul '{"from":"a","msg":"alone"}' 2>&1)" || { echo "  rc: $out"; return 1; }
    [ "$(whole_lines "$f")" = 1 ] && [ "$(jq -r .msg "$f")" = alone ] || { echo "  NULs not cut to empty: $(od -c "$f" | head -3)"; return 1; }
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
    rm -f "${SOT_COMM_HOME:?}/inbox-lock-manager"
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
fresh_route() { rm -f "${WORK:?}/wire.log"; printf '%s\n' '{"msg":"before"}' > "$INBOX/t-peer.jsonl"; }
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

# The hub's record is two lines, its lock manager then its writer's machine id;
# a script compares line 1 only.
case_a_two_line_record_whose_line_1_matches_appends_locally() {
    local out rc
    fresh_route
    out="$(route_append "nfs4 rw,vers=4.2,local_lock=none A:/x" "nfs4 A:/x"$'\n'"m-a" unix:/own ssh:hub "$OK_ANSWER")"; rc=$?
    [ "$rc" -eq 0 ] || { echo "  rc $rc ($out)"; return 1; }
    [ "$(wire_count)" -eq 0 ] || { echo "  went to the wire"; return 1; }
    [ "$(wc -l < "$INBOX/t-peer.jsonl")" -eq 2 ] || { echo "  not appended locally"; return 1; }
    return 0
}

# An unknown lock binds the folder to the machine that wrote the record: on
# NFSv3, a record naming this machine's own `none@<machine-id>` appends
# locally under its one kernel lock (another machine's goes to the wire, below).
case_a_v3_record_naming_this_machine_appends_locally() {
    local out rc
    fresh_route
    out="$(route_append "nfs rw,vers=3 A:/x" "none@0123456789abcdef0123456789abcdef"$'\n'"0123456789abcdef0123456789abcdef" \
        unix:/own ssh:hub "$OK_ANSWER")"; rc=$?
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
nfs rw,vers=3 A:/x|none@fedcba9876543210fedcba9876543210|1
nfs rw,vers=3 A:/x|none|1
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
    rm -f "${WORK:?}/noperl"/perl*
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
    # With no machine id an unknown lock is bare `none`, which never matches.
    got="$(MI="$fx/nfs3-home.mountinfo" bash -c '
        source "$1"
        _sot_findmnt() { command findmnt -F "$MI" "$@"; }
        _sot_machine_id() { :; }
        sot_inbox_lock_identity /fixture-home' _ "$SCRIPTS_DIR/comm-lib.sh")"
    [ "$got" = none ] || { echo "  v3 with no machine id: got [$got], want [none]"; return 1; }
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
    rm -rf "${HUB:?}"; mkdir -p "$HUB"
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
    rm -f "${WINHUB:?}"/*.log; printf '%s' "$1" > "$WINHUB/answer"
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

# ---- the cut, the reader's invariant and its two guards (B1 fix-up 5) -------

POLL_OUT=""; POLL_RC=0
poll_peer() {
    POLL_OUT="$(cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-peer.txt" SOT_COMM_TEST_HOST="$HOST_PIN" \
        "$BIN/comm-poll.sh" 2>&1)"
    POLL_RC=$?
    return 0
}
peer_cursor() { cat "$SOT_COMM_HOME/read/$PEER.cursor" 2>/dev/null; }
count_of() { printf '%s\n' "$1" | grep -c -F -- "$2"; }

# The captain's test: a writer killed mid-line. A poll shows nothing new and
# leaves the cursor alone; the next real send cuts the tail and says so; the
# next poll shows the new message once and skips nothing; every line parses.
case_a_dead_writers_partial_line_is_never_counted_and_is_cut() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    run_send "@$PEER" "first"
    poll_peer
    contains "$POLL_OUT" "first" || { echo "  first not shown: $POLL_OUT"; return 1; }
    local cur; cur="$(peer_cursor)"
    start_holder 'printf "%s" "{\"from\":\"holder\"," >&8; exec sleep 60' || { echo "  holder never took the lock"; return 1; }
    kill -9 "$HOLDER"; wait "$HOLDER" 2>/dev/null
    poll_peer
    contains "$POLL_OUT" "No new messages." || { echo "  poll showed the partial: $POLL_OUT"; return 1; }
    [ "$(peer_cursor)" = "$cur" ] || { echo "  cursor moved: '$cur' -> '$(peer_cursor)'"; return 1; }
    run_send "@$PEER" "second"
    [ "$SEND_RC" -eq 0 ] || { echo "  send rc $SEND_RC: $SEND_ERR"; return 1; }
    contains "$SEND_ERR" "note: cut 17 bytes of an unterminated line a dead writer left in @$PEER's inbox" \
        || { echo "  the sender's stderr had no cut note: $SEND_ERR"; return 1; }
    poll_peer
    [ "$(count_of "$POLL_OUT" "second")" -eq 1 ] && ! contains "$POLL_OUT" "first" \
        || { echo "  second not shown exactly once: $POLL_OUT"; return 1; }
    [ "$(whole_lines "$INBOX/$PEER.jsonl")" = 2 ] || { echo "  a line does not parse: $(cat "$INBOX/$PEER.jsonl")"; return 1; }
    echo "  inbox tail (last three lines):"; tail -n 3 "$INBOX/$PEER.jsonl" | sed 's/^/    /'
    return 0
}

# Main's test: a FROZEN writer that wrote a whole line holds the lock. A poll
# and the end-of-turn hook both return within the read bound with the
# try-again line, the cursor is unchanged, and after SIGCONT every line is
# delivered exactly once.
case_a_frozen_writer_makes_a_reader_try_again_never_skip_or_hang() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    run_send "@$PEER" "one"; poll_peer
    local cur t0; cur="$(peer_cursor)"
    start_holder 'printf "%s\n" "{\"from\":\"holder\",\"to\":\"t-peer\",\"repo\":\"r\",\"msg\":\"two\",\"ts\":\"t\"}" >&8; kill -STOP $$; :' \
        || { echo "  holder never took the lock"; return 1; }
    t0=$SECONDS
    SOT_INBOX_READ_WAIT_SECS=1 poll_peer_env
    [ $((SECONDS - t0)) -le 2 ] || { echo "  poll took $((SECONDS - t0))s"; kill -CONT "$HOLDER"; return 1; }
    [ "$POLL_RC" -eq 75 ] && contains "$POLL_OUT" "the inbox for @$PEER is being written — nothing was read; run comm-poll.sh again" \
        || { echo "  rc $POLL_RC: $POLL_OUT"; kill -CONT "$HOLDER"; return 1; }
    [ "$(peer_cursor)" = "$cur" ] || { echo "  cursor moved"; kill -CONT "$HOLDER"; return 1; }
    local hout hrc
    ln -sfn "$BIN" "$SOT_COMM_HOME/bin"
    t0=$SECONDS
    hout="$(SOT_INBOX_READ_WAIT_SECS=1 idle_hook 2>&1)"; hrc=$?
    [ $((SECONDS - t0)) -le 3 ] || { echo "  hook took $((SECONDS - t0))s"; kill -CONT "$HOLDER"; return 1; }
    contains "$hout" "the inbox for @$PEER is being written; it will be checked again at the next turn end" \
        || { echo "  hook said: $hout (rc $hrc)"; kill -CONT "$HOLDER"; return 1; }
    ! contains "$hout" '"decision"' || { echo "  the hook blocked the turn on a busy inbox"; kill -CONT "$HOLDER"; return 1; }
    kill -CONT "$HOLDER"; wait "$HOLDER" 2>/dev/null
    run_send "@$PEER" "three"
    poll_peer
    [ "$(count_of "$POLL_OUT" "two")" -eq 1 ] && [ "$(count_of "$POLL_OUT" "three")" -eq 1 ] && ! contains "$POLL_OUT" "one" \
        || { echo "  not exactly once each: $POLL_OUT"; return 1; }
    return 0
}
poll_peer_env() {
    POLL_OUT="$(cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-peer.txt" SOT_COMM_TEST_HOST="$HOST_PIN" \
        "$BIN/comm-poll.sh" 2>&1)"
    POLL_RC=$?
}
idle_hook() {
    local tr="$WORK/transcript.jsonl"
    { jq -nc '{type:"user",message:{content:"go"}}'
      jq -nc '{type:"assistant",message:{content:[{type:"text",text:"all done."}]}}'; } > "$tr"
    jq -nc --arg p "$tr" '{transcript_path:$p, stop_hook_active:false}' \
        | ( cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-peer.txt" SOT_COMM_TEST_HOST="$HOST_PIN" \
            CLAUDE_CODE_SESSION_ID="hub-files-test" bash "$SCRIPT_DIR/../../adapters/claude/hooks/comm-status-idle.sh" )
}

# The stubbed fsync failure, shell arm. A test-only perl module (loaded by
# PERL5OPT, production code has no hook) makes IO::Handle::sync start a reader,
# wait 0.5 s and fail, so the perl program cuts the in-flight line back.
# $1 = locked: the reader is comm-poll.sh as-is (it waits on the shared lock);
# $1 = unlocked: its PATH has no flock(1), so it counts the in-flight line.
stub_fsync_append() {  # MODE — appends "inflight" to the peer's inbox, reader output in $WORK/reader.out
    local mode="$1" rp="$PATH"
    mkdir -p "$WORK/perlstub"
    cat > "$WORK/perlstub/StubSync.pm" <<'PM'
package StubSync;
use IO::Handle;
{ no warnings 'redefine';
  *IO::Handle::sync = sub {
      system("bash -c '\"\$STUB_READER\"' >\"\$STUB_OUT\" 2>&1 9>&- &");
      select(undef, undef, undef, 0.5);
      $! = 5; return undef;
  };
}
1;
PM
    cat > "$WORK/reader.sh" <<RD
#!/usr/bin/env bash
cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-peer.txt" SOT_COMM_TEST_HOST="$HOST_PIN" SOT_INBOX_READ_WAIT_SECS=5 \
    PATH="\${READER_PATH:-\$PATH}" "$BIN/comm-poll.sh"
echo "rc=\$?"
RD
    chmod +x "$WORK/reader.sh"
    [ "$mode" = unlocked ] && rp="$WORK/noflock"
    rm -f "${WORK:?}/reader.out"
    printf '%s\n' '{"from":"t-sender","to":"t-peer","repo":"r","msg":"inflight","ts":"t"}' \
        | STUB_READER="$WORK/reader.sh" STUB_OUT="$WORK/reader.out" READER_PATH="$rp" \
          PERL5LIB="$WORK/perlstub" PERL5OPT="-MStubSync" \
          bash -c 'source "$1/comm-lib.sh"; sot_inbox_append "$2"' _ "$BIN" "$PEER" >"$WORK/stubappend.out" 2>&1
    STUB_RC=$?
    sleep 1   # the reader is detached; give it its second
    return 0
}
make_noflock() {
    local d
    mkdir -p "$WORK/noflock"
    for d in ${PATH//:/ }; do ln -s "$d"/* "$WORK/noflock/" 2>/dev/null; done
    rm -f "${WORK:?}/noflock"/flock
}

case_a_reader_on_the_shared_lock_never_counts_a_line_that_is_cut_back() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    run_send "@$PEER" "one"; poll_peer
    stub_fsync_append locked
    [ "$STUB_RC" -eq 1 ] || { echo "  the stubbed append did not fail (rc $STUB_RC): $(cat "$WORK/stubappend.out")"; return 1; }
    local out; out="$(cat "$WORK/reader.out" 2>/dev/null)"
    contains "$out" "No new messages." && ! contains "$out" inflight && contains "$out" "rc=0" \
        || { echo "  the locked reader: $out"; return 1; }
    [ "$(whole_lines "$INBOX/$PEER.jsonl")" = 1 ] || { echo "  the in-flight line was not cut back"; return 1; }
    run_send "@$PEER" "next"; poll_peer
    [ "$(count_of "$POLL_OUT" "next")" -eq 1 ] || { echo "  next not shown once: $POLL_OUT"; return 1; }
    ! contains "$POLL_OUT" "cut back" || { echo "  a needless step-back note: $POLL_OUT"; return 1; }
    return 0
}

case_an_unlocked_reader_steps_back_one_line_after_a_cut_back() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    make_noflock
    run_send "@$PEER" "one"; poll_peer
    stub_fsync_append unlocked
    [ "$STUB_RC" -eq 1 ] || { echo "  the stubbed append did not fail (rc $STUB_RC)"; return 1; }
    local out; out="$(cat "$WORK/reader.out" 2>/dev/null)"
    contains "$out" inflight || { echo "  the unlocked reader did not count the in-flight line: $out"; return 1; }
    [ "$(whole_lines "$INBOX/$PEER.jsonl")" = 1 ] || { echo "  the in-flight line was not cut back"; return 1; }
    run_send "@$PEER" "real"; poll_peer
    [ "$(count_of "$POLL_OUT" "real")" -eq 1 ] || { echo "  real not shown exactly once (nothing skipped): $POLL_OUT"; return 1; }
    contains "$POLL_OUT" "the last line read from @$PEER's inbox was cut back; reading from the line before it" \
        || { echo "  no step-back note: $POLL_OUT"; return 1; }
    ! contains "$POLL_OUT" "one" || { echo "  a step-back of more than one line: $POLL_OUT"; return 1; }
    return 0
}

# The cursor's format: `<count>` (every old file) still works, a legacy ts
# cursor still migrates, a hash mismatch steps back exactly one line.
case_the_cursor_takes_a_bare_count_a_ts_and_a_hash_and_steps_back_one() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local f="$INBOX/$PEER.jsonl" cur="$SOT_COMM_HOME/read/$PEER.cursor"
    mkdir -p "$SOT_COMM_HOME/read"
    printf '%s\n' '{"from":"a","to":"t-peer","msg":"m1","ts":"2026-01-01T00:00:01Z"}' \
        '{"from":"a","to":"t-peer","msg":"m2","ts":"2026-01-01T00:00:02Z"}' \
        '{"from":"a","to":"t-peer","msg":"m3","ts":"2026-01-01T00:00:03Z"}' > "$f"
    printf '2' > "$cur"; poll_peer
    [ "$(count_of "$POLL_OUT" m3)" -eq 1 ] && ! contains "$POLL_OUT" m2 || { echo "  bare count: $POLL_OUT"; return 1; }
    [ "$(peer_cursor | cut -d' ' -f1)" = 3 ] && [ -n "$(peer_cursor | cut -d' ' -f2)" ] || { echo "  cursor not '3 <hash>': $(peer_cursor)"; return 1; }
    printf '2026-01-01T00:00:01Z' > "$cur"; poll_peer
    [ "$(count_of "$POLL_OUT" m2)" -eq 1 ] && [ "$(count_of "$POLL_OUT" m3)" -eq 1 ] && ! contains "$POLL_OUT" m1 \
        || { echo "  ts cursor: $POLL_OUT"; return 1; }
    # a hash naming a line that is no longer line 3: one step back, not two
    printf '3 1-1' > "$cur"; poll_peer
    contains "$POLL_OUT" "was cut back" && [ "$(count_of "$POLL_OUT" m3)" -eq 1 ] && ! contains "$POLL_OUT" m2 \
        || { echo "  hash mismatch: $POLL_OUT"; return 1; }
    return 0
}

# B-1: the Linux NFS client refuses a shared lock on a descriptor without read
# access, and a local disk never says so — so the test reads the open mode. A
# wrapper `flock` records the flags of its inherited fd 9; the low two bits of
# the last octal digit are the access mode (2 = O_RDWR).
case_every_inbox_lock_descriptor_is_read_write() {
    local real fl line n=0 d
    real="$(command -v flock)"
    mkdir -p "$WORK/fbin9"
    printf '#!/bin/sh\necho "$* $(grep flags /proc/self/fdinfo/9)" >> "%s/flock9.log"\nexec %s "$@"\n' "$WORK" "$real" > "$WORK/fbin9/flock"
    chmod +x "$WORK/fbin9/flock"
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    rm -f "${WORK:?}/flock9.log"
    PATH="$WORK/fbin9:$PATH" append_one "$PEER" '{"from":"a","to":"'"$PEER"'","repo":"r","msg":"w","ts":"t"}' >/dev/null \
        || { echo "  the append refused"; return 1; }
    PATH="$WORK/fbin9:$PATH" poll_peer
    contains "$POLL_OUT" '"msg"' || contains "$POLL_OUT" w || { echo "  the poll showed nothing: $POLL_OUT"; return 1; }
    grep -q -e '-x' "$WORK/flock9.log" && grep -q -e '-s' "$WORK/flock9.log" \
        || { echo "  want one writer and one reader flock: $(tr '\n' '|' < "$WORK/flock9.log")"; return 1; }
    while IFS= read -r line; do
        fl="${line##*flags:}"; fl="${fl//[[:space:]]/}"
        d="${fl: -1}"
        [ -n "$fl" ] && [ $((d & 3)) -eq 2 ] || { echo "  fd 9 not O_RDWR: $line"; return 1; }
        n=$((n + 1))
    done < "$WORK/flock9.log"
    [ "$n" -ge 2 ] || { echo "  only $n flock calls logged"; return 1; }
    return 0
}

# B-2 at function level: a reader showed in-flight line 3, the writer cut it
# back, a new line filed in its place. The cursor is written from the bytes the
# reader HELD, so the next offset steps back one and the new line is shown.
case_a_cursor_hashes_the_line_it_read_not_the_one_filed_after() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local f="$INBOX/$PEER.jsonl" held out
    mkdir -p "$SOT_COMM_HOME/read"
    printf '%s\n' '{"from":"a","to":"t-peer","msg":"m1","ts":"1"}' '{"from":"a","to":"t-peer","msg":"m2","ts":"2"}' \
        '{"from":"a","to":"t-peer","msg":"inflight","ts":"3"}' > "$f"
    held="$(sed -n 3p "$f")"
    head -n 2 "$f" > "$f.cut" && mv "$f.cut" "$f"
    printf '%s\n' '{"from":"a","to":"t-peer","msg":"new","ts":"4"}' >> "$f"
    out="$(bash -c 'source "$1/comm-lib.sh"; sot_cursor_write "$2" 3 "$3"; sot_cursor_offset "$2"' _ "$BIN" "$PEER" "$held" 2>&1)"
    [ "${out##*$'\n'}" = 2 ] && contains "$out" "was cut back" || { echo "  offset after the cut: $out"; return 1; }
    poll_peer
    [ "$(count_of "$POLL_OUT" new)" -eq 1 ] && ! contains "$POLL_OUT" m2 || { echo "  the new line was not shown once: $POLL_OUT"; return 1; }
    return 0
}

# S-1: a hashed cursor exactly one past the end is a cut-back with nothing new:
# step back one. Anything further past the end, or a bare count, is 0.
case_a_cursor_one_past_the_end_steps_back_one_and_further_gives_zero() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local f="$INBOX/$PEER.jsonl" cur="$SOT_COMM_HOME/read/$PEER.cursor" out
    mkdir -p "$SOT_COMM_HOME/read"
    printf '%s\n' '{"from":"a","to":"t-peer","msg":"m1","ts":"1"}' '{"from":"a","to":"t-peer","msg":"m2","ts":"2"}' > "$f"
    printf '3 1-1' > "$cur"
    out="$(bash -c 'source "$1/comm-lib.sh"; sot_cursor_offset "$2"' _ "$BIN" "$PEER" 2>&1)"
    [ "${out##*$'\n'}" = 2 ] && contains "$out" "was cut back" || { echo "  3 <hash> over 2 lines: $out"; return 1; }
    printf '5 1-1' > "$cur"
    out="$(bash -c 'source "$1/comm-lib.sh"; sot_cursor_offset "$2"' _ "$BIN" "$PEER" 2>&1)"
    [ "$out" = 0 ] || { echo "  5 <hash> over 2 lines: $out"; return 1; }
    printf '3' > "$cur"
    out="$(bash -c 'source "$1/comm-lib.sh"; sot_cursor_offset "$2"' _ "$BIN" "$PEER" 2>&1)"
    [ "$out" = 0 ] || { echo "  a bare 3 over 2 lines: $out"; return 1; }
    return 0
}

# S-2: a poll shows its batch AFTER letting go of the lock. Behind a jq that
# sleeps 50 ms per call, a 100-line backlog takes ~25 s to show; a writer's
# append must still file within a second while it does.
case_a_slow_display_does_not_hold_off_a_writer() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local i pid t0 ms real
    real="$(command -v jq)"
    mkdir -p "$WORK/slowjq"
    printf '#!/bin/sh\necho x >> "%s/jq.calls"\nsleep 0.05\nexec %s "$@"\n' "$WORK" "$real" > "$WORK/slowjq/jq"
    chmod +x "$WORK/slowjq/jq"
    for i in $(seq 100); do
        printf '{"from":"a","to":"t-peer","repo":"r","msg":"m%s","ts":"t"}\n' "$i"
    done > "$INBOX/$PEER.jsonl"
    rm -f "${WORK:?}/jq.calls"
    ( cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-peer.txt" SOT_COMM_TEST_HOST="$HOST_PIN" PATH="$WORK/slowjq:$PATH" \
        "$BIN/comm-poll.sh" >/dev/null 2>&1 ) &
    pid=$!
    i=0
    while [ "$( { wc -l < "$WORK/jq.calls"; } 2>/dev/null || echo 0)" -lt 8 ] && [ "$i" -lt 100 ]; do sleep 0.05; i=$((i + 1)); done
    [ "$( { wc -l < "$WORK/jq.calls"; } 2>/dev/null || echo 0)" -ge 8 ] || { kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null; echo "  the poll never reached its display"; return 1; }
    t0="$(date +%s%N)"
    append_one "$PEER" '{"from":"a","to":"'"$PEER"'","repo":"r","msg":"late","ts":"t"}' >/dev/null
    local rc=$?
    ms=$(( ($(date +%s%N) - t0) / 1000000 ))
    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
    echo "  the append filed ${ms} ms into a slow display"
    [ "$rc" -eq 0 ] && [ "$ms" -lt 1000 ] || { echo "  the append rc $rc after ${ms} ms"; return 1; }
    return 0
}

# The wait is chosen by lock kind: under `nfs4 …` the append POLLS (`flock -n`,
# every 15-25 ms) and follows a release within a retry; under `local …` or
# `none@…` it BLOCKS (`flock -w`), as before. A wrapper logs each flock call.
case_the_lock_wait_is_chosen_by_lock_kind() {
    local mid=0123456789abcdef0123456789abcdef kind mnt rec want t0 ms real
    real="$(command -v flock)"
    mkdir -p "$WORK/fbin"
    printf '#!/bin/sh\necho "$*" >> "%s/flock.log"\nexec %s "$@"\n' "$WORK" "$real" > "$WORK/fbin/flock"
    chmod +x "$WORK/fbin/flock"
    for kind in nfs4 local none; do
        case "$kind" in
            nfs4)  mnt="nfs4 rw,vers=4.2,local_lock=none filer.example:/export/home"; rec="$RECORD"; want=poll ;;
            local) mnt="ext4 rw /dev/sda1"; rec="local $mid"; want=block ;;
            none)  mnt="nfs rw,vers=3 filer.example:/export/home"; rec="none@$mid"; want=block ;;
        esac
        setup_rows || return 1
        printf '%s\n' "$rec" > "$SOT_COMM_HOME/inbox-lock-manager"
        rm -f "${WORK:?}/flock.log"
        start_holder 'exec sleep 0.3' || { echo "  no holder"; return 1; }
        t0="$(date +%s%N)"
        if ! FAKE_MNT="$mnt" PATH="$WORK/fbin:$PATH" append_one "$PEER" '{"from":"a","to":"'"$PEER"'","repo":"r","msg":"w","ts":"t"}' >/dev/null; then
            echo "  $kind: append refused"
            printf '%s\n' "$RECORD" > "$SOT_COMM_HOME/inbox-lock-manager"
            return 1
        fi
        ms=$(( ($(date +%s%N) - t0) / 1000000 ))
        printf '%s\n' "$RECORD" > "$SOT_COMM_HOME/inbox-lock-manager"
        echo "  $kind: filed ${ms} ms after start, flock calls: $(tr '\n' '|' < "$WORK/flock.log")"
        [ "$ms" -lt 450 ] || { echo "  $kind: took ${ms} ms, more than a retry after the 300 ms release"; return 1; }
        case "$want" in
            poll)  grep -q -e '-n ' "$WORK/flock.log" && ! grep -q -e '-w ' "$WORK/flock.log" || { echo "  $kind should poll"; return 1; } ;;
            block) grep -q -e '-w ' "$WORK/flock.log" && ! grep -q -e '-n ' "$WORK/flock.log" || { echo "  $kind should block"; return 1; } ;;
        esac
        [ "$(whole_lines "$INBOX/$PEER.jsonl")" = 1 ] || { echo "  $kind: inbox not one whole line"; return 1; }
    done
    return 0
}

check "two writers through the lock give 400 whole lines" case_two_writers_give_400_whole_lines
check "a holder killed with -9 frees the lock at once and the send files" case_a_killed_holder_frees_the_lock_at_once
check "a frozen holder makes the send wait its bound and report FAILED, never filed" case_a_frozen_holder_makes_the_send_wait_then_fail
check "an append that fails under the lock is FAILED with its error" case_a_failed_write_is_failed_not_filed
check "a script whose lock identity equals the record appends locally" case_a_shared_nfs4_lock_manager_appends_locally
check "a two-line record whose line 1 matches appends locally" case_a_two_line_record_whose_line_1_matches_appends_locally
check "S2: a torn tail is cut back to the last newline and the new line is whole" case_a_torn_tail_is_cut_before_the_new_line
check "S2: a write cut short by the file-size limit is FAILED and leaves the file byte-identical" case_a_write_cut_short_leaves_the_file_byte_identical
check "S-a: a NUL-filled tail is cut, and a file of only NULs is cut to empty" case_a_nul_tail_is_cut_before_the_new_line
check "S4: a directed wire send with no daemon found is FAILED -> @h, exit 1" case_a_wire_send_with_no_daemon_is_failed
check "T5 (faked Windows): a send is one comm.file frame over the pipe, never a local append" case_a_windows_send_is_one_comm_file_over_the_pipe
check "a v3 record naming this machine's own none@<machine-id> appends locally" case_a_v3_record_naming_this_machine_appends_locally
check "v3 against another machine's record, unknown, a mismatched export, the hub's disk over NFS, no or a bare none record, and no flock(1) all go to the wire" case_anything_unproven_goes_to_the_wire
check "no perl on the PATH goes to the wire, never a local append" case_no_perl_goes_to_the_wire
check "the wire is this box's daemon, else the relay; one that does not answer is FAILED with no second route" case_the_wire_is_the_own_daemon_else_the_relay_and_only_one
check "the wire frame says whether the line was a broadcast copy" case_the_wire_frame_carries_the_broadcast_flag
check "the lock identity matches the fixture set the Rust test reads" case_the_lock_identity_matches_the_shared_fixtures
check "the wait is one number, 10, in both languages, and no lease constant is spelled" case_the_wait_is_one_number_and_no_lease_survives
check "T5: a wire send is one comm.file frame and prints the hub's answer; not_here alone falls back" case_a_wire_send_prints_the_hubs_answer
check "the comm.file read window outlasts the hub's lock wait: a line filed after it is filed" case_the_read_window_outlasts_the_hubs_lock_wait
check "T6: a hub-filed and a locally-filed line read alike and both advance the cursor" case_a_hub_line_and_a_local_line_read_alike

check "a dead writer's partial line is never counted; the next send cuts it and says so; nothing is skipped" case_a_dead_writers_partial_line_is_never_counted_and_is_cut
check "a frozen writer makes a poll and the end-of-turn hook say try again within the bound; nothing is skipped" case_a_frozen_writer_makes_a_reader_try_again_never_skip_or_hang
check "stubbed fsync failure (shell arm), locked reader: waits, counts nothing, skips nothing" case_a_reader_on_the_shared_lock_never_counts_a_line_that_is_cut_back
check "stubbed fsync failure (shell arm), unlocked reader: steps back one line and skips nothing" case_an_unlocked_reader_steps_back_one_line_after_a_cut_back
check "the cursor takes a bare count, a ts and a hash, and a mismatch steps back exactly one line" case_the_cursor_takes_a_bare_count_a_ts_and_a_hash_and_steps_back_one

check "B-1: the reader's and the writer's lock descriptor are both opened read-write" case_every_inbox_lock_descriptor_is_read_write
check "B-2: the cursor hashes the line the reader held; a line filed after a cut-back is shown" case_a_cursor_hashes_the_line_it_read_not_the_one_filed_after
check "S-1: a hashed cursor one past the end steps back one; further past, or a bare count, gives 0" case_a_cursor_one_past_the_end_steps_back_one_and_further_gives_zero
check "S-2: a slow display does not hold off a writer" case_a_slow_display_does_not_hold_off_a_writer

check "the lock wait is chosen by lock kind: nfs4 polls, local and none@ block, both follow a release" case_the_lock_wait_is_chosen_by_lock_kind

echo "---"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
