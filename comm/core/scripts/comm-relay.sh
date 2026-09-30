#!/usr/bin/env bash
# comm-relay.sh — INSTANT cross-machine agent messaging via the Ship of Tools daemon.
#
# The only live link between machines (Linux ⇄ Windows) is the SSH-forwarded
# backend socket, so cross-machine agent messages ride it: `agent.send` ->
# daemon -> `agent.message` evt broadcast
# to every connected client. On the Linux side the `bridge` subcommand holds a connection
# and drops received messages into the local sot-comm inbox so comm-poll.sh
# sees them; on Windows the frontend writes them to <state-dir>/fe-inbox.jsonl.
#
# Requires a daemon built with agent.send/agent.message support (workspace push +
# this relay land together).
#
# ENDPOINT (SOT_RELAY_ENDPOINT, or auto-detected): unix:/path, ssh:target[/host],
# or — Windows only, ADR 0042 amendment decision 5 — pipe:\\.\pipe\name /
# pipe:name, reaching that box's OWN local daemon over its named pipe via
# comm-pipe-request.ps1 (PowerShell; git-bash cannot open a named pipe
# itself). `send`/`ask` work over a pipe: endpoint; `bridge` refuses one
# (no persistent bridge loop on Windows, ever — see that subcommand).
#
# Usage:
#   comm-relay.sh send @to "message"        # fire-and-forget, instant
#   comm-relay.sh send --all "message"      # broadcast to all clients
#   comm-relay.sh ask  @to "message" [secs] # send, then print replies for secs (default 15)
#   comm-relay.sh bridge [--name NAME]      # hold a connection; relay inbound msgs
#                                           # into ~/.sot-comm/inbox/<NAME>.jsonl
#                                           # (run in background; poll with comm-poll.sh)
#   comm-relay.sh listen [secs]             # print inbound msgs to stdout for secs
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/comm-lib.sh"
eval "$("$SCRIPT_DIR/comm-context.sh")"
ensure_home

# Subcommand parsed FIRST, before any transport setup (Codex review round-2
# SHOULD-FIX 2): `send`/`ask` need a routable identity to stamp a from-field
# that means anything, and that check must run BEFORE endpoint/socket
# resolution below — otherwise an unresolved sender on a box with no
# reachable daemon sees only "no sotd daemon found", never the identity
# refusal that's the actual, fixable problem. `bridge`/`listen` are receive
# operations and need no identity at all (bridge takes its own --name).
SUB="${1:-}"; [ $# -gt 0 ] && shift || true
case "$SUB" in
    send|ask) sot_require_routable_identity || exit 1 ;;
esac

# ADR 0046 decision 1: the `bridge` subcommand's held connection IS what
# "bridge" means (comm-listen.sh's reconnect loop execs exactly this,
# `_sot_bridge_pattern`'s own anchor) — every other subcommand lets
# `sot_hello_frame` infer its role.
HELLO_ROLE=""
[ "$SUB" = "bridge" ] && HELLO_ROLE="bridge"

ENDPOINT="${SOT_RELAY_ENDPOINT:-}"
resolve_endpoint() {
    sot_relay_endpoint "${ENDPOINT:-${SOT_SPAWN_ENDPOINT:-}}"
}
# nc preferred; on hosts without it (e.g. git-bash on Windows, which ships no
# nc) fall back to bash's /dev/tcp for tcp endpoints. unix-socket endpoints
# still require nc -U (/dev/tcp can't speak AF_UNIX). A pipe: endpoint uses
# neither — see the EP_PIPE branches in nc_send/nc_hold below, which drive
# comm-pipe-request.ps1 (PowerShell) instead, since git-bash cannot open a
# named pipe itself.
HAVE_NC=0; command -v nc >/dev/null 2>&1 && HAVE_NC=1
# SOFT for `send`/`ask` (see the file-first rule below): a target this box's
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
        send|ask) ;;
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

# App-level auth (ADR 0010 hardening). The daemon now requires a token-valid
# `hello` before serving ANY op, so every connection below sends one first.
# Token source: $SOT_TOKEN, else the 0600 token file in the (700) home. Empty in
# open-config mode — an empty token still authenticates there (gate is off). The
# hello reply is an extra line on the wire, but every caller greps by op, so it
# is ignored. client_id "sot-comm" so the roster/logs show what it is. The
# frame itself is `sot_hello_frame` (comm-lib.sh, ADR 0046 decision 1),
# stamped with $HELLO_ROLE above.

