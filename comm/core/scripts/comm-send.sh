#!/usr/bin/env bash
# comm-send.sh — send a message to one agent or broadcast to all.
# Usage: comm-send.sh @name "message"
#        comm-send.sh --broadcast "message"
#
# Every send lands in the recipient's durable inbox, and THAT is the
# acknowledgement: a filed frame is read by the recipient's next turn boundary
# (its Stop hook reads its own inbox), so `filed -> @name` is the verdict and
# exit 0 means it. The append is comm-lib.sh's sot_inbox_append: under the
# inbox lock, or by the daemon that owns the comm folder when this box cannot
# prove it takes the same lock; one that cannot be made prints
# `FAILED -> @name: <why>` and exits 1, never `filed`. A directed send to a row on THIS host is additionally POKED
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
trap 'rm -f "${MSG_FILE:?}"' EXIT

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
    thost="$(sot_jq -r --arg n "$t" '.agents[$n].host         // empty' "$REGISTRY")"
    if [ -z "$thost" ]; then
        # Registry MISS on a DIRECTED send: this box cannot name the target.
        # "Miss" is exactly `.host` empty-or-absent, and comm-relay.sh's
        # `_registry_target` asks that SAME question (see its comment) — if
        # the two ever diverge again the pair ping-pongs forever.
        # That is the routine case for a session on a machine that shares no
        # $HOME with this one -- it can never have a row here -- not a typo,
        # and refusing here is what forced a session to know which verb
        # routes. Hand it to the relay instead; comm-relay.sh's own `send`
        # execs BACK here on a registry HIT, and the two triggers are
        # mutually exclusive (hit -> file, miss -> wire), so no ping-pong is
        # possible and no guard env var is needed.
        #
        # Directed only. A --broadcast fans out over registry KEYS, every one
        # of which has a row, so this branch is unreachable there -- and an
        # `exec` in the middle of that fan-out would abandon every remaining
        # target.
        if [ "$BROADCAST" = false ]; then
            # The EXIT trap does not run across an `exec` (the process image
            # is replaced), so drop the temp file by hand first.
            rm -f "${MSG_FILE:?}"
            exec "$SCRIPT_DIR/comm-relay.sh" send "@$t" "$MSG"
        fi
        echo "no such handle: $t" >&2; return 1
    fi

    # 1) durable inbox — this append IS the delivery, made by the one helper
    # that appends (sot_inbox_append), under the inbox lock or by the daemon;
    # a refused lock, a failed write or a daemon that does not file is FAILED,
    # never `filed`. Stamp `to` so
    # the recipient can rank:
    # a directed send (to == their own name) wakes the session; a broadcast
    # copy (to == "") files silently for comm-poll — the same demotion rule
    # the ping watcher applies. Lines without a `to` key (pre-stamp senders)
    # read as directed, which is why a --broadcast used to wake the whole
    # network (observed 2026-06-12: an @sot help blast woke every session).
    local to_stamp="$t"
    [ "$BROADCAST" = true ] && to_stamp=""
    ts="$(now_iso)"
    local reason
    if ! reason="$(jq -nc --arg from "$NAME" --arg to "$to_stamp" --arg repo "$REPO" --rawfile msg "$MSG_FILE" --arg ts "$ts" \
        '{from:$from, to:$to, repo:$repo, msg:$msg, ts:$ts}' | sot_inbox_append "$t")"; then
        echo "FAILED -> @$t: $reason" >&2
        return 1
    fi

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
    elif ! _live_endpoint; then
        woke=" — not woken: no daemon reachable from here"
    else
        # THE handle->row binding, asked of the daemon HERE, not read from the
        # registry field a join stamped once (comm-join.sh) and nothing ever
        # refreshes (comm-status.sh's "never clobbers workspace_id"). A session
        # that continues in another row kept waking the row it used to be in.
        # 0 or 2+ matches REFUSE: a poke aimed by a guess types into whatever
        # row the guess names. Every rc is captured with `|| rc=$?` because
        # `set -e` is in force here -- a bare assignment would abort the whole
        # send on a refusal, losing the receipt for a frame that WAS filed.
        local row_rc=0
        tws="$(sot_wake_row "$t")" || row_rc=$?
        case "$row_rc" in
            0)
                local gate_rc=0
                sot_pty_input_gated "$tws" "$(printf '%s' "$FORMATTED" | base64 | tr -d '\n')" || gate_rc=$?
                case "$gate_rc" in
                    0) woke=" +woken" ;;
                    1) woke=" — not woken: row $tws is not at a free prompt" ;;
                    3) woke=" — not woken: row $tws is gone (the daemon has no such row)" ;;
                    *) woke=" — not woken: row $tws did not answer" ;;
                esac
                ;;
            1) woke=" — not woken: no live row declares @$t" ;;
            3) woke=" — not woken: two or more rows declare @$t" ;;
            *) woke=" — not woken: the daemon did not answer" ;;
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
    mapfile -t TARGETS < <(sot_jq -r --arg me "$NAME" '.agents | keys[] | select(. != $me)' "$REGISTRY")
    # Only a filed copy counts; deliver prints each FAILED one.
    n=0; of=0
    for t in "${TARGETS[@]}"; do
        [ -n "$t" ] || continue
        of=$((of + 1))
        if deliver "$t"; then n=$((n + 1)); fi
    done
    if [ "$n" -eq "$of" ]; then echo "Broadcast to $n agent(s)."; else echo "Broadcast to $n of $of agent(s)."; fi
else
    [ -z "$TARGET" ] && { echo "no target; use @name or --broadcast" >&2; exit 1; }
    deliver "$TARGET"
fi

with_lock registry_touch "$NAME" 2>/dev/null || true
if [ "$BROADCAST" = true ] && [ "$n" -ne "$of" ]; then exit 1; fi
