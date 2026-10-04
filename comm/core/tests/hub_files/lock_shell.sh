# Part of test-hub-files.sh, sourced by it: the shell arm under the lock (writers, killed and frozen holders, torn and NUL tails).
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
