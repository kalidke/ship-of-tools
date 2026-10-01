#!/usr/bin/env bash
# test-comm-poll-cursor.sh — the read cursor is a LINE OFFSET, and the three ways
# that could otherwise go silently deaf (messaging ruling, 2026-09-26):
#
#   1. A TORN or unparseable inbox line is skipped and counted as READ. A partial
#      append is realistic on a shared filesystem, and it used to abort
#      comm-poll.sh under `set -e`: the cursor froze, so the handle never saw
#      another message while its senders kept printing a success line.
#   2. An offset PAST the end of the inbox resets to 0. Production only appends,
#      so this means the file was cleared, truncated or restored by hand —
#      exactly when nobody suspects the cursor.
#   3. A legacy TIMESTAMP cursor converts to the count of lines BEFORE THE FIRST
#      one past it, not to "every line at or below it": stamps are only ordered
#      if every sender's clock agrees, and the old count stepped over an unread
#      frame already on disk whenever they didn't.
#
# No bats dependency. HERMETIC, same seams as test-relay-file-first.sh: a temp
# $SOT_COMM_HOME, a pinned $SOT_COMM_TEST_HOST, a per-suite $SOT_COMM_SELF_FILE
# — never the real ~/.sot-comm.
#
# Usage: comm/core/tests/test-comm-poll-cursor.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"
JOIN="$SCRIPTS_DIR/comm-join.sh"
POLL="$SCRIPTS_DIR/comm-poll.sh"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-comm-poll-cursor-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
mkdir -p "$SOT_COMM_HOME"
trap 'rm -rf "${WORK:?}"' EXIT

HOST="testhost"
NAME="t-poll"
SELF="$WORK/self.txt"
INBOX="$SOT_COMM_HOME/inbox/$NAME.jsonl"
CURSOR="$SOT_COMM_HOME/read/$NAME.cursor"

( cd "$WORK" && SOT_COMM_SELF_FILE="$SELF" SOT_COMM_TEST_HOST="$HOST" \
    "$JOIN" --name "$NAME" ) >/dev/null 2>&1 \
    || { echo "FATAL: setup join failed" >&2; exit 1; }

PASS=0; FAIL=0
check() {
    local desc="$1" fn="$2"
    if "$fn"; then echo "PASS: $desc"; PASS=$((PASS + 1)); else echo "FAIL: $desc"; FAIL=$((FAIL + 1)); fi
}

reset_inbox() { mkdir -p "$(dirname "$INBOX")" "$(dirname "$CURSOR")"; : > "$INBOX"; rm -f "${CURSOR:?}"; }
line() {  # TS MSG — one well-formed inbox line
    jq -nc --arg to "$NAME" --arg ts "$1" --arg m "$2" \
        '{from:"peer",to:$to,repo:"r",msg:$m,ts:$ts}' >> "$INBOX"
}
poll() {  # -> POLL_OUT / POLL_RC
    POLL_OUT="$(cd "$WORK" && SOT_COMM_SELF_FILE="$SELF" SOT_COMM_TEST_HOST="$HOST" "$POLL" 2>&1)"
    POLL_RC=$?
    return 0
}

case_a_torn_line_is_skipped_and_counted_as_read() {
    reset_inbox
    line "2026-01-01T00:00:01Z" "first"
    printf '{"from":"peer","to":"%s","msg":"half a li\n' "$NAME" >> "$INBOX"
    line "2026-01-01T00:00:03Z" "third"
    poll
    [ "$POLL_RC" -eq 0 ] || { echo "  comm-poll.sh exited $POLL_RC (a torn line must not be fatal): $POLL_OUT"; return 1; }
    case "$POLL_OUT" in *first*) ;; *) echo "  the line before the torn one was not shown: $POLL_OUT"; return 1 ;; esac
    case "$POLL_OUT" in *third*) ;; *) echo "  the line AFTER the torn one was not shown: $POLL_OUT"; return 1 ;; esac
    # Counted as read: the cursor reaches every line, so one bad line cannot pin
    # it and a second poll has nothing left to show.
    [ "$(cut -d" " -f1 "$CURSOR" 2>/dev/null)" = "3" ] \
        || { echo "  cursor is '$(cat "$CURSOR" 2>/dev/null)', want 3 (the torn line counts as read)"; return 1; }
    poll
    case "$POLL_OUT" in *"No new messages"*) ;; *) echo "  a second poll re-showed messages: $POLL_OUT"; return 1 ;; esac
    return 0
}

