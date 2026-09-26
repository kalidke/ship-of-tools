#!/usr/bin/env bash
# comm-send.sh — send a message to one agent or broadcast to all.
# Usage: comm-send.sh @name "message"
#        comm-send.sh --broadcast "message"
#
# Every send lands in the recipient's durable inbox, and THAT is the
# acknowledgement: a filed frame is read by the recipient's next turn boundary
# (its Stop hook reads its own inbox), so `filed -> @name` is the verdict and
# exit 0 means it. A directed send to a row on THIS host is additionally POKED
# — one gated keystroke line (comm-lib.sh's sot_pty_input_gated) for a
# genuinely idle row, since a stopped agent is blocked on stdin and keystrokes
# are the only way in. The poke is diagnostic only: `+woken` / `not woken:
# <reason>` never changes the verdict.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/comm-lib.sh"
eval "$("$SCRIPT_DIR/comm-context.sh")"
ensure_home

BROADCAST=false; TARGET=""; MSG=""
while [ $# -gt 0 ]; do
    case "$1" in
        --broadcast) BROADCAST=true; shift; continue ;;
    esac
    # The recipient is ONLY the first positional @arg, taken before any message
    # text. Once a target/broadcast is set or message text has started, an
    # @arg is message content verbatim — agents naturally open replies with
    # an @mention, so the message must be allowed to begin with @.
    if [ -z "$TARGET" ] && [ -z "$MSG" ] && [ "$BROADCAST" = false ] && [ "${1#@}" != "$1" ]; then
        TARGET="${1#@}"
    elif [ -z "$MSG" ]; then
        MSG="$1"
    else
        MSG="$MSG $1"
    fi
    shift
done

[ -z "$MSG" ] && { echo "usage: comm-send.sh @name \"msg\" | --broadcast \"msg\"" >&2; exit 1; }

# MSYS2 argv-conversion guard (comm-lib.sh's sot_jq_rawfile): MSG can
# legitimately start with "/" (an agent naturally opens with a slash
# command) and must never reach jq via --arg. Computed ONCE here (not per
# recipient inside deliver()) since a --broadcast fans this same MSG out
# to every target; cleaned up on exit however this script leaves.
MSG_FILE="$(sot_jq_rawfile "$MSG")" || exit 1
trap 'rm -f "$MSG_FILE"' EXIT

# Identity refusal, via the ONE shared helper (comm-lib.sh) also used by
# comm-relay.sh and comm-bootstrap.sh: requires more than a merely nonempty
# NAME — see the helper's own comment. Checked BEFORE any transport work so
# an unresolved sender always sees THIS refusal, never a daemon error.
sot_require_routable_identity || exit 1

FORMATTED="[${NAME:-?}:$REPO] $MSG"

# The daemon that owns this host's rows, resolved once and only when a
# live delivery is actually attempted: a broadcast never types into anyone,
# and a shell with no daemon still files to the inbox.
ENDPOINT=""
_live_endpoint() {
    [ -n "$ENDPOINT" ] && return 0
    ENDPOINT="$(sot_daemon_endpoint 2>/dev/null)" && [ -n "$ENDPOINT" ]
}

deliver() {  # $1 = target name
    local t="$1" thost tws ts resp ok enter_sent code
    thost="$(jq -r --arg n "$t" '.agents[$n].host         // empty' "$REGISTRY")"
    tws="$(jq -r   --arg n "$t" '.agents[$n].workspace_id // empty' "$REGISTRY")"
    if [ -z "$thost" ]; then echo "no such handle: $t" >&2; return 1; fi

    # 1) durable inbox, always — this append IS the delivery. Stamp `to` so the
    # recipient can rank:
    # a directed send (to == their own name) wakes the session; a broadcast
    # copy (to == "") files silently for comm-poll — the same demotion rule
    # the relay bridge applies. Lines without a `to` key (pre-stamp senders)
    # read as directed, which is why a --broadcast used to wake the whole
    # network (observed 2026-06-12: an @sot help blast woke every session).
    local to_stamp="$t"
    [ "$BROADCAST" = true ] && to_stamp=""
    ts="$(now_iso)"
    jq -nc --arg from "$NAME" --arg to "$to_stamp" --arg repo "$REPO" --rawfile msg "$MSG_FILE" --arg ts "$ts" \
        '{from:$from, to:$to, repo:$repo, msg:$msg, ts:$ts}' >> "$INBOX_DIR/$t.jsonl"

    # 2) the poke. The frame is already filed, so this is no longer delivery:
    # it only shortens the wait for a row that is sitting idle at its prompt.
    # GATED (sot_pty_input_gated): typing plus Enter submits into whatever is on
    # screen, so a dialog, a menu or a half-typed draft is never typed over. A
    # busy session needs no poke at all — its Stop hook reads the inbox at the
    # turn boundary. Broadcasts never type into anyone (a --broadcast blast once
    # woke every session on the network, 2026-06-12).
    local woke=""
    if [ "$BROADCAST" = true ]; then
        woke=""
    elif [ "$thost" != "$HOST" ]; then
        woke=" — not woken: row is on $thost, read at its next turn boundary"
    elif [ -z "$tws" ]; then
        woke=" — not woken: no workspace row"
    elif ! _live_endpoint; then
        woke=" — not woken: no daemon reachable from here"
    else
        local gate_rc=0
        sot_pty_input_gated "$tws" "$(printf '%s' "$FORMATTED" | base64 | tr -d '\n')" || gate_rc=$?
        case "$gate_rc" in
            0) woke=" +woken" ;;
            1) woke=" — not woken: row $tws is not at a free prompt" ;;
            *) woke=" — not woken: row $tws did not answer" ;;
        esac
    fi
    # 3) the recipient annotation (messaging ruling, 2026-09-26): one factual
    # clause read off the same registry entry `deliver` already has open --
    # never a second file, never the daemon. Empty (missing/malformed entry
    # or fields) means no clause, never a guess (sot_recipient_note's own
    # contract) -- the send's success is unaffected either way.
    local note=""
    note="$(sot_recipient_note "$t" 2>/dev/null)" || note=""
    [ -n "$note" ] && note=" ($note)"
    echo "  filed -> @$t$woke$note"
    return 0
}

if [ "$BROADCAST" = true ]; then
    mapfile -t TARGETS < <(jq -r --arg me "$NAME" '.agents | keys[] | select(. != $me)' "$REGISTRY")
    n=0
    for t in "${TARGETS[@]}"; do [ -n "$t" ] && { deliver "$t" || true; n=$((n + 1)); }; done
    echo "Broadcast to $n agent(s)."
else
    [ -z "$TARGET" ] && { echo "no target; use @name or --broadcast" >&2; exit 1; }
    deliver "$TARGET"
fi

with_lock registry_touch "$NAME" 2>/dev/null || true
