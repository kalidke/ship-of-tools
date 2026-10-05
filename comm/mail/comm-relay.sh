#!/usr/bin/env bash
# comm-relay.sh — INSTANT cross-machine agent messaging via the Ship of Tools daemon.
#
# The only live link between machines (Linux ⇄ Windows) is the SSH-forwarded
# backend socket, so cross-machine agent messages ride it: `agent.send` ->
# daemon -> `agent.message` evt broadcast
# to every connected client. A handle the hub's comm folder lists is filed by the
# hub (`comm.file`); on a box that holds its own link to the hub, that box's
# daemon files for its own folder (rust/backend/src/comm/mail/hub_link.rs).
#
# Requires a daemon built with agent.send/agent.message support (workspace push +
# this relay land together).
#
# ENDPOINT (SOT_RELAY_ENDPOINT, or auto-detected): unix:/path, ssh:target[/host],
# or — Windows only, ADR 0042 amendment decision 5 — pipe:\\.\pipe\name /
# pipe:name, reaching that box's OWN local daemon over its named pipe via
# comm-pipe-request.ps1 (PowerShell; git-bash cannot open a named pipe
# itself). `send` works over a pipe: endpoint.
#
# Usage:
#   comm-relay.sh send @to "message"        # fire-and-forget, instant
#   comm-relay.sh send --all "message"      # broadcast to all clients
set -euo pipefail

# `bridge` is a RETIRED verb: loops started by earlier versions still re-run it
# every 2 s from their own loop text. Say so once, then sleep silently — a loop
# tied to a dead session ends by its own tether check, an untied one idles until
# a reboot. No product code kills anything. It runs before anything is sourced,
# so the retired verb costs nothing and nothing can exit before its one line;
# a number of seconds, not `infinity`, which macOS's sleep refuses.
if [ "${1:-}" = bridge ]; then
    echo "comm-relay: bridge retired in 0.6.6; this leftover loop now sleeps (a reboot clears it)" >&2
    exec sleep 2147483647
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/comm-lib.sh"
eval "$("$SCRIPT_DIR/comm-context.sh")"

# Subcommand parsed FIRST, before any transport setup (Codex review round-2
# SHOULD-FIX 2): `send` needs a routable identity to stamp a from-field
# that means anything, and that check must run BEFORE endpoint/socket
# resolution below — otherwise an unresolved sender on a box with no
# reachable daemon sees only "no sotd daemon found", never the identity
# refusal that's the actual, fixable problem.
SUB="${1:-}"; [ $# -gt 0 ] && shift || true
case "$SUB" in
    send) why="$(sot_require_routable_identity)" || { to="${1:-}"; echo "FAILED -> @${to#@}: $why" >&2; exit 1; } ;;
esac

ENDPOINT="${SOT_RELAY_ENDPOINT:-}"
resolve_endpoint() {
    sot_relay_endpoint "${ENDPOINT:-${SOT_SPAWN_ENDPOINT:-}}"
}
# nc preferred; on hosts without it (e.g. git-bash on Windows, which ships no
# nc) fall back to bash's /dev/tcp for tcp endpoints. unix-socket endpoints
# still require nc -U (/dev/tcp can't speak AF_UNIX). A pipe: endpoint uses
# neither — see the EP_PIPE branch in nc_send below, which drive
# comm-pipe-request.ps1 (PowerShell) instead, since git-bash cannot open a
# named pipe itself.
HAVE_NC=0; command -v nc >/dev/null 2>&1 && HAVE_NC=1
# SOFT for `send` (see the file-first rule below): a target this box's
# registry names is reached by appending to its inbox, which needs no daemon at
# all — so a missing daemon must not refuse the send. Every path that really
# needs the wire calls _require_endpoint and fails there instead.
# A directed send (HANDLE given) reports it as that send's verdict.
_endpoint_missing() {  # [HANDLE]
    local why="no sotd daemon found; set SOT_RELAY_ENDPOINT=unix:/path, ssh:target[/host], or (Windows) pipe:name"
    if [ -n "${1:-}" ]; then echo "FAILED -> @$1: $why" >&2; else echo "ERROR: $why" >&2; fi
}
_require_endpoint() { [ -n "$ENDPOINT" ] && return 0; _endpoint_missing "${1:-}"; return 1; }
ENDPOINT="$(resolve_endpoint || true)"
if [ -z "$ENDPOINT" ]; then
    case "$SUB" in
        send) ;;
        *) _endpoint_missing; exit 1 ;;
    esac
