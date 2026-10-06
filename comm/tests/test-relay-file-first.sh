#!/usr/bin/env bash
# test-relay-file-first.sh — the relay's ack means the frame is FILED (messaging
# ruling, 2026-09-26). Three facts:
#
#   1. A target this box's registry names never touches the wire. comm-relay.sh
#      hands it to comm-send.sh, which appends to that handle's inbox under the
#      shared $SOT_COMM_HOME — so it works with the daemon DOWN, prints
#      `filed -> @name`, and exits 0. `relayed` never meant this: it reported
#      the daemon's own success, which is why callers were told "only a reply
#      proves the path" — a design defect pushed onto every sender.
#   2. A handle nothing can file for is a FAILURE on stderr, exit 1. It used to
#      print `relayed` plus a warning and exit 0.
#   3. A cross-box directed send is ONE `comm.file` request (0031 B1) and the
#      hub's answer is the verdict: `ok` is `filed -> @h` exit 0 whatever the
#      transport's exit status or stderr say; an `error`, with or without a
#      `code`, is `FAILED -> @h: <the daemon's own words>` exit 1; no answer is
#      `FAILED -> @h: the daemon did not answer at <endpoint>`, and only then
#      does the transport's stderr lengthen the reason.
#   4. The NOT-MINE LEG: a `not_here` answer falls back to
#      `agent.send`, whose verdict is the FILER'S RECEIPT (ADR 0048):
#      `filed -> @h (by <filer>, relay)` exit 0 when a receipt carrying this
#      sender's own frame id arrives. A receipt is ONLY ever positive, so
#      silence is the negative: NOT CONFIRMED, with the attached roster as a
#      diagnostic; an empty roster with no receipt is `FAILED -> @h: nobody filed it within 5s: …`. The name-suffix GUESS (a target whose name ends in
#      an attached `fe@<host>`) is gone from the script; a case below is the
#      regression guard that the suffix now means nothing at all.
#
# No bats dependency. HERMETIC: a
# temp $SOT_COMM_HOME, a per-case $SOT_COMM_SELF_FILE, a pinned
# $SOT_COMM_TEST_HOST, and where a daemon is needed a stub `nc`/`ssh` on PATH
# that answers each connection — never the real ~/.sot-comm and never the real
# daemon.
#
# Usage: comm/tests/test-relay-file-first.sh
# Exit: 0 if every case PASSes or SKIPs cleanly, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-comm-relay-file-first-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
JOIN="$SCRIPTS_DIR/comm-join.sh"
RELAY="$SCRIPTS_DIR/comm-relay.sh"
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

SENDER_HOST="testhost"
PEER_HOST="otherbox"
SENDER="t-sender"
TARGET="t-target-$PEER_HOST"
SELF_SENDER="$WORK/self-sender.txt"
SELF_TARGET="$WORK/self-target.txt"

PASS=0; FAIL=0; SKIP=0
check() {
    local desc="$1" fn="$2" rc
    "$fn"; rc=$?
    case "$rc" in
        0) echo "PASS: $desc"; PASS=$((PASS + 1)) ;;
        2) echo "SKIP: $desc"; SKIP=$((SKIP + 1)) ;;
        *) echo "FAIL: $desc"; FAIL=$((FAIL + 1)) ;;
    esac
}
contains() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }

# Two real rows in one temp registry, joined through the real join path: the
# target lives on ANOTHER host, so the sender's poke is skipped by host and no
# pty.input is ever attempted (this suite must not reach a real daemon).
setup_rows() {
    ( cd "$WORK" && SOT_COMM_SELF_FILE="$SELF_TARGET" SOT_COMM_TEST_HOST="$PEER_HOST" \
        "$JOIN" --name "$TARGET" ) >/dev/null 2>&1 || return 1
    ( cd "$WORK" && SOT_COMM_SELF_FILE="$SELF_SENDER" SOT_COMM_TEST_HOST="$SENDER_HOST" \
        "$JOIN" --name "$SENDER" ) >/dev/null 2>&1 || return 1
    jq -e --arg a "$SENDER" --arg b "$TARGET" '.agents | has($a) and has($b)' \
        "$SOT_COMM_HOME/registry.json" >/dev/null 2>&1
}

# relay_send ENDPOINT ARGS... -> run comm-relay.sh as the sender handle.
# stdout and stderr are captured separately (the verdict is stdout, the refusal
# is stderr) and the exit code is returned.
RELAY_OUT=""; RELAY_ERR=""; RELAY_RC=0
relay_send() {
    local ep="$1"; shift
    RELAY_OUT="$(cd "$WORK" && SOT_COMM_SELF_FILE="$SELF_SENDER" SOT_COMM_TEST_HOST="$SENDER_HOST" \
        SOT_RELAY_ENDPOINT="$ep" "$RELAY" "$@" 2>"$WORK/err.txt")"
    RELAY_RC=$?
    RELAY_ERR="$(cat "$WORK/err.txt" 2>/dev/null)"
    return 0
}

# relay_send_with_path SSHDIR ENDPOINT ARGS... -> same as relay_send, with
# SSHDIR prepended to PATH for the call only (BLOCKER 1's ssh: cases below:
# a stub `ssh` ahead of any real one, never leaked into a later case).
relay_send_with_path() {
    local sshdir="$1" ep="$2"; shift 2
    # XDG_RUNTIME_DIR unset for the same reason test-endpoint-gate.sh's own
    # sot_ssh_bridge cases unset it: with it set, _sot_ssh_sharing_ok's own
    # `ssh -G ...` probe (no stdin redirection of its own) reads from the
    # SAME pipe as the real frame, and a stub `ssh` naive enough to answer
    # any invocation -- ours -- drains the frame there instead of at the
    # real bridge call. A real ssh's `-G` never touches stdin at all, so
    # this is a test-fixture concern only, never live behavior.
    RELAY_OUT="$(cd "$WORK" && unset XDG_RUNTIME_DIR && PATH="$sshdir:$PATH" SOT_COMM_SELF_FILE="$SELF_SENDER" SOT_COMM_TEST_HOST="$SENDER_HOST" \
        SOT_RELAY_ENDPOINT="$ep" "$RELAY" "$@" 2>"$WORK/err.txt")"
    RELAY_RC=$?
    RELAY_ERR="$(cat "$WORK/err.txt" 2>/dev/null)"
    return 0
}