# nc_out: send the single frame on stdin, return immediately (capture any reply line)
nc_send() {
    if [ -n "$EP_PIPE" ]; then
        command -v powershell.exe >/dev/null 2>&1 || {
            echo "ERROR: powershell.exe not found and endpoint is a named pipe" >&2; return 1; }
        local ps1="$SCRIPT_DIR/comm-pipe-request.ps1"
        [ -f "$ps1" ] || {
            echo "ERROR: comm-pipe-request.ps1 not found next to comm-relay.sh ($SCRIPT_DIR)" >&2; return 1; }
        { sot_hello_frame "$HELLO_ROLE"; cat; } | timeout 5 powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass \
            -File "$ps1" -PipeName "$EP_PIPE" -Mode Oneshot -Op agent.send -TimeoutSec 5
        return
    fi
    if [ -n "$EP_SSH_TARGET" ]; then
        # `_SOT_BRIDGE_FAIL_FILE`, when send_frame has set it, is where a
        # dying or timed-out bridge's own reason lands (BLOCKER 1's
        # loud-failure requirement) -- nc_send runs as a pipeline stage of
        # its own caller, in its own subshell, so a plain variable set
        # here would never be seen back in send_frame; the file is how it
        # survives the fork. `${...:-/dev/null}` keeps the other caller of
        # nc_send (the `bridge` subcommand's best-effort receipt) exactly
        # as silent as before.
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
        { sot_hello_frame "$HELLO_ROLE"; cat; } | sot_ssh_bridge "$EP_SSH_TARGET" "$EP_SSH_HOST" 5 2>"${raw_file:-/dev/null}" || rc=${PIPESTATUS[1]}
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
        { sot_hello_frame "$HELLO_ROLE"; cat; } | timeout 5 nc -U "$EP_UNIX"
    else
        echo "ERROR: nc not found and endpoint is a unix socket (needs nc -U)" >&2; return 1
    fi
}
# nc_hold: keep the connection open (write half stays open so the daemon doesn't
# EOF us) and stream inbound frames to stdout. $1 = seconds (empty = forever).
#
# SELF-HEAL: an ssh: endpoint's read side is `sot_ssh_bridge`'s own child
# process (C10), which EOFs the instant the daemon closes the far end
# (ssh itself exiting is what surfaces as EOF here) — same property the
# old `tail -f /dev/null | nc` form lacked: that form never exited on a
# graceful daemon close (nc's stdin, tail -f, never EOFs), so the socket
# sat in CLOSE-WAIT and the bridge stopped delivering FOREVER (this froze
# an inbox for ~2 days until a manual restart). Unix-socket endpoints
# keep nc -U, which has no child-process EOF to lean on.
#
# sot_hold_stdin: the write side of every branch below -- hello once, then
# hold the pipe open forever without exiting. For `$HELLO_ROLE = bridge`
# (topology plan §F step 2, D10: the half-open-roster fix) that means a
# `ping` every `sot_ping_interval_s` instead of silence, so the daemon's
# 90s read deadline for long-lived roles never trips a connection that's
# actually still there; every other role (`ask`/`listen`, one-shot, always
# called with an explicit `$secs` bound) keeps the original silent hold --
# a `ping` on those would be harmless but pointless, so it stays scoped to
# the role that actually needs it.
sot_hold_stdin() {
    sot_hello_frame "$HELLO_ROLE"
    if [ "$HELLO_ROLE" = "bridge" ]; then
        while :; do sleep "$(sot_ping_interval_s)"; sot_ping_frame; done
    else
        tail -f /dev/null
    fi
}
nc_hold() {
    local secs="${1:-}"
    if [ -n "$EP_PIPE" ]; then
        # No unbounded hold over a pipe: endpoint — a Windows box never
        # runs a persistent bridge loop (ADR 0042 amendment decision 5;
        # see comm-listen.sh's own no-bridge-on-Windows rule). `ask`
        # always passes a concrete $SECS; only a bare `listen`/`bridge`
        # (no seconds) would hit this, and both are refused rather than
        # silently substituting some arbitrary bound.
        [ -n "$secs" ] || {
            echo "ERROR: an unbounded hold is not supported over a pipe: endpoint (no bridge loop on Windows) -- pass an explicit number of seconds" >&2
            return 1; }
        command -v powershell.exe >/dev/null 2>&1 || {
            echo "ERROR: powershell.exe not found and endpoint is a named pipe" >&2; return 1; }
        local ps1="$SCRIPT_DIR/comm-pipe-request.ps1"
        [ -f "$ps1" ] || {
            echo "ERROR: comm-pipe-request.ps1 not found next to comm-relay.sh ($SCRIPT_DIR)" >&2; return 1; }
        # A bounded hold that runs out is its BOUND, not a failure: 124 is
        # how `timeout` reports the ordinary end of every `ask` window, and
        # under this script's `set -e` that status aborted `ask` before it
        # could print its own "not an error -- the frame is filed" verdict.
        # Same class as the verdict block in send_frame below: a transport's
        # exit status overruling a record that had already decided. The two
        # branches after this one say it by returning 0 outright; this one
        # keeps a REAL failure (a pipe that refuses the connection) visible.
        local rc=0
        sot_hello_frame "$HELLO_ROLE" | timeout "$secs" powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass \
            -File "$ps1" -PipeName "$EP_PIPE" -Mode Hold -TimeoutSec "$secs" || rc=$?
        if [ "$rc" -eq 124 ]; then rc=0; fi
        return "$rc"
    fi
    if [ -n "$EP_SSH_TARGET" ]; then
        # The self-heal /dev/tcp was written for (this function's own doc
        # above) comes free from a child process (C10; C3's own Rust
        # spawn is the same shape): `sot_ssh_bridge`'s stdout is the
        # child's own stdout, so `cat` reading it sees EOF the instant
        # the daemon closes -- no fd-9 dance, no nc-vs-/dev/tcp split
        # needed the way a single bidirectional socket fd required.
        # An empty $secs is now the unbounded case inside sot_ssh_bridge
        # itself, so the if/else this used to need collapses to one line.
        sot_hold_stdin | sot_ssh_bridge "$EP_SSH_TARGET" "$EP_SSH_HOST" "$secs"
        return 0
    fi
    # Unix-socket endpoint: requires nc -U (/dev/tcp can't speak AF_UNIX).
    if [ -n "$EP_UNIX" ] && [ "$HAVE_NC" = 1 ]; then
        if [ -n "$secs" ]; then sot_hold_stdin | timeout "$secs" nc -U "$EP_UNIX"
        else sot_hold_stdin | nc -U "$EP_UNIX"; fi
        return 0
    fi
    echo "ERROR: nc not found and endpoint is a unix socket (needs nc -U)" >&2; return 1
}

