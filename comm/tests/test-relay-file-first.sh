#!/usr/bin/env bash
# test-relay-file-first.sh — a relay send ends in the appender's verdict. Five facts:
#
#   1. A target this box's registry names never touches the wire. comm-relay.sh
#      hands it to comm-send.sh, which appends to that handle's inbox under the
#      shared $SOT_COMM_HOME — so it works with the daemon DOWN, prints
#      `filed -> @name`, and exits 0.
#   2. A handle nothing can file for is a FAILURE on stderr, exit 1.
#   3. A cross-box directed send is ONE `comm.file` request (0031) and the
#      hub's answer is the verdict: `ok` is `filed -> @h` exit 0 whatever the
#      transport's exit status or stderr say; an `error`, with or without a
#      `code`, `not_here` included, is `FAILED -> @h: <the daemon's own words>`
#      exit 1 and no second request follows; no answer is
#      `FAILED -> @h: the daemon did not answer at <endpoint>`, and only then
#      does the transport's stderr lengthen the reason.
#   4. `send --all` is no verb: it is refused with a usage line and sends nothing.
#   5. sot-nav.sh's nav notification is one `agent.send` with an empty `to`,
#      through the shared one-shot client, and prints its own one line.
#
# No bats dependency. HERMETIC: a
# temp $SOT_COMM_HOME, a per-case $SOT_COMM_SELF_FILE, a pinned
# $SOT_COMM_TEST_HOST, and where a daemon is needed a stub bridge/`ssh` on PATH
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

# relay_answer ENDPOINT [NEXT] -> the path of a stub `sotd` that answers `topology relay-endpoint` with ENDPOINT (this
# box's relay endpoint, as topology gives it) and hands every other call to NEXT when that is executable, else exits 97.
relay_answer() {
    local d="$WORK/relay-answer"
    rm -rf "${d:?}"; mkdir -p "$d"
    printf '#!/bin/sh\nep=%s next=%s\n' "'$1'" "'${2:-}'" > "$d/sotd"
    cat >> "$d/sotd" <<'STUB'
if [ "$1" = topology ] && [ "$2" = relay-endpoint ]; then printf '%s\n' "$ep"; exit 0; fi
[ -n "$next" ] && [ -x "$next" ] && exec "$next" "$@"
exit 97
STUB
    chmod +x "$d/sotd"; printf '%s\n' "$d/sotd"
}

# relay_send ENDPOINT ARGS... -> run comm-relay.sh as the sender handle, on a box whose relay endpoint is ENDPOINT.
# stdout and stderr are captured separately (the verdict is stdout, the refusal
# is stderr) and the exit code is returned.
RELAY_OUT=""; RELAY_ERR=""; RELAY_RC=0
relay_send() {
    local ep="$1" bin; shift
    bin="$(relay_answer "$ep" "${SOTD_BIN:-}")"
    RELAY_OUT="$(cd "$WORK" && SOT_COMM_SELF_FILE="$SELF_SENDER" SOT_COMM_TEST_HOST="$SENDER_HOST" \
        SOTD_BIN="$bin" "$RELAY" "$@" 2>"$WORK/err.txt")"
    RELAY_RC=$?
    RELAY_ERR="$(cat "$WORK/err.txt" 2>/dev/null)"
    return 0
}

