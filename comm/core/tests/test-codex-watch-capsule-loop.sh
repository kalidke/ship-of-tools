#!/usr/bin/env bash
# test-codex-watch-capsule-loop.sh — loop-level regressions: BOTH modes
# start at the inbox's END, ignoring any backlog or stale `.pos` file (the
# cursor is in-memory only -- catch-up is comm-poll's job); a concurrent
# inbox append is never delivered twice; a row-gone reply ends the WHOLE
# watcher; the log file stays bounded.
#
# Each case runs `_codex_watch_main` in its own `bash -c` subprocess (it
# can call `exit` without killing this suite), overriding
# `sleep`/`wc`/`sot_daemon_endpoint`/`_codex_watch_pty_input` as needed.
#
# Usage: comm/core/tests/test-codex-watch-capsule-loop.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"
WATCH="$SCRIPTS_DIR/codex-watch.sh"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-codex-watch-loop-test-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "mktemp failed" >&2; exit 1; }
trap 'rm -rf "$WORK"' EXIT

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

line() { printf '{"from":"peer","to":"me","msg":"%s"}\n' "$1"; }

case_capsule_mode_starts_at_eof_ignoring_a_stale_pos_file_and_backlog() {
    local d="$WORK/eof-capsule"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state"
    { line "one"; line "two"; line "three"; } > "$d/inbox/watchee.jsonl"
    printf '0\n' > "$d/state/codex-watch-watchee.pos"
    local attempts="$d/attempts.log"
    bash -c '
        source "'"$WATCH"'"
        export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="'"$d"'"
        sot_daemon_endpoint() { printf fixture; }
        _codex_watch_pty_input() { printf "%s" "$2" | base64 -d >> "'"$attempts"'"; printf "\n" >> "'"$attempts"'"; printf "%s" "{\"op\":\"pty.input\",\"payload\":{\"ok\":true,\"enter_sent\":true}}"; }
        turns=0
        sleep() { turns=$((turns + 1)); [ "$turns" -le 1 ] || exit 0; }
        _codex_watch_main watchee
    ' 2>/dev/null
    [ ! -f "$attempts" ] || { echo "  the pre-existing backlog was delivered (want silence -- comm-poll's job); attempts:"; cat "$attempts"; return 1; }
    return 0
}

