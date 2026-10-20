#!/usr/bin/env bash
# test-send-file.sh — `comm-send.sh @h --file PATH` (and `--file -`) delivers the
# file's bytes exactly, so a message with backquotes, `$( )`, quotes, backslashes
# and newlines needs no shell quoting from the caller (COMM-SEND-FILE).
#
#   T1. --file PATH: the newest inbox record's msg is the file's bytes, byte for
#       byte, for bodies with backquotes, `$( )`, both quote kinds, a backslash,
#       embedded blank lines, and trailing newlines or none.
#   T2. --file -: the same bytes arrive from stdin.
#   T3. A missing file is refused with one line, exit 1, and nothing is appended.
#   T4. --file with text beside it is refused, and nothing is appended.
#   T5. --file with no path is refused, and nothing is appended.
#   T6. An empty file is refused, and nothing is appended.
#
# No bats dependency. HERMETIC, same seams as test-send-routes-to-relay.sh: a
# temp $SOT_COMM_HOME, a per-case $SOT_COMM_SELF_FILE, a pinned host, and a copy
# of the scripts whose comm-relay.sh is a recorder, never the real relay.
#
# Usage: comm/tests/test-send-file.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home
. "$(dirname "${BASH_SOURCE[0]}")/lib-wait.sh"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-comm-send-file-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
# The record a daemon writes at startup (see test-send-routes-to-relay.sh).
mkdir -p "$SOT_COMM_HOME/inbox"
# A fake findmnt reports a local filesystem, so every script's identity is local.
mkdir -p "$WORK/findmnt-bin"
printf '#!/bin/sh\necho "ext4 rw,relatime /dev/fake"\n' > "$WORK/findmnt-bin/findmnt"
chmod +x "$WORK/findmnt-bin/findmnt"
export PATH="$WORK/findmnt-bin:$PATH"
bash -c 'source "$1"; sot_inbox_lock_identity "$INBOX_DIR"' _ "$SCRIPTS_DIR/comm-lib.sh" > "$SOT_COMM_HOME/inbox-lock-manager"
mkdir -p "$SOT_COMM_HOME"
trap 'rm -rf "${WORK:?}"' EXIT

# The scripts under test, with the relay replaced by a recorder: a registry
# miss must never reach a real daemon from this suite.
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

SENDER_HOST="testhost"
SENDER="t-sender"
LOCAL_PEER="t-local"
SELF_SENDER="$WORK/self-sender.txt"
SELF_PEER="$WORK/self-peer.txt"
INBOX="$SOT_COMM_HOME/inbox/$LOCAL_PEER.jsonl"

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

# The peer is pinned to another host so no daemon is dialled for the poke.
setup_rows() {
    ( cd "$WORK" && SOT_COMM_SELF_FILE="$SELF_PEER" SOT_COMM_TEST_HOST="otherbox" \
        "$JOIN" --name "$LOCAL_PEER" ) >/dev/null 2>&1 || return 1
    ( cd "$WORK" && SOT_COMM_SELF_FILE="$SELF_SENDER" SOT_COMM_TEST_HOST="$SENDER_HOST" \
        "$JOIN" --name "$SENDER" ) >/dev/null 2>&1 || return 1
}

SEND_OUT=""; SEND_ERR=""; SEND_RC=0
run_send() {
    SEND_OUT="$(cd "$WORK" && SOT_COMM_SELF_FILE="$SELF_SENDER" SOT_COMM_TEST_HOST="$SENDER_HOST" \
        "$SEND" "$@" 2>"$WORK/err.txt")"
    SEND_RC=$?
    SEND_ERR="$(cat "$WORK/err.txt" 2>/dev/null)"
    return 0
}

# The newest inbox record's msg must be FILE's bytes exactly: nothing added, dropped or re-quoted.
last_msg_is() {  # FILE
    tail -n 1 "$INBOX" | jq -j .msg > "$WORK/got.txt" || return 1
    cmp -s "$1" "$WORK/got.txt"
}

# A body with every character the shell would act on, and a trailing newline.
BODY="$WORK/body.txt"
cat > "$BODY" <<'BODY_TEXT'
Backquotes `date` and command substitution $(echo injected) stay literal.
Quotes: "double", 'single', and a back\slash with ${HOME} unexpanded.

  An indented line after a blank one, and a trailing newline below.

BODY_TEXT
# A second body: blank lines before its tail, and no newline at the very end.
BODY_TWO="$WORK/body-two.txt"
printf 'no newline before these\n\n\n' > "$BODY_TWO"
printf '%s' 'a tail with no newline: $(not run) `not run` "x" '\''y'\''' >> "$BODY_TWO"

