#!/usr/bin/env bash
# comm-send.sh — send a message to one agent or broadcast to all.
# Usage: comm-send.sh @name "message"
#        comm-send.sh --broadcast "message"
#
# `filed -> @name` means the line is in the inbox of a handle a live session
# holds, and is the whole verdict: a filed frame is read by the recipient's
# next turn boundary (its Stop hook reads its own inbox), and exit 0 means it.
# A listed handle that is not live (its registry last_seen is not under
# COMM_LIVE_SECS old) is refused here, before anything is appended. The
# append is comm-lib.sh's sot_inbox_append: under the inbox lock, or by the
# daemon that owns the comm folder when this box cannot prove it takes the
# same lock; one that cannot be made prints `FAILED -> @name: <why>` and
# exits 1, never `filed`. The daemon wakes an idle row; this script types into nobody.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/comm-lib.sh"

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

# A second agent inside the session never sends as the row's handle. The gate
# comes before the context call (which can heal a self file or create the
# registry), so a refused child touches nothing.
if ! why="$(sot_require_agent)"; then
    if [ "$BROADCAST" = true ]; then echo "FAILED -> --broadcast: $why" >&2; else echo "FAILED -> @$TARGET: $why" >&2; fi
    exit 1
fi
eval "$("$SCRIPT_DIR/comm-context.sh")"

# MSYS2 argv-conversion guard (comm-lib.sh's sot_jq_rawfile): MSG can
# legitimately start with "/" (an agent naturally opens with a slash
# command) and must never reach jq via --arg. Computed ONCE here (not per
# recipient inside deliver()) since a --broadcast fans this same MSG out
# to every target; cleaned up on exit however this script leaves.
MSG_FILE="$(sot_jq_rawfile "$MSG")" || exit 1
trap 'rm -f "${MSG_FILE:?}"' EXIT

# Identity refusal, via the ONE shared helper (comm-lib.sh) also used by
# comm-relay.sh and comm-bootstrap.sh (here without its agent gate, which ran
# above, so a send walks the ancestry once): requires more than a merely nonempty
# NAME — see the helper's own comment. Checked BEFORE any transport work so
# an unresolved sender always sees THIS refusal, never a daemon error.
if ! why="$(_sot_identity_routable)"; then
    if [ "$BROADCAST" = true ]; then echo "FAILED -> --broadcast: $why" >&2; else echo "FAILED -> @$TARGET: $why" >&2; fi
    exit 1
fi

deliver() {  # $1 = target name
    local t="$1" thost ts row reg_rc=0
    row="$(sot_registry_read "$t")" || reg_rc=$?
    if [ "$reg_rc" -ge 2 ]; then
        # Unreadable is not a miss: handing a LOCAL target to the relay would
        # route it by a registry nobody read.
        echo "FAILED -> @$t: the registry could not be read, so @$t could not be routed; nothing was sent" >&2
        return 1
    fi
    thost=""
    [ "$reg_rc" -eq 0 ] && { thost="$(printf '%s' "$row" | sot_jq -r '.host // empty' 2>/dev/null)" || thost=""; }
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

    # 0) liveness: `filed` is for a live handle. The registry's last_seen is the
    # one fact (the session stamps it, and the daemon that runs its row stamps it
    # every minute), so a handle that is not live is refused here with the
    # daemon's own sentence, before anything is built or appended. No daemon is
    # asked.
    local last_seen=""
    last_seen="$(printf '%s' "$row" | sot_jq -r '.last_seen | strings | select(length == 20)' 2>/dev/null)" || last_seen=""
    if ! sot_heartbeat_fresh "$last_seen"; then
        echo "FAILED -> @$t: no live session holds @$t" >&2
        return 1
    fi

    # 1) durable inbox — this append IS the delivery, made by the one helper
    # that appends (sot_inbox_append), under the inbox lock or by the daemon;
    # a refused lock, a failed write or a daemon that does not file is FAILED,
    # never `filed`. Stamp `to` so
    # the recipient can rank:
    # a directed send (to == their own name) wakes the session; a broadcast
    # copy (to == "") files silently for comm-poll -- the same demotion rule
    # the daemon wake applies. A line without a `to` string is never counted as
    # mail (the wake's rule), so every send stamps one.
    local to_stamp="$t"
    [ "$BROADCAST" = true ] && to_stamp=""
    ts="$(now_iso)"
    local reason
    if ! reason="$(jq -nc --arg from "$NAME" --arg to "$to_stamp" --arg repo "$REPO" --rawfile msg "$MSG_FILE" --arg ts "$ts" \
        '{from:$from, to:$to, repo:$repo, msg:$msg, ts:$ts}' | sot_inbox_append "$t")"; then
        echo "FAILED -> @$t: $reason" >&2
        return 1
    fi

    echo "  filed -> @$t"
    return 0
}

if [ "$BROADCAST" = true ]; then
    REG="$(sot_registry_read)" || { echo "FAILED -> --broadcast: the registry could not be read; nothing was sent" >&2; exit 1; }
    mapfile -t TARGETS < <(printf '%s' "$REG" | sot_jq -r --arg me "$NAME" '.agents | keys[] | select(. != $me)')
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

SOT_LOCK_WAIT_SECS=1 with_lock registry_touch "$NAME" 2>/dev/null || true
if [ "$BROADCAST" = true ] && [ "$n" -ne "$of" ]; then exit 1; fi