case_registry_target_is_filed_with_the_daemon_down() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local inbox="$SOT_COMM_HOME/inbox/$TARGET.jsonl"
    : > "$inbox"
    # Nothing listens on this socket: the point of the case. A registry target
    # needs no daemon, because its inbox is a file this box can append to.
    relay_send "unix:$WORK/no-such-daemon.sock" send "@$TARGET" "hello from the file-first path"
    [ "$RELAY_RC" -eq 0 ] || { echo "  exited $RELAY_RC, want 0 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_OUT" "filed -> @$TARGET" \
        || { echo "  verdict was '$RELAY_OUT', want 'filed -> @$TARGET'"; return 1; }
    contains "$RELAY_OUT" "relayed" \
        && { echo "  the verdict still says 'relayed': '$RELAY_OUT'"; return 1; }
    local n; n="$(grep -c 'file-first path' "$inbox" 2>/dev/null || echo 0)"
    [ "$n" -eq 1 ] || { echo "  the target's inbox holds $n copies of the message, want exactly 1"; return 1; }
    # The `to` stamp is what keeps a directed frame distinguishable from a
    # broadcast for the recipient's Stop hook and the daemon's wake.
    jq -e --arg t "$TARGET" 'select(.to == $t)' "$inbox" >/dev/null 2>&1 \
        || { echo "  the filed line is not stamped to @$TARGET"; return 1; }
    return 0
}

case_the_hubs_answer_is_the_delivery() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local dir; dir="$(mktemp -d "$WORK/stub-XXXXXX")"
    # The hub files it and says so: one request, one answer, nothing to
    # attribute. The stub would receipt an `agent.send` too, so a verdict that
    # fell through to that leg would name a filer.
    write_row_ssh_stub "$dir" ok yes yes 0 none
    relay_send_with_path "$dir" "unix:$WORK/stub.sock" send "@peer-$PEER_HOST" "/over the wire"
    [ "$RELAY_RC" -eq 0 ] || { echo "  exited $RELAY_RC, want 0 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_OUT" "filed -> @peer-$PEER_HOST" \
        || { echo "  verdict was '$RELAY_OUT', want 'filed -> @peer-$PEER_HOST'"; return 1; }
    contains "$RELAY_OUT" "(by " && { echo "  the hub's own answer named a filer: '$RELAY_OUT'"; return 1; }
    [ ! -e "$dir/agent-send.log" ] || { echo "  an agent.send went out as well"; return 1; }
    [ "$(wc -l < "$dir/comm-file.log")" -eq 1 ] || { echo "  want exactly one comm.file request"; return 1; }
    jq -e --arg t "peer-$PEER_HOST" --arg f "$SENDER" \
        '.payload == {from:$f,to:$t,text:"/over the wire",broadcast:false}' "$dir/comm-file.log" >/dev/null \
        || { echo "  the frame was not {from,to,text,broadcast:false} with no id: $(cat "$dir/comm-file.log")"; return 1; }
    return 0
}

# The not-mine leg's receipt vocabulary: in each case below the
# hub answers `not_here` first, so the `agent.send` fallback runs.
case_an_unanswered_send_is_not_confirmed_and_names_who_was_attached() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local dir; dir="$(mktemp -d "$WORK/stub-XXXXXX")"
    # Ack only, no receipt: an rc9-era frontend appends the frame and says
    # nothing about it. The roster survives HERE and only here, as a
    # diagnostic naming who was attached and did not answer.
    write_row_ssh_stub "$dir" not_here no yes 0 none '["fe@'"$PEER_HOST"'"]'
    relay_send_with_path "$dir" "unix:$WORK/stub.sock" send "@peer-$PEER_HOST" "over the wire"
    [ "$RELAY_RC" -eq 1 ] || { echo "  exited $RELAY_RC, want 1 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_ERR" "NOT CONFIRMED" \
        || { echo "  stderr was '$RELAY_ERR', want NOT CONFIRMED"; return 1; }
    contains "$RELAY_ERR" "Attached: fe@$PEER_HOST" \
        || { echo "  the diagnostic does not name who was attached: '$RELAY_ERR'"; return 1; }
    # The exact strings the guess printed, swept where they were printed
    # FROM (a grep for text that only ever lived in a comment can only pass).
    contains "$RELAY_ERR" "may file it" \
        && { echo "  the old guess text survived: '$RELAY_ERR'"; return 1; }
    contains "$RELAY_ERR" "No confirmed path to a session on another frontend" \
        && { echo "  the old dead-end text survived: '$RELAY_ERR'"; return 1; }
    contains "$RELAY_OUT" "filed" \
        && { echo "  an unproven send still printed a filed line: '$RELAY_OUT'"; return 1; }
    contains "$RELAY_OUT" "relayed" \
        && { echo "  an unproven send still printed a relayed line: '$RELAY_OUT'"; return 1; }
    return 0
}