case_an_offset_past_the_end_resets() {
    reset_inbox
    line "2026-01-01T00:00:01Z" "only one"
    # The inbox was cleared/truncated/restored by hand; the cursor still names an
    # offset from the longer file it used to be.
    printf '%s' "42" > "$CURSOR"
    poll
    [ "$POLL_RC" -eq 0 ] || { echo "  comm-poll.sh exited $POLL_RC: $POLL_OUT"; return 1; }
    case "$POLL_OUT" in *"only one"*) ;; *) echo "  a stale offset silenced the inbox: $POLL_OUT"; return 1 ;; esac
    [ "$(cut -d" " -f1 "$CURSOR" 2>/dev/null)" = "1" ] \
        || { echo "  cursor is '$(cat "$CURSOR" 2>/dev/null)', want 1"; return 1; }
    return 0
}

case_legacy_cursor_stops_at_the_first_unread_stamp() {
    reset_inbox
    # A SKEWED sender: line 1 carries a LATER stamp than line 2, which is the one
    # the cursor names. "Every line at or below the cursor" counts 1 and steps
    # over line 1, which was never shown to anybody. Stopping at the first line
    # past the cursor counts 0 and shows both.
    line "2026-01-01T00:00:09Z" "from the fast clock"
    line "2026-01-01T00:00:01Z" "already read"
    printf '%s' "2026-01-01T00:00:01Z" > "$CURSOR"
    poll
    [ "$POLL_RC" -eq 0 ] || { echo "  comm-poll.sh exited $POLL_RC: $POLL_OUT"; return 1; }
    case "$POLL_OUT" in *"from the fast clock"*) ;; *) echo "  the unread out-of-order line was never shown: $POLL_OUT"; return 1 ;; esac
    [ "$(cut -d" " -f1 "$CURSOR" 2>/dev/null)" = "2" ] \
        || { echo "  cursor is '$(cat "$CURSOR" 2>/dev/null)', want 2 (migrated to a count)"; return 1; }
    return 0
}

case_legacy_cursor_covering_the_inbox_shows_nothing() {
    reset_inbox
    line "2026-01-01T00:00:01Z" "old news"
    printf '%s' "2026-01-01T00:00:05Z" > "$CURSOR"
    poll
    [ "$POLL_RC" -eq 0 ] || { echo "  comm-poll.sh exited $POLL_RC: $POLL_OUT"; return 1; }
    case "$POLL_OUT" in *"No new messages"*) ;; *) echo "  history was re-shown on migration: $POLL_OUT"; return 1 ;; esac
    return 0
}

check "a torn line is skipped, counted as read, and never fatal" case_a_torn_line_is_skipped_and_counted_as_read
check "an offset past the end of the inbox resets to 0" case_an_offset_past_the_end_resets
check "a legacy cursor stops at the first stamp past it, not at the last below it" case_legacy_cursor_stops_at_the_first_unread_stamp
check "a legacy cursor covering the inbox re-shows nothing" case_legacy_cursor_covering_the_inbox_shows_nothing

# A read that fails part-way writes no cursor: nothing is marked read that was
# not shown. A stub sed fails every line-range print (`N,Mp` / `Np`) for one
# poll; the cursor must come out byte-identical, and the next real poll shows
# every line past it.
case_a_failing_read_changes_no_cursor() {
    reset_inbox
    line "2026-01-01T00:00:01Z" "one"
    line "2026-01-01T00:00:02Z" "two"
    line "2026-01-01T00:00:03Z" "three"
    printf '%s' "2026-01-01T00:00:01Z" > "$CURSOR"
    local before real_sed; before="$(cat "$CURSOR")"; real_sed="$(command -v sed)"
    mkdir -p "$WORK/failsed"
    cat > "$WORK/failsed/sed" <<EOF
#!/bin/sh
for a in "\$@"; do
    case "\$a" in [0-9]*p) case "\${a%p}" in *[!0-9,]*) ;; *) exit 1 ;; esac ;; esac
done
exec "$real_sed" "\$@"
EOF
    chmod +x "$WORK/failsed/sed"
    POLL_OUT="$(cd "$WORK" && PATH="$WORK/failsed:$PATH" SOT_COMM_SELF_FILE="$SELF" SOT_COMM_TEST_HOST="$HOST" "$POLL" 2>&1)"
    [ "$(cat "$CURSOR")" = "$before" ] || { echo "  a failed read moved the cursor: '$before' -> '$(cat "$CURSOR")'"; return 1; }
    poll
    case "$POLL_OUT" in *two*three*) ;; *) echo "  the next poll did not show two and three: $POLL_OUT"; return 1 ;; esac
    return 0
}

check "a read that fails part-way writes no cursor" case_a_failing_read_changes_no_cursor

echo "---"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
