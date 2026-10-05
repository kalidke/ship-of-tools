# Part of test-hub-files.sh, sourced by it: the cut, the reader's invariant and its two guards, the stubbed fsync failure.
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
    [ $((SECONDS - t0)) -lt 10 ] || { echo "  poll took $((SECONDS - t0))s"; kill -CONT "$HOLDER"; return 1; }
    [ "$POLL_RC" -eq 75 ] && contains "$POLL_OUT" "the inbox for @$PEER is being written — nothing was read; run comm-poll.sh again" \
        || { echo "  rc $POLL_RC: $POLL_OUT"; kill -CONT "$HOLDER"; return 1; }
    [ "$(peer_cursor)" = "$cur" ] || { echo "  cursor moved"; kill -CONT "$HOLDER"; return 1; }
    local hout hrc
    ln -sfn "$BIN" "$SOT_COMM_HOME/bin"
    t0=$SECONDS
    hout="$(SOT_INBOX_READ_WAIT_SECS=1 idle_hook 2>&1)"; hrc=$?
    [ $((SECONDS - t0)) -lt 10 ] || { echo "  hook took $((SECONDS - t0))s"; kill -CONT "$HOLDER"; return 1; }
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
# idle_hook [TEXT] [STOP_HOOK_ACTIVE] — the peer's Stop hook for a turn that
# ends in TEXT (default a plain "all done."), as session $HOOK_SESSION.
# idle_hook_over TRANSCRIPT [STOP_HOOK_ACTIVE] — the same over a transcript
# the case wrote.
idle_hook() {
    local tr="$WORK/transcript.jsonl"
    { jq -nc '{type:"user",message:{content:"go"}}'
      jq -nc --arg t "${1:-all done.}" '{type:"assistant",message:{content:[{type:"text",text:$t}]}}'; } > "$tr"
    idle_hook_over "$tr" "${2:-false}"
}
idle_hook_over() {
    jq -nc --arg p "$1" --argjson a "${2:-false}" '{transcript_path:$p, stop_hook_active:$a}' \
        | ( cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-peer.txt" SOT_COMM_TEST_HOST="$HOST_PIN" \
            CLAUDE_CODE_SESSION_ID="${HOOK_SESSION:-hub-files-test}" bash "$SCRIPT_DIR/../work_state/hooks/comm-status-idle.sh" )
}

# The stubbed fsync failure, shell arm. A test-only perl module (loaded by
# PERL5OPT, production code has no hook) makes IO::Handle::sync start a reader,
# wait until $STUB_GO exists and fail, so the perl program cuts the in-flight line
# back. The reader gives the go itself, where the case needs it: unlocked, after it
# has read and counted the in-flight line; locked, at its first shared-lock try (it
# is then waiting on the writer's lock). The case waits for the reader's `rc=` line,
# so no fixed wait stands in for either.
# $1 = locked: the reader is comm-poll.sh as-is (it waits on the shared lock);
# $1 = unlocked: the reader alone sees an empty mount (FAKE_MNT=""), so its lock
# identity is none@<machine-id>, not the record's: the lock is not ours, it takes
# none, and it counts the in-flight line. The writer stays ours.
stub_fsync_append() {  # MODE — appends "inflight" to the peer's inbox, reader output in $WORK/reader.out
    local mode="$1" rp="$PATH"
    mkdir -p "$WORK/perlstub"
    cat > "$WORK/perlstub/StubSync.pm" <<'PM'
package StubSync;
use IO::Handle;
{ no warnings 'redefine';
  *IO::Handle::sync = sub {
      system("bash -c '\"\$STUB_READER\"' >\"\$STUB_OUT\" 2>&1 9>&- &");
      for (1 .. 600) { last if -e $ENV{STUB_GO}; select(undef, undef, undef, 0.05); }
      $! = 5; return undef;
  };
}
1;
PM
    cat > "$WORK/reader.sh" <<RD
#!/usr/bin/env bash
[ -z "\${READER_UNLOCKED:-}" ] || export FAKE_MNT=
# The stub is the append's alone: the poll's own registry write fsyncs through
# perl too, and must not start a second reader over this one's output.
unset PERL5LIB PERL5OPT STUB_READER STUB_OUT
cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-peer.txt" SOT_COMM_TEST_HOST="$HOST_PIN" SOT_INBOX_READ_WAIT_SECS=5 \
    PATH="\${READER_PATH:-\$PATH}" "$BIN/comm-poll.sh"
echo "rc=\$?"
[ -z "\${READER_UNLOCKED:-}" ] || : > "$WORK/stub-go"
RD
    chmod +x "$WORK/reader.sh"
    local unl=""; [ "$mode" = unlocked ] && unl=1
    if [ "$mode" = locked ]; then
        mkdir -p "$WORK/goflock"
        printf '#!/bin/sh\ncase " $* " in *" -s "*) : > "%s/stub-go" ;; esac\nexec %s "$@"\n' "$WORK" "$(command -v flock)" > "$WORK/goflock/flock"
        chmod +x "$WORK/goflock/flock"
        rp="$WORK/goflock:$rp"
    fi
    rm -f "${WORK:?}/reader.out" "${WORK:?}/stub-go"
    printf '%s\n' '{"from":"t-sender","to":"t-peer","repo":"r","msg":"inflight","ts":"t"}' \
        | STUB_READER="$WORK/reader.sh" STUB_OUT="$WORK/reader.out" STUB_GO="$WORK/stub-go" READER_PATH="$rp" READER_UNLOCKED="$unl" \
          PERL5LIB="$WORK/perlstub" PERL5OPT="-MStubSync" \
          bash -c 'source "$1/comm-lib.sh"; sot_inbox_append "$2"' _ "$BIN" "$PEER" >"$WORK/stubappend.out" 2>&1
    STUB_RC=$?
    local i
    for i in $(seq 600); do grep -q '^rc=' "$WORK/reader.out" 2>/dev/null && return 0; sleep 0.05; done
    echo "  the detached reader never finished: $(cat "$WORK/reader.out" 2>/dev/null)"
    return 1
}

case_a_reader_on_the_shared_lock_never_counts_a_line_that_is_cut_back() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    run_send "@$PEER" "one"; poll_peer
    stub_fsync_append locked || return 1
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
    # The reader's identity differs from the record and the writer's does not,
    # with flock on PATH for both: the reader really is on the unlocked path.
    [ -z "$(FAKE_MNT= bash -c 'source "$1/comm-lib.sh"; _sot_inbox_lock_ours "$2"' _ "$BIN" "$INBOX")" ] \
        || { echo "  the reader's lock is still ours"; return 1; }
    [ -n "$(bash -c 'source "$1/comm-lib.sh"; _sot_inbox_lock_ours "$2"' _ "$BIN" "$INBOX")" ] \
        || { echo "  the writer's lock is not ours"; return 1; }
    run_send "@$PEER" "one"; poll_peer
    stub_fsync_append unlocked || return 1
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
# append must still file while it does, not after the poll's display (its 10 s wait is the bound).
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
    ( cd "$WORK" && exec env SOT_COMM_SELF_FILE="$WORK/self-peer.txt" SOT_COMM_TEST_HOST="$HOST_PIN" PATH="$WORK/slowjq:$PATH" \
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
    [ "$rc" -eq 0 ] && [ "$ms" -lt 10000 ] || { echo "  the append rc $rc after ${ms} ms"; return 1; }
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
        # The flock log below tells polling from blocking. The bound is the 10 s
        # wait a missed wake would run out, which only a hang reaches.
        [ "$ms" -lt 10000 ] || { echo "  $kind: took ${ms} ms, past the 10 s wait, after the 300 ms release"; return 1; }
        case "$want" in
            poll)  grep -q -e '-n ' "$WORK/flock.log" && ! grep -q -e '-w ' "$WORK/flock.log" || { echo "  $kind should poll"; return 1; } ;;
            block) grep -q -e '-w ' "$WORK/flock.log" && ! grep -q -e '-n ' "$WORK/flock.log" || { echo "  $kind should block"; return 1; } ;;
        esac
        [ "$(whole_lines "$INBOX/$PEER.jsonl")" = 1 ] || { echo "  $kind: inbox not one whole line"; return 1; }
    done
    return 0
}
