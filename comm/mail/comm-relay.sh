#!/usr/bin/env bash
# comm-relay.sh — the send for a handle this box's registry does not list. It is ONE `comm.file` request to this
# box's relay endpoint (`sotd topology relay-endpoint`: the hub, or on the hub its own daemon), and the answer is the
# verdict: `filed -> @h`, or `FAILED -> @h: <the daemon's own words>`. The hub files a handle its own comm folder
# lists, and offers any other to the daemons linked to it (each box with its own comm folder); the one whose folder
# lists the handle appends and answers. A handle this box's registry lists is comm-send.sh's.
#
# ENDPOINT: what `sotd topology relay-endpoint` answers for this box (comm-lib's sot_relay_endpoint), and nothing
# else; no variable overrides it. unix:/path, ssh:target[/host], or, on Windows, pipe:name (ADR 0042 amendment
# decision 5). Every one is opened by comm-lib's sot_dial, which connects only to an endpoint this OS account serves
# (ADR 0049, User isolation).
#
# Usage:
#   comm-relay.sh send @to "message"
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
# Before comm-context.sh, which can write: nothing is written without jq. A send says so in the protocol's one
# failure form.
if ! why="$(sot_require_tools "build a daemon request" jq 2>&1)"; then
    if [ "${1:-}" = send ]; then to="${2:-}"; echo "FAILED -> @${to#@}: $why" >&2; else echo "$why" >&2; fi
    exit 1
fi
eval "$("$SCRIPT_DIR/comm-context.sh")"

usage() { echo "usage: comm-relay.sh send @to \"msg\"  ($1)" >&2; exit 1; }
[ "${1:-}" = send ] || usage "the one verb is send"
shift
# The first argument that starts with '@' is the recipient; every other argument, a later '@…' included, is message
# text, so a message may begin with "@peer …".
TO=""; MSG=""; TO_SET=false
while [ $# -gt 0 ]; do
    case "$1" in
        @*) if [ "$TO_SET" = false ]; then TO="${1#@}"; TO_SET=true; else MSG="${MSG:+$MSG }$1"; fi ;;
        *)  MSG="${MSG:+$MSG }$1" ;;
    esac
    shift
done
[ -n "$TO" ] || usage "no recipient"
[ -n "$MSG" ] || usage "empty message"

# `send` needs a routable identity to stamp a from-field that means anything, and that check runs before the
# endpoint is resolved: an unresolved sender on a box with no reachable daemon sees the identity refusal, which is
# the fixable problem, and never only "no sotd daemon found".
why="$(sot_require_routable_identity)" || { echo "FAILED -> @$TO: $why" >&2; exit 1; }

# FILE-FIRST: a target with a row in THIS box's registry shares this $SOT_COMM_HOME, so comm-send.sh files it. Both
# doors ask the one question `deliver()` refuses on, does this box's registry give the target a HOST?, or a send
# passes between the two execs forever.
# 0 a hit, 1 a miss, 2 the registry could not be read (never a miss).
_registry_target() {
    local row rc=0
    row="$(sot_registry_read "$1")" || rc=$?
    [ "$rc" -eq 0 ] || return "$rc"
    [ -n "$(printf '%s' "$row" | sot_jq -r '.host // empty' 2>/dev/null)" ]
}
rc=0; _registry_target "$TO" || rc=$?
case "$rc" in
    0) exec "$SCRIPT_DIR/comm-send.sh" "@$TO" "$MSG" ;;
    1) ;;
    *) echo "FAILED -> @$TO: the registry could not be read, so @$TO could not be routed; nothing was sent" >&2; exit 1 ;;
esac

ENDPOINT="$(sot_relay_endpoint || true)"
if [ -z "$ENDPOINT" ]; then
    echo "FAILED -> @$TO: no sotd daemon found; \`sotd topology relay-endpoint\` gave no endpoint for this box, or no sotd is installed" >&2
    exit 1
fi
# The text reaches jq on stdin, never argv: it can start with "/" (comm-lib's MSYS2 argv-conversion guard).
filing="$(printf '%s' "$MSG" | jq -Rsc --arg f "$NAME" --arg t "$TO" '{from:$f,to:$t,msg:.}')"
rc=0; reason="$(sot_comm_file "$TO" "$filing")" || rc=$?
if [ "$rc" -eq 0 ]; then
    echo "filed -> @$TO"
    exit 0
fi
echo "FAILED -> @$TO: $reason" >&2
exit 1