fi
EP_SSH_TARGET=""; EP_SSH_HOST=""; EP_UNIX=""; EP_PIPE=""
case "$ENDPOINT" in
    ssh:*)
        ep_rest="${ENDPOINT#ssh:}"
        case "$ep_rest" in
            */*) EP_SSH_TARGET="${ep_rest%%/*}"; EP_SSH_HOST="${ep_rest#*/}" ;;
            *)   EP_SSH_TARGET="$ep_rest" ;;
        esac
        ;;
    unix:*) EP_UNIX="${ENDPOINT#unix:}" ;;
    # ADR 0042 amendment (2026-09-07): a Windows box's LOCAL daemon only
    # listens on a named pipe. Accepts either the full \\.\pipe\<name> form
    # sot_daemon_endpoint prints or a bare pipe:<name> — both reduce to the
    # trailing NAME (NamedPipeClientStream never takes the \\.\pipe\ prefix).
    pipe:*) EP_PIPE="${ENDPOINT#pipe:}"; EP_PIPE="${EP_PIPE##*\\}" ;;
    "") ;;   # no daemon and a file-first send: nothing to parse
    *) echo "ERROR: bad endpoint '$ENDPOINT'" >&2; exit 1 ;;
esac

# Hello: the daemon reads each connection's first frame for the protocol version
# and ignores its token field, so every connection below sends one first.
# Token source: $SOT_TOKEN, else the 0600 token file in the (700) home. The
# hello reply is an extra line on the wire, but every caller greps by op, so it
# is ignored. client_id "sot-comm" so the roster/logs show what it is. The
# frame itself is `sot_hello_frame` (comm-lib.sh, ADR 0046 decision 1),
# which declares the host and the OS account (ADR 0049 `## User isolation`).