case_the_name_suffix_means_nothing_now() {
    # The regression guard for the deleted GUESS: `$TARGET` ends in the
    # attached frontend's own host, which used to be reported as a delivery
    # (and matched a misspelling identically). With no receipt it gets exactly
    # the same NOT CONFIRMED as any other unanswered send -- the suffix is not
    # read anywhere.
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local dir; dir="$(mktemp -d "$WORK/stub-XXXXXX")"
    write_row_ssh_stub "$dir" not_here no yes 0 none '["fe@'"$PEER_HOST"'"]'
    relay_send_with_path "$dir" "unix:$WORK/stub.sock" send "@peeeer-$PEER_HOST" "over the wire"
    [ "$RELAY_RC" -eq 1 ] || { echo "  exited $RELAY_RC, want 1 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_ERR" "NOT CONFIRMED" \
        || { echo "  stderr was '$RELAY_ERR', want NOT CONFIRMED"; return 1; }
    contains "$RELAY_ERR" "may file it" \
        && { echo "  the old guess text survived: '$RELAY_ERR'"; return 1; }
    return 0
}

case_a_receipt_for_another_frame_is_not_a_verdict() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local dir; dir="$(mktemp -d "$WORK/stub-XXXXXX")"
    # Attributability (ADR 0048): a receipt whose id is not this sender's own
    # must not be read as its verdict, or two concurrent sends to one handle
    # can swap them and one success vouches for a failure.
    write_row_ssh_stub "$dir" not_here yes yes 0 none '["fe@'"$PEER_HOST"'"]' wrong
    relay_send_with_path "$dir" "unix:$WORK/stub.sock" send "@peer-$PEER_HOST" "over the wire"
    [ "$RELAY_RC" -eq 1 ] || { echo "  exited $RELAY_RC, want 1 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_ERR" "NOT CONFIRMED" \
        || { echo "  stderr was '$RELAY_ERR', want NOT CONFIRMED"; return 1; }
    contains "$RELAY_OUT" "filed" \
        && { echo "  another frame's receipt was read as a delivery: '$RELAY_OUT'"; return 1; }
    return 0
}

case_a_hub_link_receipt_is_filed_with_no_frontend_attached() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local dir; dir="$(mktemp -d "$WORK/stub-XXXXXX")"
    # A hub link files and receipts but says hello as role cli, so it is never
    # on the roster: with no frontend attached the ack's roster is empty, and
    # the receipt must still be waited for and read as the verdict.
    write_row_ssh_stub "$dir" not_here yes yes 0 none '[]' echo "sotd-$PEER_HOST"
    relay_send_with_path "$dir" "unix:$WORK/stub.sock" send "@peer-$PEER_HOST" "window closed"
    [ "$RELAY_RC" -eq 0 ] || { echo "  exited $RELAY_RC, want 0 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_OUT" "filed -> @peer-$PEER_HOST (by sotd-$PEER_HOST, relay)" \
        || { echo "  verdict was '$RELAY_OUT', want the filed line"; return 1; }
    return 0
}

case_an_empty_roster_with_no_receipt_is_failed() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local dir; dir="$(mktemp -d "$WORK/stub-XXXXXX")"
    # Not listed, nobody on the roster, no receipt: FAILED, naming both causes the sender cannot tell apart.
    # The stub hangs after the ack, so only the 5 s receipt window ends the wait.
    write_row_ssh_stub "$dir" not_here no yes hang none '[]'
    local t0=$SECONDS
    relay_send_with_path "$dir" "unix:$WORK/stub.sock" send "@peer-$PEER_HOST" "into the void"
    [ $((SECONDS - t0)) -ge 5 ] || { echo "  decided after $((SECONDS - t0))s, want the full 5 s receipt window"; return 1; }
    [ "$RELAY_RC" -eq 1 ] || { echo "  exited $RELAY_RC, want 1 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_ERR" "FAILED -> @peer-$PEER_HOST: nobody filed it within 5s: no box knows that handle, or the daemon that holds it is stopped or not linked to the hub" \
        || { echo "  stderr was '$RELAY_ERR', want the nobody-filed FAILED line"; return 1; }
    contains "$RELAY_ERR" "NOT CONFIRMED" \
        && { echo "  an empty roster gave NOT CONFIRMED: '$RELAY_ERR'"; return 1; }
    return 0
}

# BLOCKER 1 (`timeout N sot_ssh_bridge` never ran a shell function through
# `timeout`'s own execvp): the real call site, comm-relay.sh's send_frame
# via nc_send. A stub `ssh` on PATH stands in for the far end -- never a
# real ssh, never a real daemon.
#
# The stub `ssh` these cases drive is `write_row_ssh_stub` below, shared with
# the verdict table (it is the same seam parameterised further: receipt, ack,
# exit status and stderr content). Here receipt and ack are both yes and the
# stderr is the noise a real `ssh` writes on a first connection, so the ONE
# variable across the three cases is the exit status -- and the frame is
# filed and receipted before the child goes anywhere, so a verdict that
# changes with that status is the transport overruling the record.
#
#   0   -- ROUND 2's OWN BLOCKER: the child's stderr was redirected into
#          the same file `send_frame` read as "a real failure happened", so
#          the `Warning: Permanently added ...` line on a first connection,
#          a server `Banner` or any remote shell noise turned a delivered
#          frame into a FAILED. Hence the stderr line every stub here
#          writes before anything else.
#   141 -- ROUND 3's: `send_frame` breaks out of its read loop the instant
#          the receipt lands and closes the pipe the child is still writing
#          to, so SIGPIPE is the child's ORDINARY way to end a successful
#          send.
#   255 -- the same thing for an abrupt ssh teardown.
#
# `case_ssh_endpoint_bridge_failure_says_failed_with_reason` below is the
# other half: the same stderr line, a non-zero exit, and NO receipt -- so
# the reason is all there is and it must still be reported loudly.
# The three cases are one body: a receipted send over an ssh child that then
# exits with $1. The verdict must be the receipt's in all three. They drive the
# not-mine leg (the hub answers `not_here` first) and go with it in B2; the
# table below holds the same property for the hub's own answer.
_filed_despite_ssh_exit() {  # EXIT_STATUS
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local sshdir; sshdir="$(mktemp -d "$WORK/ssh-exit$1-XXXXXX")"
    write_row_ssh_stub "$sshdir" not_here yes yes "$1" noise
    relay_send_with_path "$sshdir" "ssh:testtarget" send "@peer-$PEER_HOST" "over ssh"
    [ "$RELAY_RC" -eq 0 ] \
        || { echo "  exited $RELAY_RC, want 0 (the ssh child exited $1 AFTER the receipt; out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_OUT" "filed -> @peer-$PEER_HOST (by peer-$PEER_HOST, relay)" \
        || { echo "  verdict was '$RELAY_OUT', want the filed line (err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_ERR" "FAILED" \
        && { echo "  a filed and receipted frame still printed a FAILED line: '$RELAY_ERR'"; return 1; }
    return 0
}
case_ssh_endpoint_reaches_a_stub_daemon_and_files() { _filed_despite_ssh_exit 0; }
case_a_receipt_outranks_a_sigpipe_exit() { _filed_despite_ssh_exit 141; }
case_a_receipt_outranks_an_abrupt_teardown_exit() { _filed_despite_ssh_exit 255; }

case_ssh_endpoint_bridge_failure_says_failed_with_reason() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local sshdir; sshdir="$(mktemp -d "$WORK/ssh-fail-XXXXXX")"
    cat > "$sshdir/ssh" <<'EOF'
#!/bin/sh
echo "Permission denied (publickey)." >&2
exit 255
EOF
    chmod +x "$sshdir/ssh"
    relay_send_with_path "$sshdir" "ssh:testtarget" send "@peer-$PEER_HOST" "over ssh"
    # BLOCKER 1's loud-failure requirement, and the captain's check 3: a
    # status-only assertion (RELAY_RC -ne 0) would ALSO have passed on the
    # pre-fix code, which died at 127 (also nonzero) before ever touching
    # this stub -- the message is what proves the bridge actually ran and
    # THEN failed, not that it never ran at all. The `comm.file` request got
    # no answer, so the child's stderr is the reason after the no-answer line.
    [ "$RELAY_RC" -eq 1 ] || { echo "  exited $RELAY_RC, want 1 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_ERR" "FAILED -> @peer-$PEER_HOST:" \
        || { echo "  stderr was '$RELAY_ERR', want a FAILED line naming the target"; return 1; }
    contains "$RELAY_ERR" "the daemon did not answer at ssh:testtarget" \
        || { echo "  stderr was '$RELAY_ERR', want the no-answer sentence"; return 1; }
    contains "$RELAY_ERR" "Permission denied" \
        || { echo "  stderr was '$RELAY_ERR', want the child's own stderr folded into the reason"; return 1; }
    return 0
}

# ---- THE TABLE (round-3 addendum). Every combination of the four things a
# send's verdict could be read from, walked through the ONE verdict block in
# send_frame over the real transport, and asserted.
#
# THE RULE, from comm/PROTOCOL.md's delivery section, and every expectation
# below is derived from IT and not from the code: a send with a receipt
# reports delivered, whatever the ssh child's exit status or stderr say. With
# no receipt the ack decides; with neither, the bridge's reason decides and
# its text is shown; with none of the three, the daemon did not answer.
# Stderr content never changes a verdict -- it only lengthens the text of a
# FAILED reason.
#
# 0031 B1 put a `comm.file` leg in front of all of that, and its 16 rows come
# first: the hub's answer ok / error with a code / error with no code / none,
# x exit 0/255 x stderr none/complaint. The answer decides; stderr is read
# only when there is none. The 38 rows after them are the not-mine leg (the
# hub answers `not_here` first), deleted in B2.
#
# Not-mine axes: receipt yes/no x ack yes/no x exit 0/255/141 x stderr
# none/complaint/noise = 36 rows. 141 is where this defect came from:
# send_frame breaks out of its read loop the instant a receipt lands and
# closes the pipe the child is still writing to, so SIGPIPE is the ORDINARY
# end of a successful send. Two rows follow the 36 for the timeout wording,
# where the child outlives the bridge's 5s bound (added, not substituted).
#
# Driven end to end rather than by calling an extracted verdict function:
# that also proves what the verdict SEES -- most of all that an exit of 0
# leaves no bridge reason at all no matter what the child wrote to stderr,
# which is round 2's fix and cannot be expressed by handing a function a
# reason string.
_stderr_text() {  # KIND
    case "$1" in
        complaint) printf 'Permission denied (publickey).' ;;
        noise)     printf "Warning: Permanently added 'testtarget' (ED25519) to the list of known hosts." ;;
        *)         printf '' ;;
    esac
}