# relay_send_with_path SSHDIR ENDPOINT ARGS... -> same as relay_send, with
# SSHDIR prepended to PATH for the call only (BLOCKER 1's ssh: cases below:
# a stub `ssh` ahead of any real one, never leaked into a later case).
relay_send_with_path() {
    local sshdir="$1" ep="$2" bin; shift 2
    bin="$(relay_answer "$ep" "$sshdir/sotd")"
    # XDG_RUNTIME_DIR unset for the same reason test-endpoint-gate.sh's own
    # sot_ssh_bridge cases unset it: with it set, _sot_ssh_sharing_ok's own
    # `ssh -G ...` probe (no stdin redirection of its own) reads from the
    # SAME pipe as the real frame, and a stub `ssh` naive enough to answer
    # any invocation -- ours -- drains the frame there instead of at the
    # real bridge call. A real ssh's `-G` never touches stdin at all, so
    # this is a test-fixture concern only, never live behavior.
    RELAY_OUT="$(cd "$WORK" && unset XDG_RUNTIME_DIR && PATH="$sshdir:$PATH" SOTD_BIN="$bin" SOT_COMM_SELF_FILE="$SELF_SENDER" SOT_COMM_TEST_HOST="$SENDER_HOST" \
        "$RELAY" "$@" 2>"$WORK/err.txt")"
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
    # The hub files it and says so: one request, one answer.
    write_row_ssh_stub "$dir" ok 0 none
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

# not_here is a refusal like any other: one request, its words, exit 1. The stub answers an `agent.send` as the
# hub of the retired receipt route did, with an ack and this frame's receipt, so a relay that still fell back to that
# route would print a filed line.
case_not_here_is_final() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local dir; dir="$(mktemp -d "$WORK/stub-XXXXXX")"
    write_row_ssh_stub "$dir" not_here 0 none
    relay_send_with_path "$dir" "unix:$WORK/stub.sock" send "@peer-$PEER_HOST" "over the wire"
    [ "$RELAY_RC" -eq 1 ] || { echo "  exited $RELAY_RC, want 1 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    [ "$RELAY_ERR" = "FAILED -> @peer-$PEER_HOST: no box knows that handle: peer-$PEER_HOST" ] \
        || { echo "  stderr was '$RELAY_ERR', want the hub's not_here words"; return 1; }
    [ -z "$RELAY_OUT" ] || { echo "  a refused send printed '$RELAY_OUT'"; return 1; }
    [ ! -e "$dir/agent-send.log" ] || { echo "  not_here started a second route: $(cat "$dir/agent-send.log")"; return 1; }
    [ "$(wc -l < "$dir/comm-file.log")" -eq 1 ] || { echo "  want exactly one comm.file request"; return 1; }
    return 0
}

# A broadcast is comm-send.sh's (`--broadcast`, filed per recipient); the relay has no `--all`.
case_send_all_is_not_a_verb() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local dir; dir="$(mktemp -d "$WORK/stub-XXXXXX")"
    write_row_ssh_stub "$dir" ok 0 none
    relay_send_with_path "$dir" "unix:$WORK/stub.sock" send --all "to everyone"
    [ "$RELAY_RC" -eq 1 ] || { echo "  exited $RELAY_RC, want 1 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_ERR" "usage: comm-relay.sh send @to" || { echo "  stderr was '$RELAY_ERR', want the usage line"; return 1; }
    [ -z "$RELAY_OUT" ] || { echo "  --all printed '$RELAY_OUT'"; return 1; }
    [ ! -e "$dir/agent-send.log" ] && [ ! -e "$dir/comm-file.log" ] || { echo "  --all sent a frame"; return 1; }
    return 0
}

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
    # The message, not the status alone, proves the bridge ran and then failed: the `comm.file` request got no
    # answer, so the child's stderr is the reason after the no-answer line.
    [ "$RELAY_RC" -eq 1 ] || { echo "  exited $RELAY_RC, want 1 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_ERR" "FAILED -> @peer-$PEER_HOST:" \
        || { echo "  stderr was '$RELAY_ERR', want a FAILED line naming the target"; return 1; }
    contains "$RELAY_ERR" "the daemon did not answer at ssh:testtarget" \
        || { echo "  stderr was '$RELAY_ERR', want the no-answer sentence"; return 1; }
    contains "$RELAY_ERR" "Permission denied" \
        || { echo "  stderr was '$RELAY_ERR', want the child's own stderr folded into the reason"; return 1; }
    return 0
}

# ---- THE TABLE. Every combination of the things a send's verdict could be read from, walked through
# comm-relay.sh over the real transport, and asserted.
#
# THE RULE, from comm/PROTOCOL.md's delivery section, and every expectation below is derived from IT and not from the
# code: the hub's answer decides, `not_here` like any other refusal; the transport's stderr is read only when there is
# no answer, and then only lengthens the reason. Axes: the hub's answer ok / error with a code / error with no code /
# not_here / none, x exit 0/255 x stderr none/complaint = 20 rows.
#
# Driven end to end rather than by calling an extracted verdict function: that also proves what the verdict SEES,
# most of all that an exit of 0 leaves no bridge reason at all no matter what the child wrote to stderr.
_stderr_text() {  # KIND
    case "$1" in
        complaint) printf 'Permission denied (publickey).' ;;
        *)         printf '' ;;
    esac
}