case_concurrent_append_is_never_delivered_twice() {
    local d="$WORK/dedup"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state"
    { line "one"; line "two"; } > "$d/inbox/watchee.jsonl"
    local attempts="$d/attempts.log"
    # `wc -l` appends a THIRD line right after counting -- the race window.
    # A call-number file fires it on the SECOND (in-loop) call, not the pre-loop pos= init.
    bash -c '
        source "'"$WATCH"'"
        export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="'"$d"'"
        sot_daemon_endpoint() { printf fixture; }
        _codex_watch_pty_input() { printf "%s" "$2" | base64 -d >> "'"$attempts"'"; printf "\n" >> "'"$attempts"'"; printf "%s" "{\"op\":\"pty.input\",\"payload\":{\"ok\":true,\"enter_sent\":true}}"; }
        CALL_COUNT_FILE="'"$d"'/wc-calls"
        : > "$CALL_COUNT_FILE"
        wc() {
            if [ "$1" = "-l" ] && [ $# -eq 1 ]; then
                local tmp n callnum
                tmp="$(mktemp)"; cat > "$tmp"
                n=$(command wc -l < "$tmp")
                rm -f "$tmp"
                printf x >> "$CALL_COUNT_FILE"
                callnum=$(command wc -c < "$CALL_COUNT_FILE")
                if [ "$callnum" -eq 2 ]; then
                    printf "%s\n" "{\"from\":\"peer\",\"to\":\"me\",\"msg\":\"raced\"}" >> "'"$d"'/inbox/watchee.jsonl"
                fi
                printf "%s\n" "$n"
                return 0
            fi
            command wc "$@"
        }
        turns=0
        sleep() { turns=$((turns + 1)); [ "$turns" -le 2 ] || exit 0; }
        _codex_watch_main watchee
    ' 2>/dev/null
    local n; n="$(grep -c "peer: raced" "$attempts" 2>/dev/null || echo 0)"
    [ "$n" -eq 1 ] || { echo "  'raced' delivered $n time(s), want exactly 1"; cat "$attempts" 2>/dev/null; return 1; }
    return 0
}

case_row_gone_ends_the_whole_watcher_not_just_the_inner_loop() {
    local d="$WORK/gone"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state"
    : > "$d/inbox/watchee.jsonl"   # starts empty: the watcher begins at EOF (0)
    local sleeps="$d/sleeps.count"
    bash -c '
        source "'"$WATCH"'"
        export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="'"$d"'"
        sot_daemon_endpoint() { printf fixture; }
        _codex_watch_pty_input() { printf "%s" "{\"op\":\"pty.input\",\"payload\":{\"error\":\"unknown workspace\",\"code\":\"unknown_workspace\"}}"; }
        # A message arrives after the cursor is already at EOF (same wc-race
        # trick as the dedup case above).
        RACED_MARKER="'"$d"'/raced.marker"
        wc() {
            if [ "$1" = "-l" ] && [ $# -eq 1 ]; then
                local tmp n
                tmp="$(mktemp)"; cat > "$tmp"
                n=$(command wc -l < "$tmp")
                rm -f "$tmp"
                if [ ! -f "$RACED_MARKER" ]; then
                    : > "$RACED_MARKER"
                    printf "%s\n" "{\"from\":\"peer\",\"to\":\"me\",\"msg\":\"after-start\"}" >> "'"$d"'/inbox/watchee.jsonl"
                fi
                printf "%s\n" "$n"
                return 0
            fi
            command wc "$@"
        }
        sleep() {
            n=$(cat "'"$sleeps"'" 2>/dev/null || echo 0)
            n=$((n + 1))
            printf "%s" "$n" > "'"$sleeps"'"
            [ "$n" -le 20 ] || exit 1   # safety valve: never loop forever if the fix regresses
        }
        _codex_watch_main watchee
    ' 2>/dev/null
    local rc=$?
    [ "$rc" -eq 0 ] || { echo "  watcher did not exit cleanly (rc=$rc)"; return 1; }
    local n; n="$(cat "$sleeps" 2>/dev/null || echo 0)"
    [ "$n" -eq 1 ] || { echo "  sleep was called $n time(s); a row-gone reply on the FIRST cycle must end the outer loop too, not just the inner read"; return 1; }
    return 0
}

case_log_file_is_bounded_to_roughly_256kb() {
    local d="$WORK/logbound"; rm -rf "$d"; mkdir -p "$d/state"
    local log="$d/state/codex-watch-watchee.log"
    # 400 KiB seed, ending in a marker -- the bound must keep the TAIL.
    head -c 409600 /dev/zero | tr '\0' 'x' > "$log"
    printf 'TAIL-MARKER\n' >> "$log"
    bash -c '
        source "'"$WATCH"'"
        LOG_FILE="'"$log"'"
        _codex_watch_bound_log
    '
    local size; size=$(wc -c < "$log" 2>/dev/null || echo 0)
    [ "$size" -le 262144 ] || { echo "  log is $size bytes after bounding, want <= 262144"; return 1; }
    grep -q "TAIL-MARKER" "$log" || { echo "  the tail (most recent content) was lost, not just the head"; return 1; }
    return 0
}

case_log_file_under_the_cap_is_left_alone() {
    local d="$WORK/logbound-small"; rm -rf "$d"; mkdir -p "$d/state"
    local log="$d/state/codex-watch-watchee.log"
    printf 'small log\n' > "$log"
    bash -c '
        source "'"$WATCH"'"
        LOG_FILE="'"$log"'"
        _codex_watch_bound_log
    '
    [ "$(cat "$log")" = "small log" ] || { echo "  an under-cap log was rewritten unnecessarily"; return 1; }
    return 0
}

case_log_bound_keeps_appending_correctly_after_truncation() {
    # fd 2 opens (exec 2>>) BEFORE the file grows past the cap and BEFORE
    # the bound runs -- an mv-based bound would swap in a new inode the
    # already-open fd never sees, losing later writes.
    local d="$WORK/logbound-append"; rm -rf "$d"; mkdir -p "$d/state"
    local log="$d/state/codex-watch-watchee.log"
    bash -c '
        source "'"$WATCH"'"
        LOG_FILE="'"$log"'"
        exec 2>>"$LOG_FILE"
        big_line="$(head -c 4096 /dev/zero | tr "\0" x)"
        i=0
        while [ "$i" -lt 70 ]; do
            printf "%s\n" "$big_line" >&2
            i=$((i + 1))
        done
        _codex_watch_bound_log
        echo "POST-BOUND-MARKER" >&2
    '
    local size; size=$(wc -c < "$log" 2>/dev/null || echo 0)
    [ "$size" -lt 300000 ] || { echo "  log is $size bytes; the ~287 KiB grown-while-open was never truncated"; return 1; }
    local hits; hits=$(grep -c "POST-BOUND-MARKER" "$log" 2>/dev/null || echo 0)
    [ "$hits" -eq 1 ] || { echo "  the line written through the SAME fd after bounding appeared $hits time(s), want exactly 1"; return 1; }
    return 0
}

check "capsule mode starts at EOF, ignoring a stale .pos file and backlog" case_capsule_mode_starts_at_eof_ignoring_a_stale_pos_file_and_backlog
check "a message appended between the count and the read is delivered exactly once" case_concurrent_append_is_never_delivered_twice
check "a row-gone reply ends the outer watch loop, not just the inner read" case_row_gone_ends_the_whole_watcher_not_just_the_inner_loop
check "the watcher log is bounded to roughly 256 KiB, keeping the tail" case_log_file_is_bounded_to_roughly_256kb
check "a log file under the cap is left alone" case_log_file_under_the_cap_is_left_alone
check "the log keeps appending correctly after a bound truncates it mid-run" case_log_bound_keeps_appending_correctly_after_truncation

echo "---"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