# write_row_ssh_stub DIR COMMFILE RECEIPT ACK EXIT STDERR_KIND [RECEIVERS] [ID_MODE] [FILER]
# -- a stub `ssh` (and the same script as `nc`, for a unix: endpoint) that says
# exactly what one row asks for; each connection is one run of it. COMMFILE is
# the hub's `comm.file` answer: ok, code, nocode, not_here, or none. ACK is the
# `agent.send` answer: yes (ok, with the roster), error, or no. The row is
# baked into the script's own header (and its stderr text into a file beside
# it, which keeps every quote in that text out of the generated script), so
# the body below is one static template for all 54 rows. RECEIVERS is the
# ack's roster (the first one files); ID_MODE `wrong` receipts another frame;
# FILER names the receipt's filer (default: the roster's first name).
write_row_ssh_stub() {
    local dir="$1" commfile="$2" receipt="$3" ack="$4" status="$5" errkind="$6"
    local receivers="${7:-[\"peer-$PEER_HOST\"]}" idmode="${8:-echo}" filer="${9:-}"
    local hang=no
    if [ "$status" = hang ]; then hang=yes; status=0; fi
    _stderr_text "$errkind" > "$dir/stderr.txt"
    {
        printf '#!/bin/sh\n'
        printf "d='%s'\n" "$dir"
        printf 'commfile=%s\nreceipt=%s\nack=%s\nhang=%s\nstatus=%s\nidmode=%s\n' \
            "$commfile" "$receipt" "$ack" "$hang" "$status" "$idmode"
        printf "receivers='%s'\nfiler='%s'\n" "$receivers" "$filer"
        cat <<'STUB'
receiver=$(printf '%s' "$receivers" | sed -n 's/^\["\([^"]*\)".*/\1/p')
[ -n "$filer" ] && receiver="$filer"
[ -s "$d/stderr.txt" ] && cat "$d/stderr.txt" >&2
while IFS= read -r line; do
    case "$line" in
        *'"op":"hello"'*)
            # A refusing daemon answers the hello with its error. Of this release it then closes; an older one refuses
            # only a protocol mismatch and goes on serving what is behind the hello.
            if [ -s "$d/hello-refusal.txt" ]; then
                code=os_user_conflict; [ -s "$d/hello-code.txt" ] && code="$(cat "$d/hello-code.txt")"
                printf '{"v":1,"id":1,"kind":"res","op":"hello","payload":{"error":"%s","code":"%s"}}\n' "$(cat "$d/hello-refusal.txt")" "$code"
                [ "$code" = protocol_mismatch ] || exit "$status"
            else
                printf '{"v":1,"id":1,"kind":"res","op":"hello","payload":{"ok":true}}\n'
            fi ;;
        *'"op":"comm.file"'*)
            printf '%s\n' "$line" >> "$d/comm-file.log"
            to=$(printf '%s' "$line" | sed -n 's/.*"to":"\([^"]*\)".*/\1/p')
            r='{"v":1,"id":1,"kind":"res","op":"comm.file","payload":'
            case "$commfile" in
                ok)       printf '%s{"ok":true}}\n' "$r" ;;
                code)     printf '%s{"error":"no live session holds @%s","code":"no_live_session"}}\n' "$r" "$to" ;;
                nocode)   printf '%s{"error":"unknown op: comm.file"}}\n' "$r" ;;
                not_here) printf '%s{"error":"no box knows that handle: %s","code":"not_here"}}\n' "$r" "$to" ;;
            esac
            exit "$status"
            ;;
        *'"op":"agent.send"'*)
            printf '%s\n' "$line" >> "$d/agent-send.log"
            id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
            if [ "$ack" = yes ]; then
                printf '{"v":1,"id":1,"kind":"res","op":"agent.send","payload":{"ok":true,"receivers":%s,"id":"%s"}}\n' "$receivers" "$id"
            elif [ "$ack" = error ]; then
                printf '{"v":1,"id":1,"kind":"res","op":"agent.send","payload":{"error":"the daemon could not read this agent.send","code":"bad_request"}}\n'
            fi
            [ "$idmode" = wrong ] && id="$id-not-yours"
            if [ "$receipt" = yes ]; then
                printf '{"v":1,"id":1,"kind":"evt","op":"agent.receipt","payload":{"id":"%s","filer":"%s"}}\n' "$id" "$receiver"
            fi
            # A child that outlives the bridge's bound, with `exec` so
            # `timeout`'s signal reaches it rather than leaving it holding
            # the pipes the sender reads.
            if [ "$hang" = yes ]; then exec sleep 10; fi
            exit "$status"
            ;;
    esac