# write_row_ssh_stub DIR COMMFILE EXIT STDERR_KIND -- a stub `ssh` (and the same job in a stub bridge, for a unix:
# endpoint) that says exactly what one row asks for; each connection is one run of it. COMMFILE is the hub's
# `comm.file` answer: ok, code, nocode, not_here, or none. Every frame it reads is logged by op. An `agent.send` is
# answered as the hub of the retired receipt route answered it, an ack with a roster and this frame's receipt, so a
# sender that still took that route would print a filed line. The row is baked into the script's own header (and its
# stderr text into a file beside it, which keeps every quote in that text out of the generated script).
write_row_ssh_stub() {
    local dir="$1" commfile="$2" status="$3" errkind="$4"
    _stderr_text "$errkind" > "$dir/stderr.txt"
    {
        printf '#!/bin/sh\n'
        printf "d='%s'\n" "$dir"
        printf 'commfile=%s\nstatus=%s\n' "$commfile" "$status"
        cat <<'STUB'
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
            printf '{"v":1,"id":1,"kind":"res","op":"agent.send","payload":{"ok":true,"receivers":["fe@far"],"id":"%s"}}\n' "$id"
            printf '{"v":1,"id":1,"kind":"evt","op":"agent.receipt","payload":{"id":"%s","filer":"fe@far"}}\n' "$id"
            exit "$status"
            ;;
    esac
done
exit "$status"
STUB
    } > "$dir/ssh"
    chmod +x "$dir/ssh"
    { printf '#!/bin/sh\n'; cat <<'BRIDGE'
[ "$1" = stdio-bridge ] && [ "$2" = --endpoint ] || exit 97
case "$3" in unix:*) ;; *) exit 97 ;; esac
exec "$(dirname "$0")/ssh"
BRIDGE
    } > "$dir/sotd"
    chmod +x "$dir/sotd"
}

# COMMFILE EXIT STDERR EXPECT -- EXPECT is what the rule requires:
#   hubfiled     the hub's `ok` is the verdict
#   refused      the hub's error with a code, in its own words
#   nothere      the hub's not_here, in its own words, and nothing else sent
#   oldhub       an older daemon's code-less unknown-op refusal, as FAILED
#   noanswer     nothing answered: the no-answer sentence
_VERDICT_TABLE=(
    "ok       0    none      hubfiled"
    "ok       0    complaint hubfiled"
    "ok       255  none      hubfiled"
    "ok       255  complaint hubfiled"
    "code     0    none      refused"
    "code     0    complaint refused"
    "code     255  none      refused"
    "code     255  complaint refused"
    "not_here 0    none      nothere"
    "not_here 0    complaint nothere"
    "not_here 255  none      nothere"
    "not_here 255  complaint nothere"
    "nocode   0    none      oldhub"
    "nocode   0    complaint oldhub"
    "nocode   255  none      oldhub"
    "nocode   255  complaint oldhub"
    "none     0    none      noanswer"
    "none     0    complaint noanswer"
    "none     255  none      noanswer"
    "none     255  complaint noanswer"
)

# One row: build its stub, send through it, and hold the result against the
# rule. Prints the row and what it got on a disagreement, nothing otherwise.
_check_verdict_row() {  # COMMFILE EXIT STDERR_KIND EXPECT
    local commfile="$1" status="$2" errkind="$3" expect="$4"
    local dir; dir="$(mktemp -d "$WORK/row-XXXXXX")"
    write_row_ssh_stub "$dir" "$commfile" "$status" "$errkind"
    relay_send_with_path "$dir" "ssh:testtarget" send "@peer-$PEER_HOST" "table row"
    local want_rc=1 got="$RELAY_OUT$RELAY_ERR" want=""
    case "$expect" in
        hubfiled) want_rc=0; want="filed -> @peer-$PEER_HOST" ;;
        refused)  want="FAILED -> @peer-$PEER_HOST: no live session holds @peer-$PEER_HOST" ;;
        nothere)  want="FAILED -> @peer-$PEER_HOST: no box knows that handle: peer-$PEER_HOST" ;;
        oldhub)   want="FAILED -> @peer-$PEER_HOST: unknown op: comm.file" ;;
        noanswer) want="FAILED -> @peer-$PEER_HOST: the daemon did not answer at ssh:testtarget" ;;
    esac
    local bad=""
    [ "$RELAY_RC" -eq "$want_rc" ] || bad="exit $RELAY_RC, want $want_rc"
    contains "$got" "$want" || bad="${bad:+$bad; }missing '$want'"
    if [ "$expect" = hubfiled ]; then
        ! contains "$RELAY_ERR" "FAILED" || bad="${bad:+$bad; }a filed frame also printed FAILED"
    fi
    [ ! -e "$dir/agent-send.log" ] || bad="${bad:+$bad; }an agent.send followed the comm.file answer"
    if [ "$errkind" != none ]; then
        case "$commfile" in
            none) contains "$got" "$(_stderr_text "$errkind")" || bad="${bad:+$bad; }the reason dropped the child's stderr" ;;
            *)    ! contains "$got" "$(_stderr_text "$errkind")" || bad="${bad:+$bad; }stderr was read although the hub answered" ;;
        esac
    fi
    [ -z "$bad" ] && return 0
    echo "  row [$commfile $status $errkind -> $expect]: $bad (out: '$RELAY_OUT' err: '$RELAY_ERR')"
    return 1
}

