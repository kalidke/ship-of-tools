#!/usr/bin/env bash
# comm-relay.sh — INSTANT cross-machine agent messaging via the Ship of Tools daemon.
#
# The git bus is async (commit/push/poll). The only live link between machines
# (Linux ⇄ Windows) is the SSH-forwarded backend socket, so cross-machine
# agent messages ride it: `agent.send` -> daemon -> `agent.message` evt broadcast
# to every connected client. On the Linux side the `bridge` subcommand holds a connection
# and drops received messages into the local sot-comm inbox so comm-poll.sh
# sees them; on Windows the frontend writes them to <state-dir>/fe-inbox.jsonl.
#
# Requires a daemon built with agent.send/agent.message support (workspace push +
# this relay land together).
#
# ENDPOINT (SOT_RELAY_ENDPOINT, or auto-detected): unix:/path, tcp:HOST:PORT,
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
ENDPOINT="$(resolve_endpoint)" || { echo "ERROR: no sotd daemon found; set SOT_RELAY_ENDPOINT=unix:/path, tcp:HOST:PORT, or (Windows) pipe:name" >&2; exit 1; }
EP_HOST=""; EP_PORT=""; EP_UNIX=""; EP_PIPE=""
case "$ENDPOINT" in
    tcp:*)  hp="${ENDPOINT#tcp:}"; EP_HOST="${hp%:*}"; EP_PORT="${hp##*:}" ;;
    unix:*) EP_UNIX="${ENDPOINT#unix:}" ;;
    # ADR 0042 amendment (2026-09-07): a Windows box's LOCAL daemon only
    # listens on a named pipe. Accepts either the full \\.\pipe\<name> form
    # sot_daemon_endpoint prints or a bare pipe:<name> — both reduce to the
    # trailing NAME (NamedPipeClientStream never takes the \\.\pipe\ prefix).
    pipe:*) EP_PIPE="${ENDPOINT#pipe:}"; EP_PIPE="${EP_PIPE##*\\}" ;;
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
    if [ "$HAVE_NC" = 1 ]; then
        if [ -n "$EP_UNIX" ]; then { sot_hello_frame "$HELLO_ROLE"; cat; } | timeout 5 nc -U "$EP_UNIX"; else { sot_hello_frame "$HELLO_ROLE"; cat; } | timeout 5 nc "$EP_HOST" "$EP_PORT"; fi
    elif [ -n "$EP_HOST" ]; then
        # nc-free fallback: bash /dev/tcp. Forward the frame on stdin to the
        # socket, then read the reply for up to 5s. fd 9 stays RW so the daemon
        # doesn't see EOF mid-exchange. The exec MUST live in a subshell: a
        # redirections-only exec whose redirect fails EXITS a non-interactive
        # shell outright — the || error path here was unreachable and a
        # transient connect failure killed the whole send silently (same
        # class as the comm-listen _inject death, fixed 2026-06-11).
        (
            exec 9<>"/dev/tcp/$EP_HOST/$EP_PORT" 2>/dev/null \
                || { echo "ERROR: /dev/tcp connect to $EP_HOST:$EP_PORT failed" >&2; exit 1; }
            { sot_hello_frame "$HELLO_ROLE"; cat; } >&9
            timeout 5 cat <&9
            exec 9<&- 9>&- 2>/dev/null || true
        ) || return 1
    else
        echo "ERROR: nc not found and endpoint is a unix socket (needs nc -U)" >&2; return 1
    fi
}
# nc_hold: keep the connection open (write half stays open so the daemon doesn't
# EOF us) and stream inbound frames to stdout. $1 = seconds (empty = forever).
#
# SELF-HEAL: for TCP we use bash /dev/tcp, NOT nc. `cat <&9` returns the instant
# the daemon closes its end (FIN/EOF), so `bridge` exits and comm-listen.sh's
# reconnect loop re-establishes the connection within ~2s. The old
# `tail -f /dev/null | nc` form never exits on a graceful daemon close — nc keeps
# running because its stdin (tail -f) never EOFs — so the socket sits in
# CLOSE-WAIT and the bridge stops delivering FOREVER (this froze an inbox for
# ~2 days until a manual restart). /dev/tcp fixes that. fd 9 is opened RW so the
# write half stays open (daemon doesn't EOF us) while the read EOF still fires.
# Unix-socket endpoints can't use /dev/tcp (AF_UNIX) so they keep nc -U.
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
        sot_hello_frame "$HELLO_ROLE" | timeout "$secs" powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass \
            -File "$ps1" -PipeName "$EP_PIPE" -Mode Hold -TimeoutSec "$secs"
        return
    fi
    if [ -n "$EP_HOST" ]; then
        if exec 9<>"/dev/tcp/$EP_HOST/$EP_PORT" 2>/dev/null; then
            # Write side backgrounded (hello, then -- for bridge -- a `ping`
            # every sot_ping_interval_s) so it can keep feeding fd 9 while
            # this same process foreground-reads <&9 below; killed the
            # instant that read returns (self-heal is unaffected -- it's
            # `cat <&9`'s own EOF-on-daemon-close that still drives it).
            ( sot_hold_stdin >&9 ) &
            local writer_pid=$!
            if [ -n "$secs" ]; then timeout "$secs" cat <&9; else cat <&9; fi
            kill "$writer_pid" 2>/dev/null || true
            wait "$writer_pid" 2>/dev/null || true
            exec 9<&- 9>&- 2>/dev/null || true
            return 0
        fi
        # bash built without /dev/tcp: fall back to nc. NOTE: this form does NOT
        # self-heal on a graceful close — prefer a /dev/tcp-capable bash for bridges.
        if [ "$HAVE_NC" = 1 ]; then
            if [ -n "$secs" ]; then sot_hold_stdin | timeout "$secs" nc "$EP_HOST" "$EP_PORT"
            else sot_hold_stdin | nc "$EP_HOST" "$EP_PORT"; fi
            return 0
        fi
        echo "ERROR: cannot open /dev/tcp/$EP_HOST/$EP_PORT and nc not found" >&2; return 1
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
    trap 'rm -f "$msg_file"' EXIT
    local frame; frame="$(jq -nc --arg f "$NAME" --arg t "$1" --rawfile m "$msg_file" \
        '{v:1,id:1,kind:"req",op:"agent.send",payload:{from:$f,to:$t,text:$m}}')"
    rm -f "$msg_file"
    local resp; resp="$(printf '%s\n' "$frame" | nc_send 2>/dev/null | grep -m1 '"op":"agent.send"' || true)"
    # An EMPTY $resp must never pass: `jq -e` on zero input never sees a
    # falsy last value to react to, so it exits 0 — a missing socket used
    # to print "relayed" and exit 0 (Codex review round-3 finding 3).
    # Require a NONEMPTY response AND a true ack before even looking at
    # `receivers` — `ok` stays the wire-compat gate this checks first
    # (scheduled for deletion at the next protocol bump); anything short
    # of that is a real failure, falling through to the WARN branch below.
    if [ -n "$resp" ] && printf '%s' "$resp" | jq -e '.payload.ok == true' >/dev/null 2>&1; then
        # `receivers` (the honest-send fix) is what actually answers "did
        # this land anywhere" — `ok` alone no longer means that: a send
        # with nobody subscribed used to ack ok too. Gate on it here.
        if printf '%s' "$resp" | jq -e '.payload.receivers | type == "array"' >/dev/null 2>&1; then
            local -a receivers; mapfile -t receivers < <(printf '%s' "$resp" | jq -r '.payload.receivers[]')
            if [ -z "$1" ]; then
                # --all: there's no single named recipient to check for —
                # report the count, even zero (that's the honest answer).
                echo "relayed -> <all> (${#receivers[@]} receiver(s)) via $ENDPOINT"
                return 0
            fi
            # A Windows-hosted handle runs no bridge — its frontend files
            # every frame into its own inbox instead, declared as
            # `fe@<host>` (comm-listen.sh, comm-send.sh:62's lookup
            # pattern) — so a target whose OWN row is that frontend still
            # counts as reached even though its exact name never appears.
            local want_fe="" thost
            thost="$(jq -r --arg n "$1" '.agents[$n].host // empty' "$REGISTRY" 2>/dev/null)"
            [ -n "$thost" ] && want_fe="fe@$thost"
            # A handle on ANOTHER box is never in this registry (each box
            # keeps its own), so the lookup above cannot name its frontend
            # and a delivered send read as "no receiver" (rc.33 regression,
            # 2026-09-17). Derived handles end in -<host>, so a frontend
            # receiver whose host the target's name ends with is that
            # handle's own frontend: count it.
            local r
            for r in "${receivers[@]}"; do
                if [ "$r" = "$1" ] || { [ -n "$want_fe" ] && [ "$r" = "$want_fe" ]; } \
                    || { [ -z "$thost" ] && [ "${r#fe@}" != "$r" ] && [ "${1%-${r#fe@}}" != "$1" ]; }; then
                    echo "relayed -> $1 via $ENDPOINT"
                    return 0
                fi
            done
            # Nothing in the roster names the target. That is NOT proof of
            # absence, and it used to be reported as one. The roster belongs
            # to the daemon THIS send reached, and a handle on another box
            # registers on ITS OWN daemon — so for a cross-box target this
            # branch is the normal case, not a fault, and the frame is
            # forwarded and delivered regardless. Measured from both ends
            # 2026-09-25: this line printed with exit 1 while the message
            # landed in the target's inbox one second later and woke its
            # pane. The relay is live-only and a reply is the only proof of
            # delivery — which the docs already say — so a check that cannot
            # prove absence warns and does not fail. The old "use durable
            # delivery instead" advice is deleted with it: on the box where
            # this fires, comm-send.sh cannot address that name either, so
            # it sent obedient sessions down a route that always fails and
            # more than one concluded the backend was dead.
            echo "WARN: this daemon's roster does not name $1 — expected for a handle on another box. The frame was accepted; only a reply proves delivery." >&2
            echo "relayed -> $1 via $ENDPOINT"
            return 0
        fi
        # `receivers` absent: an OLD daemon answered — it can't prove a
        # receiver either way, so this is NOT a success to report; fall
        # through to the same WARN/failure branch as no-ack.
    fi
    echo "WARN: no ack from daemon — the message may NOT have been delivered." >&2
    echo "      Retry, or use durable delivery: comm-send.sh @<name> \"msg\"" >&2
    echo "      (If every send does this, the daemon may predate agent.send.)" >&2
    return 1
}