# nc_out: send the single frame on stdin, return immediately (capture any reply line)
nc_send() {
    # The hello is built once, before any transport starts; a process that cannot name its host or OS account
    # sends nothing (the builder says why on stderr).
    local hello
    hello="$(sot_hello_frame)" || return 1
    if [ -n "$EP_PIPE" ]; then
        command -v powershell.exe >/dev/null 2>&1 || {
            echo "ERROR: powershell.exe not found and endpoint is a named pipe" >&2; return 1; }
        local ps1="$SCRIPT_DIR/comm-pipe-request.ps1"
        [ -f "$ps1" ] || {
            echo "ERROR: comm-pipe-request.ps1 not found next to comm-relay.sh ($SCRIPT_DIR)" >&2; return 1; }
        { printf '%s\n' "$hello"; cat; } | timeout 5 powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass \
            -File "$ps1" -PipeName "$EP_PIPE" -Mode Oneshot -Op agent.send -TimeoutSec 5
        return
    fi
    if [ -n "$EP_SSH_TARGET" ]; then
        # `_SOT_BRIDGE_FAIL_FILE`, when send_frame has set it, is where a
        # dying or timed-out bridge's own reason lands (BLOCKER 1's
        # loud-failure requirement) -- nc_send runs as a pipeline stage of
        # its own caller, in its own subshell, so a plain variable set
        # here would never be seen back in send_frame; the file is how it
        # survives the fork. `${...:-/dev/null}` keeps a caller that has not
        # set it silent.
        # `|| rc=${PIPESTATUS[1]}`, not a bare pipeline: this script runs
        # under `set -euo pipefail`, so an unchecked nonzero pipe would
        # exit the whole script right here, before the reformatting below
        # ever ran (it did, the first time this was written -- a dying
        # bridge's raw, unprefixed stderr reached send_frame instead of
        # the "ssh to TARGET exited N: ..." reason).
        # The child's stderr lands in `$raw_file`, a path DISTINCT from
        # `_SOT_BRIDGE_FAIL_FILE`: send_frame's `-s` check reads the latter
        # as "a real failure happened", so it must stay empty on a clean
        # exit -- a successful ssh that merely wrote a known-hosts warning
        # or a login banner to stderr is a delivered frame, not a FAILED
        # one (round-2 blocker: raw stderr used to land straight in the
        # file the success path also checked).
        local rc=0
        local raw_file=""
        [ -n "${_SOT_BRIDGE_FAIL_FILE:-}" ] && raw_file="${_SOT_BRIDGE_FAIL_FILE}.raw"
        { printf '%s\n' "$hello"; cat; } | sot_ssh_bridge "$EP_SSH_TARGET" "$EP_SSH_HOST" 5 2>"${raw_file:-/dev/null}" || rc=${PIPESTATUS[1]}
        if [ "$rc" -ne 0 ] && [ -n "${_SOT_BRIDGE_FAIL_FILE:-}" ]; then
            local detail; detail="$(tr '\n' ' ' < "$raw_file" 2>/dev/null)"
            if [ "$rc" -eq 124 ]; then
                printf 'timed out after 5s reaching %s' "$EP_SSH_TARGET" > "$_SOT_BRIDGE_FAIL_FILE"
            else
                printf 'ssh to %s exited %d' "$EP_SSH_TARGET" "$rc" > "$_SOT_BRIDGE_FAIL_FILE"
            fi
            [ -n "$detail" ] && printf ': %s' "$detail" >> "$_SOT_BRIDGE_FAIL_FILE"
            printf '\n' >> "$_SOT_BRIDGE_FAIL_FILE"
        fi
        [ -n "$raw_file" ] && rm -f "${raw_file:?}"
        return "$rc"
    fi
    if [ "$HAVE_NC" = 1 ] && [ -n "$EP_UNIX" ]; then
        { printf '%s\n' "$hello"; cat; } | timeout 5 nc -U "$EP_UNIX"
    else
        echo "ERROR: nc not found and endpoint is a unix socket (needs nc -U)" >&2; return 1
    fi
}