case_every_combination_gets_the_verdict_the_rule_requires() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local row rows=0 bad=0
    for row in "${_VERDICT_TABLE[@]}"; do
        rows=$((rows + 1))
        # Word-split on purpose: the row IS four fields.
        # shellcheck disable=SC2086
        _check_verdict_row $row || bad=$((bad + 1))
    done
    [ "$rows" -eq 20 ] || { echo "  the table holds $rows rows, want 20"; return 1; }
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
    write_row_ssh_stub "$dir" ok 0 none
    refuse_hello "$dir"
    relay_send_with_path "$dir" "unix:$WORK/stub.sock" send "@peer-$PEER_HOST" "over the wire"
    [ "$RELAY_RC" -eq 1 ] || { echo "  exited $RELAY_RC, want 1 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_ERR" "FAILED -> @peer-$PEER_HOST: hello refused: $HELLO_REFUSAL" \
        || { echo "  stderr was '$RELAY_ERR', want the refusal named"; return 1; }
    contains "$RELAY_ERR" "did not answer" && { echo "  a daemon that answered was called silent: '$RELAY_ERR'"; return 1; }
    [ -z "$RELAY_OUT" ] || { echo "  a refused send printed a verdict on stdout: '$RELAY_OUT'"; return 1; }
    return 0
}

# An older daemon refuses the protocol and goes on to serve the request: its answer is the verdict.
case_an_older_daemons_protocol_refusal_does_not_decide_a_directed_send() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local dir; dir="$(mktemp -d "$WORK/stub-XXXXXX")"
    write_row_ssh_stub "$dir" ok 0 none
    refuse_hello "$dir" protocol_mismatch
    relay_send_with_path "$dir" "unix:$WORK/stub.sock" send "@peer-$PEER_HOST" "over the wire"
    [ "$RELAY_RC" -eq 0 ] || { echo "  exited $RELAY_RC, want 0 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_OUT" "filed -> @peer-$PEER_HOST" \
        || { echo "  verdict was '$RELAY_OUT', want 'filed -> @peer-$PEER_HOST'"; return 1; }
    contains "$RELAY_ERR" "refused" && { echo "  the refusal was named though the request was answered: '$RELAY_ERR'"; return 1; }
    return 0
}

# A process that cannot name its OS account sends nothing, and says so: the builder's words, never "did not answer".
UNREADABLE_ACCOUNT="_sot_os_user: this process's OS account is unreadable -- cannot declare an identity"
case_an_unreadable_account_is_named_by_a_directed_send() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local dir; dir="$(mktemp -d "$WORK/stub-XXXXXX")"
    write_row_ssh_stub "$dir" ok 0 none
    printf '#!/bin/sh\nexit 0\n' > "$dir/id"; chmod +x "$dir/id"
    relay_send_with_path "$dir" "unix:$WORK/stub.sock" send "@peer-$PEER_HOST" "over the wire"
    [ "$RELAY_RC" -eq 1 ] || { echo "  exited $RELAY_RC, want 1 (out: '$RELAY_OUT' err: '$RELAY_ERR')"; return 1; }
    contains "$RELAY_ERR" "FAILED -> @peer-$PEER_HOST: $UNREADABLE_ACCOUNT" \
        || { echo "  stderr was '$RELAY_ERR', want the builder's words"; return 1; }
    return 0
}

# ADR 0049, User isolation: a local bridge's refusal is the reason a directed send prints.
case_a_directed_send_names_the_local_bridges_refusal() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local dir why
    dir="$(mktemp -d "$WORK/refusing-bridge-XXXXXX")"
    why='sotd stdio-bridge: /x/s.sock: not connecting: another OS account listens on this socket'
    cat > "$dir/sotd" <<'FAKEBRIDGE'
