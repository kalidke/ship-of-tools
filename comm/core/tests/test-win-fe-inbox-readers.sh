#!/usr/bin/env bash
# test-win-fe-inbox-readers.sh — on Windows a session has TWO inbox files, and
# every reader must read both (field report, 2026-09-27: a Windows session went
# an hour without seeing two messages; its fe-inbox.jsonl held four frames
# addressed to it while comm-poll.sh printed "No new messages" from a per-handle
# file three weeks stale).
#
# There is no inbox listener on Windows: the frontend files every inbound relay
# frame into its own fe-inbox.jsonl (comm-listen.sh's header, comm-watch.sh's
# Windows branch), so THAT file is the mail. Two facts make it a different file
# rather than the same one renamed, and both are asserted here:
#
#   * TWO SCHEMAS — a frontend line carries `.text`, a per-handle line `.msg`.
#   * ONE FILE PER BOX, SHARED BY EVERY HANDLE — the frontend appends every
#     frame whatever its `to`, so admission is `.to == this handle` strictly.
#     The cursor is a LINE OFFSET and the two files have unrelated line counts,
#     so the frontend cursor must be its own file.
#
# The turn-end hook is what enforces "a turn does not end while directed mail
# sits unread" — the guarantee a session relies on instead of the ping — so it
# must COUNT frontend mail, while still never advancing a cursor (only a real
# comm-poll.sh does, which is what keeps "read" an honest word).
#
# The Monitor has the MIRROR-IMAGE blind side and this suite covers it too: it
# watched only ONE file per platform, so on Windows a frame from a session on the
# SAME box — comm-send.sh:107 files it into inbox/<handle>.jsonl on every platform
# — never woke anybody, while a cross-box frame in fe-inbox.jsonl woke but was
# unreadable. Both readers and the watcher must see both files. The wake cases run
# comm-watch.sh in ONE foreground process with its poll pause stubbed, so the peer's
# append lands between two polls deterministically instead of being raced.
#
# No bats dependency. HERMETIC, same seams as test-comm-poll-cursor.sh: a temp
# $SOT_COMM_HOME, a pinned $SOT_COMM_TEST_HOST, a per-suite $SOT_COMM_SELF_FILE
# — never the real ~/.sot-comm. Windows is FAKED per invocation ($OS +
# $LOCALAPPDATA), so the same suite proves the non-Windows path unchanged.
#
# Usage: comm/core/tests/test-win-fe-inbox-readers.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/../../adapters/claude/hooks" && pwd)"
JOIN="$SCRIPTS_DIR/comm-join.sh"
POLL="$SCRIPTS_DIR/comm-poll.sh"
IDLE_HOOK="$HOOKS_DIR/comm-status-idle.sh"
WATCH="$SCRIPTS_DIR/comm-watch.sh"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-comm-win-fe-inbox-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
export SOT_COMM_HOME="$WORK/home"
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

# The hook resolves comm-context.sh, comm-status.sh and comm-lib.sh out of the
# comm home's bin/ — the flat layout update_comm deploys, where this checkout
# keeps hooks/ and core/scripts/ apart.
ln -s "$SCRIPTS_DIR" "$SOT_COMM_HOME/bin"

export SOT_COMM_TEST_HOST="testhost"
export SOT_COMM_SELF_FILE="$WORK/self.txt"
# A stable tick key: the hook's mail block is bounded to one per pending batch
# per session, and $PPID would differ on every invocation below.
export CLAUDE_CODE_SESSION_ID="win-fe-inbox-test"
unset SOT_COMM_NAME OS 2>/dev/null || true

NAME="t-winfe"
OTHER="t-someone-else"
LOCAL_APPDATA="$WORK/AppDataLocal"
FE_INBOX="$LOCAL_APPDATA/sot/fe-inbox.jsonl"
INBOX="$SOT_COMM_HOME/inbox/$NAME.jsonl"
CUR="$SOT_COMM_HOME/read/$NAME.cursor"
FE_CUR="$SOT_COMM_HOME/read/$NAME.fe.cursor"
mkdir -p "$LOCAL_APPDATA/sot"

( cd "$WORK" && "$JOIN" --name "$NAME" ) >/dev/null 2>&1 \
    || { echo "FATAL: setup join failed" >&2; exit 1; }