case_file_path_delivers_the_body_byte_identical() {
    linux_only "--file PATH delivers the file's bytes" || return 0
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local f
    for f in "$BODY" "$BODY_TWO"; do
        : > "$INBOX"
        run_send "@$LOCAL_PEER" --file "$f"
        [ "$SEND_RC" -eq 0 ] || { echo "  exited $SEND_RC (out: '$SEND_OUT' err: '$SEND_ERR')"; return 1; }
        contains "$SEND_OUT" "filed -> @$LOCAL_PEER" \
            || { echo "  verdict was '$SEND_OUT'"; return 1; }
        last_msg_is "$f" || { echo "  the delivered body differs from $f"; return 1; }
    done
    [ -s "$STUB_LOG" ] && { echo "  a registry hit reached the relay"; return 1; }
    return 0
}

case_file_dash_delivers_the_body_from_stdin_byte_identical() {
    linux_only "--file - delivers stdin's bytes" || return 0
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    : > "$INBOX"
    run_send "@$LOCAL_PEER" --file - < "$BODY"
    [ "$SEND_RC" -eq 0 ] || { echo "  exited $SEND_RC (out: '$SEND_OUT' err: '$SEND_ERR')"; return 1; }
    contains "$SEND_OUT" "filed -> @$LOCAL_PEER" \
        || { echo "  verdict was '$SEND_OUT'"; return 1; }
    last_msg_is "$BODY" || { echo "  the delivered body differs from the stdin bytes"; return 1; }
    return 0
}

case_a_missing_file_is_refused_and_appends_nothing() {
    linux_only "a missing file is refused" || return 0
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    : > "$INBOX"
    run_send "@$LOCAL_PEER" --file "$WORK/no-such-file.txt"
    [ "$SEND_RC" -eq 1 ] || { echo "  exited $SEND_RC, want 1 (out: '$SEND_OUT' err: '$SEND_ERR')"; return 1; }
    contains "$SEND_OUT$SEND_ERR" "cannot read" \
        || { echo "  the refusal does not name the unreadable file: '$SEND_OUT$SEND_ERR'"; return 1; }
    [ -s "$INBOX" ] && { echo "  a refused send appended a record"; return 1; }
    return 0
}

case_a_file_beside_text_is_refused_and_appends_nothing() {
    linux_only "--file beside text is refused" || return 0
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    : > "$INBOX"
    run_send "@$LOCAL_PEER" --file "$BODY" "and some text"
    [ "$SEND_RC" -eq 1 ] || { echo "  exited $SEND_RC, want 1 (out: '$SEND_OUT' err: '$SEND_ERR')"; return 1; }
    [ -s "$INBOX" ] && { echo "  a refused send appended a record"; return 1; }
    return 0
}

case_a_file_with_no_path_is_refused_and_appends_nothing() {
    linux_only "--file with no path is refused" || return 0
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    : > "$INBOX"
    run_send "@$LOCAL_PEER" --file
    [ "$SEND_RC" -eq 1 ] || { echo "  exited $SEND_RC, want 1 (out: '$SEND_OUT' err: '$SEND_ERR')"; return 1; }
    [ -s "$INBOX" ] && { echo "  a refused send appended a record"; return 1; }
    return 0
}

case_an_empty_file_is_refused_and_appends_nothing() {
    linux_only "an empty file is refused" || return 0
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    : > "$INBOX"
    : > "$WORK/empty.txt"
    run_send "@$LOCAL_PEER" --file "$WORK/empty.txt"
    [ "$SEND_RC" -eq 1 ] || { echo "  exited $SEND_RC, want 1 (out: '$SEND_OUT' err: '$SEND_ERR')"; return 1; }
    contains "$SEND_OUT$SEND_ERR" "is empty" \
        || { echo "  the refusal does not say the file is empty: '$SEND_OUT$SEND_ERR'"; return 1; }
    [ -s "$INBOX" ] && { echo "  a refused send appended a record"; return 1; }
    return 0
}

check "T1: --file PATH delivers the body byte for byte" case_file_path_delivers_the_body_byte_identical
check "T2: --file - delivers stdin's bytes byte for byte" case_file_dash_delivers_the_body_from_stdin_byte_identical
check "T3: a missing file is refused with one line, exit 1, and nothing appended" case_a_missing_file_is_refused_and_appends_nothing
check "T4: --file beside text is refused, and nothing appended" case_a_file_beside_text_is_refused_and_appends_nothing
check "T5: --file with no path is refused, and nothing appended" case_a_file_with_no_path_is_refused_and_appends_nothing
check "T6: an empty file is refused, and nothing appended" case_an_empty_file_is_refused_and_appends_nothing

echo "---"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