done
exit "$status"
STUB
    } > "$dir/ssh"
    chmod +x "$dir/ssh"
    cp "$dir/ssh" "$dir/nc"
}

# COMMFILE RECEIPT ACK EXIT STDERR EXPECT -- EXPECT is what the rule requires:
#   hubfiled     the hub's `ok` is the verdict, with no filer to name
#   refused      the hub's error with a code, in its own words
#   oldhub       an older daemon's code-less unknown-op refusal, as FAILED
#   noanswer     nothing answered: the no-answer sentence
#   filed        (not-mine) the receipt is the verdict, nothing outranks it
#   notconfirmed (not-mine) no receipt, but the ack is the record and decides
#   failed       (not-mine) neither: the bridge's reason speaks, names the exit
#   timeout      (not-mine) the same, for a child killed at the 5s bound
_VERDICT_TABLE=(
    "ok       yes yes 0    none      hubfiled"
    "ok       yes yes 0    complaint hubfiled"
    "ok       yes yes 255  none      hubfiled"
    "ok       yes yes 255  complaint hubfiled"
    "code     yes yes 0    none      refused"
    "code     yes yes 0    complaint refused"
    "code     yes yes 255  none      refused"
    "code     yes yes 255  complaint refused"
    "nocode   yes yes 0    none      oldhub"
    "nocode   yes yes 0    complaint oldhub"
    "nocode   yes yes 255  none      oldhub"
    "nocode   yes yes 255  complaint oldhub"
    "none     yes yes 0    none      noanswer"
    "none     yes yes 0    complaint noanswer"
    "none     yes yes 255  none      noanswer"
    "none     yes yes 255  complaint noanswer"
    "not_here yes yes 0    none      filed"
    "not_here yes yes 0    complaint filed"
    "not_here yes yes 0    noise     filed"
    "not_here yes yes 255  none      filed"
    "not_here yes yes 255  complaint filed"
    "not_here yes yes 255  noise     filed"
    "not_here yes yes 141  none      filed"
    "not_here yes yes 141  complaint filed"
    "not_here yes yes 141  noise     filed"
    "not_here yes no  0    none      filed"
    "not_here yes no  0    complaint filed"
    "not_here yes no  0    noise     filed"
    "not_here yes no  255  none      filed"
    "not_here yes no  255  complaint filed"
    "not_here yes no  255  noise     filed"
    "not_here yes no  141  none      filed"
    "not_here yes no  141  complaint filed"
    "not_here yes no  141  noise     filed"
    "not_here no  yes 0    none      notconfirmed"
    "not_here no  yes 0    complaint notconfirmed"
    "not_here no  yes 0    noise     notconfirmed"
    "not_here no  yes 255  none      notconfirmed"
    "not_here no  yes 255  complaint notconfirmed"
    "not_here no  yes 255  noise     notconfirmed"
    "not_here no  yes 141  none      notconfirmed"
    "not_here no  yes 141  complaint notconfirmed"
    "not_here no  yes 141  noise     notconfirmed"
    "not_here no  no  0    none      noanswer"
    "not_here no  no  0    complaint noanswer"
    "not_here no  no  0    noise     noanswer"
    "not_here no  no  255  none      failed"
    "not_here no  no  255  complaint failed"
    "not_here no  no  255  noise     failed"
    "not_here no  no  141  none      failed"
    "not_here no  no  141  complaint failed"
    "not_here no  no  141  noise     failed"
    "not_here no  yes hang noise     notconfirmed"
    "not_here no  no  hang noise     timeout"
)