send_frame() {  # $1 to, $2 text
    _require_endpoint "$1" || return 1
    # Identity is already validated (sot_require_routable_identity, called
    # above for SUB=send before any transport setup — Codex review
    # round-2 finding 4/C) — every relay frame stamps `from:$NAME` on the
    # wire, and a peer's reply routes off that field, so this is never
    # called with an unroutable NAME.
    # MSYS2 argv-conversion guard (comm-lib.sh's sot_jq_rawfile): the
    # message text ($2) can legitimately start with "/" and must never
    # reach jq via --arg — see that helper's comment for the mechanism.
    # The EXIT trap (Codex round finding 9, mirrors comm-send.sh's own
    # MSG_FILE cleanup) is what actually guarantees this temp file never
    # leaks: a `jq` failure under `set -e` exits immediately, skipping a
    # bare `rm -f` placed after it.
    # Not `local`: the EXIT trap below fires after this function has returned.
    msg_file="$(sot_jq_rawfile "$2")" || return 1
    trap 'rm -f "${msg_file:?}"' EXIT
    # A directed send is ONE `comm.file` request and ONE verdict (0031 B1): the
    # daemon that appends answers, so the answer is the whole record.
    local filing=""
    [ -n "$1" ] && filing="$(jq -nc --arg f "$NAME" --arg t "$1" --rawfile m "$msg_file" '{from:$f,to:$t,msg:$m}')"
    # ADR 0048: one opaque id per send, minted HERE and nowhere else. It is
    # what makes a filer's receipt attributable to exactly this frame —
    # without it two concurrent sends to one handle can swap verdicts and a
    # single success can vouch for a failure. Drawn from digits and dashes
    # only, so `--arg` is correct for it (the MSYS2 argv-conversion guard
    # applies to the message BODY, which still goes through --rawfile).
    local MSG_ID; MSG_ID="$(date +%s%N)-$$-$RANDOM"
    local frame; frame="$(jq -nc --arg f "$NAME" --arg t "$1" --arg i "$MSG_ID" --rawfile m "$msg_file" \
        '{v:1,id:1,kind:"req",op:"agent.send",payload:{from:$f,to:$t,text:$m,id:$i}}')"
    rm -f "${msg_file:?}"
    if [ -n "$1" ]; then
        local reason rc=0
        reason="$(sot_comm_file "$1" "$filing")" || rc=$?
        case "$rc" in
            0) echo "filed -> @$1"; return 0 ;;
            2) ;;   # not_here: the hub's folder does not list it — the leg below
            *) echo "FAILED -> @$1: $reason" >&2; return 1 ;;
        esac
    fi
    # THE NOT-MINE LEG: a handle the hub's folder does not list
    # still goes out as `agent.send` and waits for a filer's receipt. A
    # broadcast (`send --all`, sot-nav.sh's envelope) takes this path for its
    # `relayed` count.
    # ONE connection carries both legs (ADR 0048): the `agent.send` ack, then
    # this sender's own `agent.receipt`. Read until the receipt arrives, the
    # daemon closes (EOF ends the loop), or nc_send's existing `timeout 5`
    # expires — the receipt window IS that one transport bound, deliberately
    # not a second knob. `break` closes the fd and the writer takes SIGPIPE.
    #
    # The loop body runs in THIS shell (process substitution, never a pipe),
    # so the verdict variables below survive it.
    local line op ack_ok=false ack_array=false
    local rcpt_seen=false rcpt_filer=""
    local -a receivers=()
    # Not `local`: nc_send below runs inside the process substitution's own
    # subshell (a fork, not this loop), so only a path on disk -- not a
    # variable -- carries a dying/timed-out bridge's reason back here.
    _SOT_BRIDGE_FAIL_FILE="$(mktemp "${TMPDIR:-/tmp}/sot-comm-bridge-fail.XXXXXX")" || _SOT_BRIDGE_FAIL_FILE=""
    while IFS= read -r line; do
        [ -z "$line" ] && continue
        op="$(printf '%s' "$line" | sot_jq -r '.op // empty' 2>/dev/null || true)"
        case "$op" in
            agent.send)
                # An EMPTY line must never pass as an ack: `jq -e` over zero
                # input never sees a falsy last value and exits 0, which is
                # how a missing socket once printed "relayed" (Codex review
                # round-3 finding 3). `ok` stays the wire-compat gate,
                # checked first; `receivers` is the roster snapshot, now a
                # DIAGNOSTIC only (it names who was attached, never that
                # anyone appended).
                if printf '%s' "$line" | jq -e '.payload.ok == true' >/dev/null 2>&1; then
                    ack_ok=true
                fi
                if printf '%s' "$line" | jq -e '.payload.receivers | type == "array"' >/dev/null 2>&1; then
                    ack_array=true
                    mapfile -t receivers < <(printf '%s' "$line" | sot_jq -r '.payload.receivers[]')
                fi
                # A directed send waits for its receipt, roster or no roster: a hub link files and receipts and is never on the roster (role cli). A broadcast has no single addressee, so it is decided on the ack.
                if [ "$ack_ok" = false ] || [ "$ack_array" = false ] || [ -z "$1" ]; then
                    break
                fi
                ;;
            agent.receipt)
                # Somebody else's receipt on this shared fan-out is not ours:
                # the id is the only thing that makes it ours.
                if printf '%s' "$line" | jq -e --arg i "$MSG_ID" '.payload.id == $i' >/dev/null 2>&1; then
                    # A receipt EXISTS only as a positive claim, so its
                    # arrival is the whole verdict: whoever sent it appended
                    # this frame. Two receipts would mean two filers and
                    # either is true, so the first one ends the wait.
                    rcpt_seen=true
                    rcpt_filer="$(printf '%s' "$line" | sot_jq -r '.payload.filer // ""' 2>/dev/null || true)"
                    break
                fi
                ;;
        esac
    done < <(printf '%s\n' "$frame" | nc_send 2>/dev/null)

    # THE VERDICT, decided in ONE place with ONE stated precedence:
    #
    #   1. this sender's own receipt -- the frame was appended;
    #   2. the daemon's own ack -- what it said about the send;
    #   3. the bridge's reason -- consulted ONLY where 1 and 2 said nothing;
    #   4. the daemon did not answer.
    #
    # The RECORD decides; the transport only EXPLAINS. An exit status, a
    # signal or a line of stderr cannot turn an appended frame into a
    # failure -- it exists to explain a verdict the record could not
    # supply. Every earlier shape here assembled the verdict from
    # independent checks whose ORDER was the behaviour, and each fix added
    # one more in front of the others: that is how a filed AND receipted
    # frame came to be reported FAILED because the ssh child took SIGPIPE
    # (141) or an abrupt teardown (255) on the way out -- the loop above
    # `break`s the instant the receipt lands, closing the pipe the child is
    # still writing to -- and how a plainly unanswered ssh send reported
    # the bridge's own 5s timeout, the very bound the receipt window IS,
    # instead of NOT CONFIRMED and the roster that diagnoses it.
    #
    # Reading the reason is not consulting it: it is gathered here (and the
    # file removed either way) so the branches below stay one decision.
    local bridge_reason=""
    if [ -n "$_SOT_BRIDGE_FAIL_FILE" ] && [ -s "$_SOT_BRIDGE_FAIL_FILE" ]; then
        bridge_reason="$(cat "$_SOT_BRIDGE_FAIL_FILE")"
    fi
    [ -z "${_SOT_BRIDGE_FAIL_FILE:-}" ] || rm -f -- "${_SOT_BRIDGE_FAIL_FILE:?}"

    # 1. A receipt carrying this sender's own frame id (the loop above
    # accepts no other) is the whole verdict, and nothing outranks it.
    if [ "$rcpt_seen" = true ] && [ -n "$1" ]; then
        echo "filed -> @$1 (by ${rcpt_filer:-an unnamed filer}, relay)"
        return 0
    fi
    # 2. No receipt: the ack is the rest of the record, and it decides on
    # its own -- a transport that died on the way out explains nothing a
    # daemon's own answer has not already settled.
    if [ "$ack_ok" = true ] && [ "$ack_array" = true ]; then
        if [ -z "$1" ]; then
            # --all: there's no single named recipient to check for —
            # report the count, even zero (that's the honest answer).
            echo "relayed -> <all> (${#receivers[@]} receiver(s)) via $ENDPOINT"
            return 0
        fi
        if [ "${#receivers[@]}" -eq 0 ]; then
            # No receipt in the window, and no one on the roster to name.
            echo "FAILED -> @$1: nobody filed it within 5s: no box knows that handle, or the daemon that holds it is stopped or not linked to the hub" >&2
            return 1
        fi
        # The frame WAS sent and may well have been filed; nothing claimed
        # it. This is the ONLY negative: no filer can honestly report "not
        # me" (it cannot know about the others, and every attached frontend
        # would say it about a handle it does not host), so silence is the
        # negative and it is the sender's own conclusion.
        # The roster survives HERE and nowhere else: as a diagnostic naming
        # who was attached and did not answer, never as a verdict. A name
        # match against a receiver is the bug class this replaced — nothing
        # in this file may compare a target to a receiver's name again.
        local joined; joined="$(printf '%s, ' "${receivers[@]}")"
        echo "NOT CONFIRMED: sent for @$1; nobody claimed it within 5s. Attached: ${joined%, }." >&2
        return 1
    fi
    # 3. `ok` false, or `receivers` absent, or no ack at all: the record
    # supplied no verdict,
    # so NOW the bridge's own reason gets to speak. BLOCKER 1's loud-failure
    # requirement lives exactly here -- a bridge that died or timed out with
    # nothing to show for it must say WHY, never the generic line below.
    if [ -n "$bridge_reason" ]; then
        if [ -z "$1" ]; then
            echo "FAILED -> <all>: $bridge_reason" >&2
        else
            echo "FAILED -> @$1: $bridge_reason" >&2
        fi
        return 1
    fi
    # 4. Nothing answered and the transport has no complaint of its own.
    if [ -z "$1" ]; then
        echo "FAILED -> <all>: the daemon did not answer at $ENDPOINT" >&2
    else
        echo "FAILED -> @$1: the daemon did not answer at $ENDPOINT" >&2
    fi
    return 1
}