[ "$(cd "$WORK" && "$SCRIPTS_DIR/comm-context.sh" | sed -n 's/^NAME=//p')" = "$NAME" ] \
    || { echo "FATAL: context did not resolve the pinned handle" >&2; exit 1; }

PASS=0; FAIL=0
check() {
    local desc="$1" fn="$2"
    if "$fn"; then echo "PASS: $desc"; PASS=$((PASS + 1)); else echo "FAIL: $desc"; FAIL=$((FAIL + 1)); fi
}

reset_inboxes() {
    mkdir -p "$(dirname "$INBOX")" "$(dirname "$CUR")" "$(dirname "$FE_INBOX")"
    : > "$INBOX"; : > "$FE_INBOX"
    rm -f "${CUR:?}" "${FE_CUR:?}" "${SOT_COMM_HOME:?}"/state/mail-*.tick
}

fe_frame() {  # TO TEXT [FROM] — one FRONTEND frame: the `.text` schema, no `.msg`
    jq -nc --arg to "$1" --arg t "$2" --arg from "${3:-peer}" \
        --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        '{from:$from,to:$to,repo:"r",text:$t,ts:$ts}'
}
ph_frame() {  # TO MSG — one PER-HANDLE frame: the `.msg` schema
    jq -nc --arg to "$1" --arg m "$2" --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        '{from:"peer",to:$to,repo:"r",msg:$m,ts:$ts}'
}
fe_line() { fe_frame "$@" >> "$FE_INBOX"; }
ph_line() { ph_frame "$NAME" "$1" >> "$INBOX"; }

# The Monitor's poll pause, stubbed on $PATH: the FIRST pause is when the peer
# files its frame — a genuine append by another process between two polls, merely
# ordered instead of raced — and every later pause is real, so the loop idles
# until `timeout` ends it. This keeps the whole wake path in ONE foreground
# process: no background job, nothing left running after the suite.
STUB_DIR="$WORK/stub"; mkdir -p "$STUB_DIR"
cat > "$STUB_DIR/sleep" <<'STUBEOF'
#!/usr/bin/env bash
n=$(( $(cat "$SOT_WATCH_TEST_STATE" 2>/dev/null || echo 0) + 1 ))
printf '%s' "$n" > "$SOT_WATCH_TEST_STATE"
if [ "$n" = 1 ] && [ -n "${SOT_WATCH_TEST_LINE:-}" ]; then
    printf '%s\n' "$SOT_WATCH_TEST_LINE" >> "$SOT_WATCH_TEST_FILE"
    exit 0
fi
exec /bin/sleep "${1:-1}"
STUBEOF
chmod +x "$STUB_DIR/sleep"

wake() {  # OS FILE FRAME -> WAKE_OUT: what the Monitor emitted for FRAME appended to FILE
    rm -f "${WORK:?}/stub.count"
    WAKE_OUT="$(cd "$WORK" && OS="$1" LOCALAPPDATA="$LOCAL_APPDATA" \
        PATH="$STUB_DIR:$PATH" \
        SOT_WATCH_TEST_STATE="$WORK/stub.count" \
        SOT_WATCH_TEST_FILE="$2" SOT_WATCH_TEST_LINE="$3" \
        timeout 3 bash "$WATCH" "$NAME" 2>/dev/null || true)"
    return 0
}
wake_win() { wake Windows_NT "$@"; }
wake_nix() { wake "" "$@"; }

poll_win() {  # -> POLL_OUT / POLL_RC, as a Windows box sees it
    POLL_OUT="$(cd "$WORK" && OS=Windows_NT LOCALAPPDATA="$LOCAL_APPDATA" "$POLL" 2>&1)"
    POLL_RC=$?
    return 0
}
poll_nix() {  # -> POLL_OUT / POLL_RC, with no frontend in sight
    POLL_OUT="$(cd "$WORK" && LOCALAPPDATA="$LOCAL_APPDATA" "$POLL" 2>&1)"
    POLL_RC=$?
    return 0
}
# stop_win / stop_nix — the turn-end Stop hook for a turn ending in plain text
# (no closing marker, so the marker branch never short-circuits it). Prints the
# hook's stdout: a JSON `decision: block` when it found unread directed mail.
_stop() {  # WINFLAG
    local tr="$WORK/transcript.jsonl"
    { jq -nc '{type:"user",message:{content:"go"}}'
      jq -nc '{type:"assistant",message:{content:[{type:"text",text:"all done."}]}}'; } > "$tr"
    jq -nc --arg p "$tr" '{transcript_path:$p, stop_hook_active:false}' \
        | ( cd "$WORK" && OS="$1" LOCALAPPDATA="$LOCAL_APPDATA" bash "$IDLE_HOOK" )
}
stop_win() { _stop Windows_NT; }
stop_nix() { _stop ""; }