# One row: build its stub, send through it, and hold the result against the
# rule. Prints the row and what it got on a disagreement, nothing otherwise.
_check_verdict_row() {  # COMMFILE RECEIPT ACK EXIT STDERR_KIND EXPECT
    local commfile="$1" receipt="$2" ack="$3" status="$4" errkind="$5" expect="$6"
    local dir; dir="$(mktemp -d "$WORK/row-XXXXXX")"
    write_row_ssh_stub "$dir" "$commfile" "$receipt" "$ack" "$status" "$errkind"
    relay_send_with_path "$dir" "ssh:testtarget" send "@peer-$PEER_HOST" "table row"
    local want_rc=1 got="$RELAY_OUT$RELAY_ERR" want=""
    case "$expect" in
        hubfiled)
            want_rc=0; want="filed -> @peer-$PEER_HOST" ;;
        refused)
            want="FAILED -> @peer-$PEER_HOST: no live session holds @peer-$PEER_HOST" ;;
        oldhub)
            want="FAILED -> @peer-$PEER_HOST: unknown op: comm.file" ;;
        noanswer)
            want="FAILED -> @peer-$PEER_HOST: the daemon did not answer at ssh:testtarget" ;;
        filed)
            want_rc=0; want="filed -> @peer-$PEER_HOST (by peer-$PEER_HOST, relay)" ;;
        notconfirmed)
            want="NOT CONFIRMED: sent for @peer-$PEER_HOST" ;;
        failed)
            want="FAILED -> @peer-$PEER_HOST: ssh to testtarget exited $status" ;;
        timeout)
            want="FAILED -> @peer-$PEER_HOST: timed out after 5s reaching testtarget" ;;
    esac
    local bad=""
    [ "$RELAY_RC" -eq "$want_rc" ] || bad="exit $RELAY_RC, want $want_rc"
    contains "$got" "$want" || bad="${bad:+$bad; }missing '$want'"
    # A verdict the transport decided: a filed frame must carry no FAILED
    # line, and a FAILED reason must carry the child's stderr when it wrote
    # any -- the text of a reason is the ONLY thing stderr may change.
    if [ "$expect" = filed ] || [ "$expect" = hubfiled ]; then
        ! contains "$RELAY_ERR" "FAILED" || bad="${bad:+$bad; }a filed frame also printed FAILED"
    fi
    if [ "$expect" = hubfiled ]; then
        ! contains "$RELAY_OUT" "(by " || bad="${bad:+$bad; }the hub's own answer fell through to a receipt"
    fi
    if [ "$errkind" != none ]; then
        case "$commfile:$expect" in
            *:failed|*:timeout|none:noanswer)
                contains "$got" "$(_stderr_text "$errkind")" || bad="${bad:+$bad; }the reason dropped the child's stderr" ;;
            ok:*|code:*|nocode:*)
                ! contains "$got" "$(_stderr_text "$errkind")" || bad="${bad:+$bad; }stderr was read although the hub answered" ;;
        esac
    fi
    [ -z "$bad" ] && return 0
    echo "  row [$commfile $receipt $ack $status $errkind -> $expect]: $bad (out: '$RELAY_OUT' err: '$RELAY_ERR')"
    return 1
}

case_every_combination_gets_the_verdict_the_rule_requires() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local row rows=0 bad=0
    for row in "${_VERDICT_TABLE[@]}"; do
        rows=$((rows + 1))
        # Word-split on purpose: the row IS six fields.
        # shellcheck disable=SC2086
        _check_verdict_row $row || bad=$((bad + 1))
    done
    [ "$rows" -eq 54 ] || { echo "  the table holds $rows rows, want 16 comm.file rows plus 36 and 2 timeout rows"; return 1; }
    [ "$bad" -eq 0 ] || { echo "  $bad of $rows rows disagreed with the rule"; return 1; }
    return 0
}

