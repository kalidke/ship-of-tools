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

ENDPOINT="${SOT_RELAY_ENDPOINT:-}"
resolve_endpoint() {
    sot_daemon_endpoint "${ENDPOINT:-${SOT_SPAWN_ENDPOINT:-}}"
}
# nc preferred; on hosts without it (e.g. git-bash on Windows, which ships no
# nc) fall back to bash's /dev/tcp for tcp endpoints. unix-socket endpoints
# still require nc -U (/dev/tcp can't speak AF_UNIX).
HAVE_NC=0; command -v nc >/dev/null 2>&1 && HAVE_NC=1
ENDPOINT="$(resolve_endpoint)" || { echo "ERROR: no sotd daemon found; set SOT_RELAY_ENDPOINT=unix:/path or tcp:HOST:PORT" >&2; exit 1; }
EP_HOST=""; EP_PORT=""; EP_UNIX=""
case "$ENDPOINT" in
    tcp:*)  hp="${ENDPOINT#tcp:}"; EP_HOST="${hp%:*}"; EP_PORT="${hp##*:}" ;;
    unix:*) EP_UNIX="${ENDPOINT#unix:}" ;;
    *) echo "ERROR: bad endpoint '$ENDPOINT'" >&2; exit 1 ;;
esac

# App-level auth (ADR 0010 hardening). The daemon now requires a token-valid
# `hello` before serving ANY op, so every connection below sends one first.
# Token source: $SOT_TOKEN, else the 0600 token file in the (700) home. Empty in
# open-config mode — an empty token still authenticates there (gate is off). The
# hello reply is an extra line on the wire, but every caller greps by op, so it
# is ignored. client_id "sot-comm" so the roster/logs show what it is.
_sot_hello() {
    local tok; tok="${SOT_TOKEN:-$(cat "${XDG_CONFIG_HOME:-$HOME/.config}/sot/token" 2>/dev/null || true)}"
    printf '{"v":1,"id":1,"kind":"req","op":"hello","payload":{"client_id":"sot-comm","last_seen_revision":0,"protocol":1,"app_version":"comm","token":"%s"}}\n' "$tok"
}

# nc_out: send the single frame on stdin, return immediately (capture any reply line)
nc_send() {
    if [ "$HAVE_NC" = 1 ]; then
        if [ -n "$EP_UNIX" ]; then { _sot_hello; cat; } | timeout 5 nc -U "$EP_UNIX"; else { _sot_hello; cat; } | timeout 5 nc "$EP_HOST" "$EP_PORT"; fi
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
            { _sot_hello; cat; } >&9
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
nc_hold() {
    local secs="${1:-}"
    if [ -n "$EP_HOST" ]; then
        if exec 9<>"/dev/tcp/$EP_HOST/$EP_PORT" 2>/dev/null; then
            _sot_hello >&9   # authenticate the connection before holding it open
            if [ -n "$secs" ]; then timeout "$secs" cat <&9; else cat <&9; fi
            exec 9<&- 9>&- 2>/dev/null || true
            return 0
        fi
        # bash built without /dev/tcp: fall back to nc. NOTE: this form does NOT
        # self-heal on a graceful close — prefer a /dev/tcp-capable bash for bridges.
        if [ "$HAVE_NC" = 1 ]; then
            if [ -n "$secs" ]; then { _sot_hello; tail -f /dev/null; } | timeout "$secs" nc "$EP_HOST" "$EP_PORT"
            else { _sot_hello; tail -f /dev/null; } | nc "$EP_HOST" "$EP_PORT"; fi
            return 0
        fi
        echo "ERROR: cannot open /dev/tcp/$EP_HOST/$EP_PORT and nc not found" >&2; return 1
    fi
    # Unix-socket endpoint: requires nc -U (/dev/tcp can't speak AF_UNIX).
    if [ -n "$EP_UNIX" ] && [ "$HAVE_NC" = 1 ]; then
        if [ -n "$secs" ]; then { _sot_hello; tail -f /dev/null; } | timeout "$secs" nc -U "$EP_UNIX"
        else { _sot_hello; tail -f /dev/null; } | nc -U "$EP_UNIX"; fi
        return 0
    fi
    echo "ERROR: nc not found and endpoint is a unix socket (needs nc -U)" >&2; return 1
}

send_frame() {  # $1 to, $2 text
    local frame; frame="$(jq -nc --arg f "$NAME" --arg t "$1" --arg m "$2" \
        '{v:1,id:1,kind:"req",op:"agent.send",payload:{from:$f,to:$t,text:$m}}')"
    local resp; resp="$(printf '%s\n' "$frame" | nc_send 2>/dev/null | grep -m1 '"op":"agent.send"' || true)"
    if printf '%s' "$resp" | jq -e '.payload.ok == true' >/dev/null 2>&1; then
        echo "relayed -> ${1:-<all>} via $ENDPOINT"
    else
        echo "WARN: no ack from daemon — the message may NOT have been delivered." >&2
        echo "      Retry, or use durable delivery: comm-send.sh @<name> \"msg\"" >&2
        echo "      (If every send does this, the daemon may predate agent.send.)" >&2
    fi
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

SUB="${1:-}"; [ $# -gt 0 ] && shift || true
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