#!/bin/sh
[ "$1" = stdio-bridge ] && [ "$2" = --endpoint ] || exit 97
printf 'sotd stdio-bridge: /x/s.sock: not connecting: another OS account listens on this socket\n' >&2
exit 1
FAKEBRIDGE
    chmod +x "$dir/sotd"
    SOTD_BIN="$dir/sotd" relay_send 'unix:/x/s.sock' send "@peer-$PEER_HOST" 'refused'
    [ "$RELAY_RC" -eq 1 ] && [ -z "$RELAY_OUT" ] || { echo "  rc $RELAY_RC, out: $RELAY_OUT"; return 1; }
    contains "$RELAY_ERR" "FAILED -> @peer-$PEER_HOST: the daemon did not answer at unix:/x/s.sock" \
        || { echo "  the no-answer sentence was lost: $RELAY_ERR"; return 1; }
    contains "$RELAY_ERR" "$why" || { echo "  the bridge's refusal was lost: $RELAY_ERR"; return 1; }
}

# sot-nav.sh: one empty-target `agent.send` through the shared one-shot client, to this box's relay endpoint, and
# its own one line. The stub acks it as the hub of the retired receipt route did (with a roster), so a nav that still
# went through `comm-relay.sh send --all` would print that relay's "relayed" line too.
case_nav_is_one_ui_notification() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local dir bin out rc=0 env
    dir="$(mktemp -d "$WORK/nav-XXXXXX")"
    write_row_ssh_stub "$dir" ok 0 none
    bin="$(relay_answer "unix:$WORK/stub.sock" "$dir/sotd")"
    out="$(cd "$WORK" && unset XDG_RUNTIME_DIR && PATH="$dir:$PATH" SOTD_BIN="$bin" SOT_COMM_SELF_FILE="$SELF_SENDER" \
        SOT_COMM_TEST_HOST="$SENDER_HOST" SOT_WORKSPACE=ws1 SOT_WORKSPACE_ROOT="$WORK" \
        "$SCRIPTS_DIR/sot-nav.sh" preview "$WORK/src/a.jl" 2>"$WORK/err.txt")" || rc=$?
    [ "$rc" -eq 0 ] || { echo "  exited $rc (out: '$out' err: '$(cat "$WORK/err.txt")')"; return 1; }
    [ "$out" = "sot-nav: nav.preview workspace=ws1 path=src/a.jl (broadcast)" ] \
        || { echo "  stdout was '$out', want nav's one line"; return 1; }
    [ "$(wc -l < "$dir/agent-send.log")" -eq 1 ] && [ ! -e "$dir/comm-file.log" ] \
        || { echo "  want exactly one agent.send and no comm.file"; return 1; }
    env='{"sot_ui":{"v":1,"cmd":"nav.preview","workspace":"ws1","mode":"files","path":"src/a.jl"}}'
    jq -e --arg f "$SENDER" --arg t "$env" '.payload == {from:$f,to:"",text:$t}' "$dir/agent-send.log" >/dev/null \
        || { echo "  the request was not {from,to:\"\",text:<envelope>}: $(cat "$dir/agent-send.log")"; return 1; }
    return 0
}

check "a registry target is filed with the daemon down" case_registry_target_is_filed_with_the_daemon_down
check "the hub's own answer is the delivery: one comm.file frame, no id" case_the_hubs_answer_is_the_delivery
check "not_here is final: one comm.file, the hub's words, exit 1, no second route" case_not_here_is_final
check "send --all is no verb: a usage line, and nothing sent" case_send_all_is_not_a_verb
check "a dying ssh child says FAILED and names the target, its exit status and its stderr" case_ssh_endpoint_bridge_failure_says_failed_with_reason
check "all 20 comm.file answer/exit/stderr rows get the verdict the rule requires" case_every_combination_gets_the_verdict_the_rule_requires
check "a refused hello with nothing behind it is the verdict of a directed send, in the daemon's own words" case_a_refused_hello_is_the_verdict_for_a_directed_send
check "an older daemon's protocol refusal does not decide a directed send: its answer does" case_an_older_daemons_protocol_refusal_does_not_decide_a_directed_send
check "an unreadable OS account is named by a directed send" case_an_unreadable_account_is_named_by_a_directed_send
check "a directed send whose local bridge refuses names the bridge's line (ADR 0049)" case_a_directed_send_names_the_local_bridges_refusal
check "sot-nav.sh sends one empty-target agent.send through the shared client and prints its one line" case_nav_is_one_ui_notification

echo "---"
echo "PASS=$PASS FAIL=$FAIL SKIP=$SKIP"
[ "$FAIL" -eq 0 ]