# A daemon that refuses this client's hello (ADR 0049 `## User isolation`) is read by one rule: the request behind the hello
# is decided by its own reply, and the refusal is named only when nothing answered. A daemon of this release closes after
# refusing (stub: os_user_conflict), so there is no reply; an older one refuses only the protocol and then answers.
HELLO_REFUSAL="host-a has said hello as more than one OS account"
# refuse_hello DIR [CODE] -- the stub in DIR refuses the hello with HELLO_REFUSAL under CODE (default os_user_conflict).
refuse_hello() {
    printf '%s' "$HELLO_REFUSAL" > "$1/hello-refusal.txt"
    [ -z "${2:-}" ] || printf '%s' "$2" > "$1/hello-code.txt"
}
case_a_refused_hello_is_the_verdict_for_a_directed_send() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local dir; dir="$(mktemp -d "$WORK/stub-XXXXXX")"
    write_row_ssh_stub "$dir" ok yes yes 0 none
    refuse_hello "$dir"
    relay_send_with_path "$dir" "unix:$WORK/stub.sock" send "@peer-$PEER_HOST" "over the wire"
    [ "$RELAY_RC" -eq 1 ] || { echo "  exited $RELAY_RC, want 1 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_ERR" "FAILED -> @peer-$PEER_HOST: hello refused: $HELLO_REFUSAL" \
        || { echo "  stderr was '$RELAY_ERR', want the refusal named"; return 1; }
    contains "$RELAY_ERR" "did not answer" && { echo "  a daemon that answered was called silent: '$RELAY_ERR'"; return 1; }
    [ -z "$RELAY_OUT" ] || { echo "  a refused send printed a verdict on stdout: '$RELAY_OUT'"; return 1; }
    return 0
}

case_a_refused_hello_is_the_verdict_for_a_broadcast() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local dir; dir="$(mktemp -d "$WORK/stub-XXXXXX")"
    write_row_ssh_stub "$dir" ok yes yes 0 none
    refuse_hello "$dir"
    relay_send_with_path "$dir" "unix:$WORK/stub.sock" send --all "to everyone"
    [ "$RELAY_RC" -eq 1 ] || { echo "  exited $RELAY_RC, want 1 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_ERR" "FAILED -> <all>: hello refused: $HELLO_REFUSAL" \
        || { echo "  stderr was '$RELAY_ERR', want the refusal named"; return 1; }
    [ -z "$RELAY_OUT" ] || { echo "  a refused broadcast printed a verdict on stdout: '$RELAY_OUT'"; return 1; }
    return 0
}

# An older daemon refuses the protocol and goes on to serve the request: its answer is the verdict.
case_an_older_daemons_protocol_refusal_does_not_decide_a_directed_send() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local dir; dir="$(mktemp -d "$WORK/stub-XXXXXX")"
    write_row_ssh_stub "$dir" ok yes yes 0 none
    refuse_hello "$dir" protocol_mismatch
    relay_send_with_path "$dir" "unix:$WORK/stub.sock" send "@peer-$PEER_HOST" "over the wire"
    [ "$RELAY_RC" -eq 0 ] || { echo "  exited $RELAY_RC, want 0 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_OUT" "filed -> @peer-$PEER_HOST" \
        || { echo "  verdict was '$RELAY_OUT', want 'filed -> @peer-$PEER_HOST'"; return 1; }
    contains "$RELAY_ERR" "refused" && { echo "  the refusal was named though the request was answered: '$RELAY_ERR'"; return 1; }
    return 0
}

case_an_older_daemons_protocol_refusal_does_not_decide_a_broadcast() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local dir; dir="$(mktemp -d "$WORK/stub-XXXXXX")"
    write_row_ssh_stub "$dir" ok yes yes 0 none
    refuse_hello "$dir" protocol_mismatch
    relay_send_with_path "$dir" "unix:$WORK/stub.sock" send --all "to everyone"
    [ "$RELAY_RC" -eq 0 ] || { echo "  exited $RELAY_RC, want 0 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_OUT" "relayed -> <all> (1 receiver(s)) via unix:$WORK/stub.sock" \
        || { echo "  verdict was '$RELAY_OUT', want the relayed count"; return 1; }
    return 0
}