# Filter inbound frames for agent.message addressed to me (or broadcast).
# Reads frames on stdin; emits one compact JSON line per matching message.
filter_inbound() {
    while IFS= read -r line; do
        [ -z "$line" ] && continue
        local op to from; op="$(printf '%s' "$line" | jq -r '.op // empty' 2>/dev/null || true)"
        [ "$op" = "agent.message" ] || continue
        from="$(printf '%s' "$line" | jq -r '.payload.from // ""' 2>/dev/null || true)"
        [ "$from" = "$NAME" ] && continue   # drop our own broadcasts (self-echo)
        to="$(printf '%s' "$line" | jq -r '.payload.to // ""' 2>/dev/null || true)"
        [ "$to" = "" ] || [ "$to" = "$NAME" ] || continue
        printf '%s\n' "$line"
    done
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
        send_frame "$TO" "$MSG"
        echo "listening ${SECS}s for replies..."
        nc_hold "$SECS" | filter_inbound | while IFS= read -r m; do
            printf '[%s] [%s] %s\n' \
                "$(printf '%s' "$m" | jq -r '.payload.ts')" \
                "$(printf '%s' "$m" | jq -r '.payload.from')" \
                "$(printf '%s' "$m" | jq -r '.payload.text')"
        done
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
            printf '%s' "$m" | jq -c '{from:.payload.from, to:(.payload.to // ""), repo:"daemon", msg:.payload.text, ts:.payload.ts}' \
                >> "$INBOX_DIR/$NAME.jsonl"
        done
        ;;
    *)
        echo "usage: comm-relay.sh {send|ask|listen|bridge} ..." >&2; exit 1 ;;
esac
