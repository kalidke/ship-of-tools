#!/usr/bin/env bash
# comm-send.sh — send a message to one agent or broadcast to all.
# Usage: comm-send.sh @name "message"
#        comm-send.sh --broadcast "message"
#
# Every send lands in the recipient's durable inbox. A directed send to a
# session that owns a workspace row on THIS host is also typed live into
# that row (the daemon's `pty.input`, Enter appended — the same path
# codex-watch.sh uses). With no row on this host the message stays in the
# inbox (the recipient's Monitor or bridge picks it up) and the send says so.
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
    if [ -z "$thost" ]; then echo "  @$t: not in registry — skipped" >&2; return 1; fi

    # 1) durable inbox, always. Stamp `to` so the recipient's Monitor can rank:
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

    # 2) live typing, only for a directed send to a row on this host. Typing
    # plus Enter is a full interrupt (it submits into the recipient's input —
    # a model turn), so it follows the same demotion rule as the Monitor:
    # broadcasts file silently, only directed sends interrupt. The daemon
    # refuses a row whose capsule is not ready, so the text never lands at a
    # bare shell prompt; an unknown or gone row leaves the message queued.
    if [ "$BROADCAST" != true ] && [ "$thost" = "$HOST" ] && [ -n "$tws" ] && _live_endpoint; then
        resp="$(sot_pty_input "$tws" "$(printf '%s' "$FORMATTED" | base64 | tr -d '\n')" || true)"
        IFS='|' read -r ok enter_sent code <<EOF
$(printf '%s' "$resp" | jq -r '[.payload.ok // false, .payload.enter_sent // false, .payload.code // ""] | map(tostring) | join("|")' 2>/dev/null)
EOF
        if [ "$ok" = true ] && [ "$enter_sent" = true ]; then
            echo "  @$t: delivered live (+inbox)"
        elif [ "$ok" = true ]; then
            echo "  @$t: typed live, Enter unconfirmed (+inbox)"
        else
            echo "  @$t: queued to inbox (row $tws: ${code:-no reply})"
        fi
    else
        echo "  @$t: queued to inbox ($thost)"
    fi
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