is_block() { [ -n "$1" ] && printf '%s' "$1" | jq -e '.decision == "block"' >/dev/null 2>&1; }

# (a) A frontend frame addressed to this handle is SHOWN by poll and COUNTED by
#     the hook — the defect itself: neither reader had ever opened this file.
case_frontend_frame_for_me_is_shown_and_counted() {
    reset_inboxes
    fe_line "$NAME" "mail from another box"
    local out; out="$(stop_win)"
    is_block "$out" || { echo "  the turn-end hook did not block on frontend mail: '$out'"; return 1; }
    printf '%s' "$out" | jq -e '.reason | test("comm-poll")' >/dev/null 2>&1 \
        || { echo "  the block does not name comm-poll: '$out'"; return 1; }
    # The hook may never mark mail read — only a real poll does.
    [ ! -f "$FE_CUR" ] \
        || { echo "  the hook advanced the frontend cursor to '$(cat "$FE_CUR")'"; return 1; }
    poll_win
    [ "$POLL_RC" -eq 0 ] || { echo "  comm-poll.sh exited $POLL_RC: $POLL_OUT"; return 1; }
    case "$POLL_OUT" in *"mail from another box"*) ;; *) echo "  poll did not show the frontend frame: $POLL_OUT"; return 1 ;; esac
    [ "$(cat "$FE_CUR" 2>/dev/null)" = "1" ] \
        || { echo "  frontend cursor is '$(cat "$FE_CUR" 2>/dev/null)', want 1"; return 1; }
    # Read once: a second turn end has nothing to hold the turn open with.
    out="$(stop_win)"
    is_block "$out" && { echo "  the hook blocked again on mail already polled: '$out'"; return 1; }
    return 0
}

# (b) The frontend inbox is SHARED, so a frame addressed to another handle on
#     this box is neither shown nor counted.
case_frontend_frame_for_another_handle_is_invisible() {
    reset_inboxes
    fe_line "$OTHER" "not for you"
    local out; out="$(stop_win)"
    is_block "$out" && { echo "  another handle's frame blocked the turn: '$out'"; return 1; }
    poll_win
    [ "$POLL_RC" -eq 0 ] || { echo "  comm-poll.sh exited $POLL_RC: $POLL_OUT"; return 1; }
    case "$POLL_OUT" in *"not for you"*) echo "  poll showed another handle's frame: $POLL_OUT"; return 1 ;; esac
    case "$POLL_OUT" in *"No new messages"*) ;; *) echo "  poll said something other than 'No new messages': $POLL_OUT"; return 1 ;; esac
    return 0
}

# (c) Two files, two unrelated line counts, two cursors: neither may be stepped
#     by the other's reader, and polling twice may not repeat a line.
case_the_two_cursors_advance_independently() {
    reset_inboxes
    ph_line "per-handle one"
    fe_line "$NAME" "frontend one"
    fe_line "$NAME" "frontend two"
    poll_win
    for want in "per-handle one" "frontend one" "frontend two"; do
        case "$POLL_OUT" in *"$want"*) ;; *) echo "  poll did not show '$want': $POLL_OUT"; return 1 ;; esac
    done
    [ "$(cut -d" " -f1 "$CUR" 2>/dev/null)" = "1" ] \
        || { echo "  per-handle cursor is '$(cat "$CUR" 2>/dev/null)', want 1"; return 1; }
    [ "$(cat "$FE_CUR" 2>/dev/null)" = "2" ] \
        || { echo "  frontend cursor is '$(cat "$FE_CUR" 2>/dev/null)', want 2"; return 1; }
    poll_win
    case "$POLL_OUT" in *"No new messages"*) ;; *) echo "  a second poll re-showed messages: $POLL_OUT"; return 1 ;; esac
    # Only the frontend file grows: only its cursor moves.
    fe_line "$NAME" "frontend three"
    poll_win
    case "$POLL_OUT" in *"frontend three"*) ;; *) echo "  poll missed the new frontend frame: $POLL_OUT"; return 1 ;; esac
    case "$POLL_OUT" in *"frontend one"*) echo "  poll re-showed an already-read frontend frame: $POLL_OUT"; return 1 ;; esac
    [ "$(cut -d" " -f1 "$CUR" 2>/dev/null)" = "1" ] \
        || { echo "  the per-handle cursor moved with the frontend file: '$(cat "$CUR" 2>/dev/null)'"; return 1; }
    [ "$(cat "$FE_CUR" 2>/dev/null)" = "3" ] \
        || { echo "  frontend cursor is '$(cat "$FE_CUR" 2>/dev/null)', want 3"; return 1; }
    return 0
}