send_frame() {  # $1 to, $2 text
    _require_endpoint "$1" || return 1
    # Identity is already validated (sot_require_routable_identity, called
    # above for SUB in {send,ask} before any transport setup — Codex review
    # round-2 finding 4/C) — every relay frame stamps `from:$NAME` on the
    # wire, and a peer's reply (or the self-echo filter in filter_inbound
    # above) routes off that field, so this is never called with an
    # unroutable NAME.
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
    # THE NOT-MINE LEG, deleted in B2: a handle the hub's folder does not list
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
                # Wait for a receipt ONLY where one can exist: a directed
                # send with somebody attached. Everything else is decided on
                # the ack and must not spend the caller's seconds — a
                # broadcast has no single
                # addressee to file for, and nobody attached means nobody
                # can append.
                if [ "$ack_ok" = false ] || [ "$ack_array" = false ] || [ -z "$1" ] \
                   || [ "${#receivers[@]}" -eq 0 ]; then
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
            # Nothing is attached to this daemon, and neither this box's
            # registry nor the hub's folder names the target. There is nowhere
            # for the frame to land.
            echo "FAILED -> @$1: no box knows that handle: $1" >&2
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

# Filter inbound frames for agent.message addressed to me (or broadcast).
# Reads frames on stdin; emits one compact JSON line per matching message.
filter_inbound() {
    while IFS= read -r line; do
        [ -z "$line" ] && continue
        local op to from; op="$(printf '%s' "$line" | sot_jq -r '.op // empty' 2>/dev/null || true)"
        [ "$op" = "agent.message" ] || continue
        from="$(printf '%s' "$line" | sot_jq -r '.payload.from // ""' 2>/dev/null || true)"
        [ "$from" = "$NAME" ] && continue   # drop our own broadcasts (self-echo)
        to="$(printf '%s' "$line" | sot_jq -r '.payload.to // ""' 2>/dev/null || true)"
        [ "$to" = "" ] || [ "$to" = "$NAME" ] || continue
        printf '%s\n' "$line"
    done
}

# FILE-FIRST (messaging ruling, 2026-09-26). A target with a row in THIS box's
# registry shares this $SOT_COMM_HOME, so its inbox is a plain local append:
# comm-send.sh files the frame, pokes the row if it is idle, and the FILE is the
# acknowledgement. The wire is only for targets this box cannot name. `relayed`
# was never proof of delivery — it reported the daemon's own success — and the
# "only a reply proves the path" rule it forced on every caller is withdrawn.
# The question BOTH doors must ask (ADR 0048): does this box's registry give
# the target a HOST? That is the field comm-send.sh needs to file and poke,
# and it is what `deliver()` refuses on. Asking a different question here —
# "does a row exist at all" — made the two execs non-exclusive: a row that
# existed with an empty or absent `host` was a hit for the relay and a miss
# for send, so each handed the send to the other, forever, leaking a temp
# file per lap. The predicates agreeing is what deletes that loop; a
# recursion guard would only survive it.
_registry_target() {
    [ -n "$1" ] && jq -e --arg n "$1" '(.agents[$n].host // "") != ""' "$REGISTRY" >/dev/null 2>&1
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
        if _registry_target "$TO"; then
            exec "$SCRIPT_DIR/comm-send.sh" "@$TO" "$MSG"
        fi
        send_frame "$TO" "$MSG"
        ;;
    ask)
        TO=""; MSG=""; SECS=15; TO_SET=false
        while [ $# -gt 0 ]; do
            case "$1" in
                # First token = recipient; after that an '@'-arg is message body
                # (a message may begin with "@peer …"). See `send` above.
                @*) if [ "$TO_SET" = false ]; then TO="${1#@}"; TO_SET=true
                    else MSG="${MSG:+$MSG }$1"; fi; shift ;;
                *)  if [ -z "$MSG" ]; then MSG="$1"
                    elif [[ "$1" =~ ^[0-9]+$ ]]; then SECS="$1"
                    else MSG="$MSG $1"; fi; shift ;;
            esac
        done
        [ "$TO_SET" = true ] || { echo "usage: comm-relay.sh ask @to \"msg\" [secs]  (no recipient)" >&2; exit 1; }
        [ -z "$MSG" ] && { echo "usage: comm-relay.sh ask @to \"msg\" [secs]" >&2; exit 1; }
        if _registry_target "$TO"; then
            "$SCRIPT_DIR/comm-send.sh" "@$TO" "$MSG"
        else
            send_frame "$TO" "$MSG"
        fi
        # The frame is filed; the wait is for a convenience reply, so running
        # out of seconds is not a failure and was never one to report.
        echo "listening ${SECS}s for replies (a timeout is not an error — the frame is filed)..."
        # Replies still stream live as they arrive (unchanged) -- the marker
        # file is only how the caller, after nc_hold's pipeline returns,
        # learns whether it saw NONE of them, so the TIMEOUT annotation below
        # (messaging ruling, 2026-09-26) fires only on a genuine timeout.
        _seen="$(mktemp "${TMPDIR:-/tmp}/sot-comm-ask-seen.XXXXXX")" || _seen=""
        # The verdict was printed BEFORE this window opened, so nothing the
        # window does may change it -- the same rule the verdict block in
        # send_frame states, one call frame out. `nc_hold`'s `pipe:` branch
        # returns a real refusal's status (a named pipe that denies the
        # connection is not a timeout), and under this script's `set -euo
        # pipefail` an uncaught pipeline failure aborted `ask` HERE: after
        # `filed -> @h` had been printed, before the TIMEOUT annotation, and
        # with a non-zero exit that PROTOCOL.md pairs with `FAILED`. So the
        # CALLER owns the status: the window's failure is reported as what it
        # is -- no reply window -- and `ask` still exits 0, because the frame
        # is filed either way.
        _hold_rc=0
        { nc_hold "$SECS" | filter_inbound | while IFS= read -r m; do
            [ -n "$_seen" ] && printf '1' > "$_seen"
            printf '[%s] [%s] %s\n' \
                "$(printf '%s' "$m" | jq -r '.payload.ts')" \
                "$(printf '%s' "$m" | jq -r '.payload.from')" \
                "$(printf '%s' "$m" | jq -r '.payload.text')"
        done; } || _hold_rc=$?
        if [ "$_hold_rc" -ne 0 ]; then
            echo "no reply window: the hold over this endpoint exited $_hold_rc -- @$TO's frame is already filed, so this is not a delivery failure (the hold's own reason is above)."
        elif [ -n "$_seen" ] && [ ! -s "$_seen" ]; then
            note="$(sot_recipient_note "$TO" 2>/dev/null)" || note=""
            case "$note" in
                working*)
                    echo "TIMEOUT: no reply from @$TO in ${SECS}s, but it has not ignored you -- it was $note (not an error — the frame is filed)." ;;
                "needs its own user"*)
                    echo "TIMEOUT: no reply from @$TO in ${SECS}s -- it is stopped on its own user and $note; a reply needs that human first (not an error — the frame is filed)." ;;
                "no heartbeat"*)
                    echo "TIMEOUT: no reply from @$TO in ${SECS}s -- $note (not an error — the frame is filed)." ;;
                "")
                    echo "TIMEOUT: no reply from @$TO in ${SECS}s (not an error — the frame is filed)." ;;
                *)
                    echo "TIMEOUT: no reply from @$TO in ${SECS}s -- it is $note (not an error — the frame is filed)." ;;
            esac
        fi
        [ -z "${_seen:-}" ] || rm -f -- "${_seen:?}"
        ;;
    listen)
        SECS="${1:-}"
        nc_hold "$SECS" | filter_inbound | while IFS= read -r m; do
            printf '[%s] [%s] %s\n' \
                "$(printf '%s' "$m" | jq -r '.payload.ts')" \
                "$(printf '%s' "$m" | jq -r '.payload.from')" \
                "$(printf '%s' "$m" | jq -r '.payload.text')"
        done
        ;;
    bridge)
        [ "${1:-}" = "--name" ] && { NAME="$2"; shift 2; }
        [ -z "$NAME" ] && { echo "ERROR: not joined and no --name; run comm-join.sh first" >&2; exit 1; }
        # ADR 0042 amendment decision 5: no bridge loop on Windows, ever —
        # a persistent `comm-relay.sh bridge` pins this script open and
        # blocks update_comm's replace-in-place (the same reason
        # comm-listen.sh starts no bridge there). A pipe: endpoint only
        # ever means "this box's own local daemon"; that box's receive
        # path is the FE inbox, not a bridge.
        if [ -n "$EP_PIPE" ]; then
            echo "ERROR: comm-relay.sh bridge does not support a pipe: endpoint -- a Windows box's receive path is the FE inbox (fe-inbox.jsonl), never a bridge loop. Use 'comm-relay.sh ask' or 'sot-fe type/screen' for direct pipe requests instead." >&2
            exit 1
        fi
        echo "bridge: relaying inbound agent.messages for @$NAME into $INBOX_DIR/$NAME.jsonl (Ctrl-C to stop)"
        nc_hold | filter_inbound | while IFS= read -r m; do
            # `to` is preserved so the inbox Monitor can rank: direct (to==me)
            # wakes the session, broadcast (to=="") files silently for
            # comm-poll. filter_inbound already dropped to-other frames.
            bline="$(printf '%s' "$m" | jq -c '{from:.payload.from, to:(.payload.to // ""), repo:"daemon", msg:.payload.text, ts:.payload.ts}')" \
                && [ -n "$bline" ] || continue
            # Through the one helper that appends, under the inbox lock.
            breason="$(printf '%s\n' "$bline" | sot_inbox_append "$NAME")" \
                || { echo "bridge: FAILED -> @$NAME: $breason" >&2; continue; }
            # ADR 0048: the append above IS the delivery, so claim it — and
            # only now, after it returned 0. A bridge IS its handle, so
            # `filed: true` is honest by construction; an append that fails
            # claims nothing and the sender reports NOT CONFIRMED.
            # Directed frames only (a broadcast has no addressee to file
            # for), and only when the sender minted an id to attribute it to.
            rid="$(printf '%s' "$m" | jq -r '.payload.id // ""' 2>/dev/null || true)"
            rto="$(printf '%s' "$m" | jq -r '.payload.to // ""' 2>/dev/null || true)"
            [ -n "$rid" ] && [ -n "$rto" ] || continue
            # A fresh one-shot connection, with the bridge role CLEARED: the
            # daemon's roster must not gain a phantom second bridge, and the
            # long-lived-role read deadline must not be armed for a
            # connection that lives for one frame. A receipt that cannot be
            # sent is not an error for the recipient — the frame is filed
            # either way.
            # `grep -qm1` is load-bearing, not tidiness: nc_send is
            # `timeout 5 nc` and the daemon never closes a one-shot
            # connection, so without an early exit every filed frame would
            # cost five seconds inside this loop — a burst would file at one
            # message per five seconds and each receipt would miss its
            # sender's window. The SIGPIPE on the ack is what returns here.
            (
                HELLO_ROLE=""
                printf '%s\n' "$(jq -nc --arg i "$rid" \
                    '{v:1,id:1,kind:"req",op:"agent.filed",payload:{id:$i}}')" \
                    | nc_send 2>/dev/null | grep -qm1 '"op":"agent.filed"'
            ) || true
        done
        ;;
    *)
        echo "usage: comm-relay.sh {send|ask|listen|bridge} ..." >&2; exit 1 ;;
esac