# FILE-FIRST (messaging ruling, 2026-09-26). A target with a row in THIS box's
# registry shares this $SOT_COMM_HOME, so its inbox is a plain local append:
# comm-send.sh files the frame (the daemon wakes an idle row), and the FILE is the
# acknowledgement. The wire is only for targets this box cannot name. `relayed`
# was never proof of delivery — it reported the daemon's own success — and the
# "only a reply proves the path" rule it forced on every caller is withdrawn.
# The question BOTH doors must ask (ADR 0048): does this box's registry give
# the target a HOST? That is the field comm-send.sh needs to file,
# and it is what `deliver()` refuses on. Asking a different question here —
# "does a row exist at all" — made the two execs non-exclusive: a row that
# existed with an empty or absent `host` was a hit for the relay and a miss
# for send, so each handed the send to the other, forever, leaking a temp
# file per lap. The predicates agreeing is what deletes that loop; a
# recursion guard would only survive it.
# 0 a hit, 1 a miss, 2 the registry could not be read (never a miss).
_registry_target() {
    local row rc=0
    [ -n "$1" ] || return 1
    row="$(sot_registry_read "$1")" || rc=$?
    [ "$rc" -eq 0 ] || return "$rc"
    [ -n "$(printf '%s' "$row" | sot_jq -r '.host // empty' 2>/dev/null)" ]
}