# (d) Off Windows there is no frontend inbox: nothing about either reader changes,
#     even with the file sitting right there.
case_non_windows_reads_only_the_per_handle_inbox() {
    reset_inboxes
    ph_line "linux mail"
    fe_line "$NAME" "frontend mail"
    local out; out="$(stop_nix)"
    is_block "$out" || { echo "  per-handle mail stopped blocking the turn off Windows: '$out'"; return 1; }
    poll_nix
    [ "$POLL_RC" -eq 0 ] || { echo "  comm-poll.sh exited $POLL_RC: $POLL_OUT"; return 1; }
    case "$POLL_OUT" in *"linux mail"*) ;; *) echo "  poll did not show the per-handle line: $POLL_OUT"; return 1 ;; esac
    case "$POLL_OUT" in *"frontend mail"*) echo "  poll read the frontend inbox off Windows: $POLL_OUT"; return 1 ;; esac
    [ ! -f "$FE_CUR" ] \
        || { echo "  a frontend cursor was written off Windows: '$(cat "$FE_CUR")'"; return 1; }
    # And with ONLY frontend mail, an off-Windows turn ends and poll says nothing.
    reset_inboxes
    fe_line "$NAME" "frontend only"
    out="$(stop_nix)"
    is_block "$out" && { echo "  the hook blocked on a frontend frame off Windows: '$out'"; return 1; }
    poll_nix
    case "$POLL_OUT" in *"frontend only"*) echo "  poll read the frontend inbox off Windows: $POLL_OUT"; return 1 ;; esac
    return 0
}

# (e) A torn frontend append is skipped, never fatal, and never a boundary — the
#     same rule the per-handle inbox has, because the same failure (a frozen
#     cursor behind one bad line) would make this handle permanently deaf.
case_a_torn_frontend_line_is_not_fatal() {
    reset_inboxes
    fe_line "$NAME" "before the tear"
    printf '{"from":"peer","to":"%s","text":"half a li\n' "$NAME" >> "$FE_INBOX"
    fe_line "$NAME" "after the tear"
    poll_win
    [ "$POLL_RC" -eq 0 ] || { echo "  comm-poll.sh exited $POLL_RC on a torn frontend line: $POLL_OUT"; return 1; }
    case "$POLL_OUT" in *"before the tear"*) ;; *) echo "  the frame before the tear was not shown: $POLL_OUT"; return 1 ;; esac
    case "$POLL_OUT" in *"after the tear"*) ;; *) echo "  the frame AFTER the tear was not shown: $POLL_OUT"; return 1 ;; esac
    [ "$(cat "$FE_CUR" 2>/dev/null)" = "3" ] \
        || { echo "  frontend cursor is '$(cat "$FE_CUR" 2>/dev/null)', want 3 (the torn line counts as read)"; return 1; }
    return 0
}

# (f) THE WAKE PATH, frontend file: a cross-box frame appended while the Monitor
#     runs is emitted — one stdout line is what wakes the session.
case_a_frontend_frame_wakes_the_session() {
    reset_inboxes
    wake_win "$FE_INBOX" "$(fe_frame "$NAME" "wake me from another box")"
    case "$WAKE_OUT" in *"wake me from another box"*) ;; *) echo "  the Monitor did not wake on the frontend frame: '$WAKE_OUT'"; return 1 ;; esac
    case "$WAKE_OUT" in *"[relay] from peer"*) ;; *) echo "  the wake line lost its sender: '$WAKE_OUT'"; return 1 ;; esac
    return 0
}