# An ack that carries an error is the daemon's answer to the send and decides it, even behind an older daemon's protocol
# refusal: the refusal is named only when nothing else decided.
ACK_ERROR="the daemon could not read this agent.send"
case_an_ack_with_an_error_decides_the_not_mine_leg() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local dir; dir="$(mktemp -d "$WORK/stub-XXXXXX")"
    write_row_ssh_stub "$dir" not_here no error 0 none
    refuse_hello "$dir" protocol_mismatch
    relay_send_with_path "$dir" "unix:$WORK/stub.sock" send "@peer-$PEER_HOST" "over the wire"
    [ "$RELAY_RC" -eq 1 ] || { echo "  directed: exited $RELAY_RC, want 1 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_ERR" "FAILED -> @peer-$PEER_HOST: $ACK_ERROR" || { echo "  directed: stderr was '$RELAY_ERR', want the ack's words"; return 1; }
    contains "$RELAY_ERR" "hello refused" && { echo "  directed: the refusal was named over the ack: '$RELAY_ERR'"; return 1; }
    relay_send_with_path "$dir" "unix:$WORK/stub.sock" send --all "to everyone"
    [ "$RELAY_RC" -eq 1 ] || { echo "  broadcast: exited $RELAY_RC, want 1 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_ERR" "FAILED -> <all>: $ACK_ERROR" || { echo "  broadcast: stderr was '$RELAY_ERR', want the ack's words"; return 1; }
    contains "$RELAY_ERR" "hello refused" && { echo "  broadcast: the refusal was named over the ack: '$RELAY_ERR'"; return 1; }
    return 0
}

# A process that cannot name its OS account sends nothing, and says so: the builder's words, never "did not answer".
UNREADABLE_ACCOUNT="_sot_os_user: this process's OS account is unreadable -- cannot declare an identity"
case_an_unreadable_account_is_named_by_a_directed_send() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local dir; dir="$(mktemp -d "$WORK/stub-XXXXXX")"
    write_row_ssh_stub "$dir" ok yes yes 0 none
    printf '#!/bin/sh\nexit 0\n' > "$dir/id"; chmod +x "$dir/id"
    relay_send_with_path "$dir" "unix:$WORK/stub.sock" send "@peer-$PEER_HOST" "over the wire"
    [ "$RELAY_RC" -eq 1 ] || { echo "  exited $RELAY_RC, want 1 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_ERR" "FAILED -> @peer-$PEER_HOST: $UNREADABLE_ACCOUNT" \
        || { echo "  stderr was '$RELAY_ERR', want the builder's words"; return 1; }
    return 0
}

case_an_unreadable_account_is_named_by_a_broadcast() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local dir; dir="$(mktemp -d "$WORK/stub-XXXXXX")"
    write_row_ssh_stub "$dir" ok yes yes 0 none
    printf '#!/bin/sh\nexit 0\n' > "$dir/id"; chmod +x "$dir/id"
    relay_send_with_path "$dir" "unix:$WORK/stub.sock" send --all "to everyone"
    [ "$RELAY_RC" -eq 1 ] || { echo "  exited $RELAY_RC, want 1 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_ERR" "FAILED -> <all>: $UNREADABLE_ACCOUNT" \
        || { echo "  stderr was '$RELAY_ERR', want the builder's words"; return 1; }
    return 0
}

check "a registry target is filed with the daemon down" case_registry_target_is_filed_with_the_daemon_down
check "the hub's own answer is the delivery: one comm.file frame, no id, no filer named" case_the_hubs_answer_is_the_delivery
check "an ack with no receipt is NOT CONFIRMED and names who was attached" case_an_unanswered_send_is_not_confirmed_and_names_who_was_attached
check "a target whose name ends in an attached frontend's host gets no credit for it" case_the_name_suffix_means_nothing_now
check "a receipt carrying another frame's id is not this send's verdict" case_a_receipt_for_another_frame_is_not_a_verdict
check "a hub link's receipt is filed though no frontend is attached" case_a_hub_link_receipt_is_filed_with_no_frontend_attached
check "not-mine: an empty roster and no receipt is FAILED naming both causes" case_an_empty_roster_with_no_receipt_is_failed
check "a noisy but clean ssh: exit still files -- stderr output alone is not a failure" case_ssh_endpoint_reaches_a_stub_daemon_and_files
check "a receipt outranks the SIGPIPE (141) the child takes when the send succeeds" case_a_receipt_outranks_a_sigpipe_exit
check "a receipt outranks an abrupt ssh teardown (255) after the frame was filed" case_a_receipt_outranks_an_abrupt_teardown_exit
check "a dying ssh child says FAILED and names the target, its exit status and its stderr" case_ssh_endpoint_bridge_failure_says_failed_with_reason
check "all 16 comm.file answer/exit/stderr rows and 36+2 not-mine rows get the verdict the rule requires" case_every_combination_gets_the_verdict_the_rule_requires
check "a refused hello with nothing behind it is the verdict of a directed send, in the daemon's own words" case_a_refused_hello_is_the_verdict_for_a_directed_send
check "a refused hello with nothing behind it is the verdict of a broadcast, in the daemon's own words" case_a_refused_hello_is_the_verdict_for_a_broadcast
check "an older daemon's protocol refusal does not decide a directed send: its answer does" case_an_older_daemons_protocol_refusal_does_not_decide_a_directed_send
check "an older daemon's protocol refusal does not decide a broadcast: its answer does" case_an_older_daemons_protocol_refusal_does_not_decide_a_broadcast
check "an ack that carries an error decides the not-mine leg, a directed send and a broadcast, over a protocol refusal" case_an_ack_with_an_error_decides_the_not_mine_leg
check "an unreadable OS account is named by a directed send" case_an_unreadable_account_is_named_by_a_directed_send
check "an unreadable OS account is named by a broadcast" case_an_unreadable_account_is_named_by_a_broadcast

# ADR 0049, User isolation: a local bridge's refusal survives the broadcast failure path.
case_a_broadcast_names_the_local_bridges_refusal() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local dir why
    dir="$(mktemp -d "$WORK/refusing-bridge-XXXXXX")"
    why='sotd stdio-bridge: /x/s.sock: not connecting: /x is not a private folder of this OS account'
    cat > "$dir/sotd" <<'FAKEBRIDGE'
#!/bin/sh
[ "$1" = stdio-bridge ] && [ "$2" = --endpoint ] || exit 97
printf 'sotd stdio-bridge: /x/s.sock: not connecting: /x is not a private folder of this OS account\n' >&2
exit 1
FAKEBRIDGE
    chmod +x "$dir/sotd"
    SOTD_BIN="$dir/sotd" relay_send 'unix:/x/s.sock' send --all 'refused'
    [ "$RELAY_RC" -eq 1 ] && [ -z "$RELAY_OUT" ] || { echo "  rc $RELAY_RC, out: $RELAY_OUT"; return 1; }
    contains "$RELAY_ERR" "FAILED -> <all>: $why" || { echo "  the bridge's refusal was lost: $RELAY_ERR"; return 1; }
}
check "a broadcast whose local bridge refuses names the bridge's line (ADR 0049)" case_a_broadcast_names_the_local_bridges_refusal

echo "---"
echo "PASS=$PASS FAIL=$FAIL SKIP=$SKIP"
[ "$FAIL" -eq 0 ]