# SUB was already parsed (and shifted off) above, before the identity gate
# and endpoint resolution — not re-parsed here.
case "$SUB" in
    send)
        TO=""; MSG=""; TO_SET=false
        while [ $# -gt 0 ]; do
            case "$1" in
                --all) TO=""; TO_SET=true; shift ;;
                # Only the FIRST token is the recipient. Once it's consumed, an
                # arg starting with '@' is MESSAGE content — a message may begin
                # with "@peer …". (Previously every @arg overwrote TO, so a body
                # starting with '@' emptied MSG and the send silently dropped.)
                @*)    if [ "$TO_SET" = false ]; then TO="${1#@}"; TO_SET=true
                       else MSG="${MSG:+$MSG }$1"; fi; shift ;;
                *)     MSG="${MSG:+$MSG }$1"; shift ;;
            esac
        done
        [ "$TO_SET" = true ] || { echo "usage: comm-relay.sh send @to \"msg\" | --all \"msg\"  (no recipient)" >&2; exit 1; }
        [ -z "$MSG" ] && { echo "usage: comm-relay.sh send @to \"msg\" | --all \"msg\"  (empty message)" >&2; exit 1; }
        rc=0; _registry_target "$TO" || rc=$?
        case "$rc" in
            0) exec "$SCRIPT_DIR/comm-send.sh" "@$TO" "$MSG" ;;
            1) ;;
            *) echo "FAILED -> @$TO: the registry could not be read, so @$TO could not be routed; nothing was sent" >&2; exit 1 ;;
        esac
        send_frame "$TO" "$MSG"
        ;;
    *)
        echo "usage: comm-relay.sh send ..." >&2; exit 1 ;;
esac