# (g) The frontend file is SHARED, so a sibling handle's frame must not wake this
#     session — a wake costs a model turn.
case_another_handles_frontend_frame_does_not_wake() {
    reset_inboxes
    wake_win "$FE_INBOX" "$(fe_frame "$OTHER" "wake the other one")"
    [ -z "$WAKE_OUT" ] || { echo "  another handle's frame woke this session: '$WAKE_OUT'"; return 1; }
    return 0
}

# (h) THE SAME-BOX WAKE, which is the half the Monitor was missing: comm-send.sh
#     files a directed send into inbox/<handle>.jsonl on EVERY platform, so on
#     Windows a frame from a session on the same box has to wake through the
#     per-handle file — the one the Windows branch never watched.
case_a_same_box_frame_wakes_on_windows() {
    reset_inboxes
    wake_win "$INBOX" "$(ph_frame "$NAME" "wake me from this very box")"
    case "$WAKE_OUT" in *"wake me from this very box"*) ;; *) echo "  a same-box frame did not wake the session on Windows: '$WAKE_OUT'"; return 1 ;; esac
    return 0
}

# (i) The post-arm wake-proof. comm-listen.sh --selftest injects a __selftest__
#     frame and sot-session-start RELIES on the Monitor firing on it; comm-poll.sh
#     deliberately does the opposite and never shows it. Both halves must hold on
#     the frontend file too.
case_a_frontend_selftest_frame_still_wakes() {
    reset_inboxes
    wake_win "$FE_INBOX" "$(fe_frame "$NAME" "receive-path self-test" "__selftest__")"
    case "$WAKE_OUT" in *"receive-path self-test"*) ;; *) echo "  the frontend selftest frame did not wake the session: '$WAKE_OUT'"; return 1 ;; esac
    reset_inboxes
    fe_line "$NAME" "receive-path self-test" "__selftest__"
    poll_win
    case "$POLL_OUT" in *"receive-path self-test"*) echo "  poll showed a selftest frame as a message: $POLL_OUT"; return 1 ;; esac
    [ "$(cat "$FE_CUR" 2>/dev/null)" = "1" ] \
        || { echo "  frontend cursor is '$(cat "$FE_CUR" 2>/dev/null)', want 1 (a selftest frame still counts as read)"; return 1; }
    return 0
}

# (j) Off Windows the Monitor watches exactly what it watched before: the
#     per-handle inbox, and not the frontend file sitting right there.
case_off_windows_the_monitor_watches_only_the_per_handle_inbox() {
    reset_inboxes
    wake_nix "$INBOX" "$(ph_frame "$NAME" "linux wake")"
    case "$WAKE_OUT" in *"linux wake"*) ;; *) echo "  the Monitor stopped waking on per-handle mail off Windows: '$WAKE_OUT'"; return 1 ;; esac
    reset_inboxes
    wake_nix "$FE_INBOX" "$(fe_frame "$NAME" "frontend wake")"
    [ -z "$WAKE_OUT" ] || { echo "  the Monitor read the frontend inbox off Windows: '$WAKE_OUT'"; return 1; }
    return 0
}

check "a frontend frame addressed to this handle is shown by poll and counted by the turn-end hook" \
    case_frontend_frame_for_me_is_shown_and_counted
check "a frontend frame addressed to another handle is shown and counted by neither" \
    case_frontend_frame_for_another_handle_is_invisible
check "the per-handle and frontend cursors advance independently, and no line is shown twice" \
    case_the_two_cursors_advance_independently
check "off Windows neither reader touches the frontend inbox or its cursor" \
    case_non_windows_reads_only_the_per_handle_inbox
check "a torn frontend line is skipped, counted as read, and never fatal" \
    case_a_torn_frontend_line_is_not_fatal
check "a frontend frame appended while the Monitor runs wakes the session" \
    case_a_frontend_frame_wakes_the_session
check "another handle's frontend frame never wakes this session" \
    case_another_handles_frontend_frame_does_not_wake
check "a same-box frame in the per-handle inbox wakes the session on Windows" \
    case_a_same_box_frame_wakes_on_windows
check "a frontend __selftest__ frame wakes the Monitor and is still never shown by poll" \
    case_a_frontend_selftest_frame_still_wakes
check "off Windows the Monitor watches the per-handle inbox and nothing else" \
    case_off_windows_the_monitor_watches_only_the_per_handle_inbox

echo "---"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
