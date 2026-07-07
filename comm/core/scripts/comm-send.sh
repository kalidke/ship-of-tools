#!/usr/bin/env bash
# comm-send.sh — send a message to one agent or broadcast to all.
# Usage: comm-send.sh @name "message"
#        comm-send.sh --broadcast "message"
#        comm-send.sh --force-target SESSION:WIN.PANE "message"   # first contact, no registry
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/comm-lib.sh"
eval "$("$SCRIPT_DIR/comm-context.sh")"
ensure_home
# Private tmux socket (security review) — the live-paste delivery legs below
# target daemon-created panes, which live on the daemon's private socket,
# not tmux's default server. Resolved once, used on every `tmux` call below
# via `-S`.
SOT_TMUX_SOCK="$(sot_tmux_socket)" \
    || { echo "ERROR: could not resolve/secure the private tmux socket dir — see reason above" >&2; exit 1; }

BROADCAST=false; TARGET=""; MSG=""; FORCE_TARGET=""
while [ $# -gt 0 ]; do
    case "$1" in
        --broadcast)    BROADCAST=true; shift; continue ;;
        --force-target) FORCE_TARGET="$2"; shift 2; continue ;;
    esac
    # The recipient is ONLY the first positional @arg, taken before any message
    # text. Once a target/broadcast/force is set or message text has started,
    # an @arg is message content verbatim — agents naturally open replies with
    # an @mention, so the message must be allowed to begin with @.
    if [ -z "$TARGET" ] && [ -z "$MSG" ] && [ "$BROADCAST" = false ] \
       && [ -z "$FORCE_TARGET" ] && [ "${1#@}" != "$1" ]; then
        TARGET="${1#@}"
    elif [ -z "$MSG" ]; then
        MSG="$1"
    else
        MSG="$MSG $1"
    fi
    shift
done

[ -z "$MSG" ] && { echo "usage: comm-send.sh @name \"msg\" | --broadcast \"msg\" | --force-target T \"msg\"" >&2; exit 1; }
[ -z "$NAME" ] && NAME="unknown-$HOST"

FORMATTED="[$NAME:$REPO] $MSG"

# Raw delivery to a tmux target with no registry lookup — for first contact with
# a session that hasn't joined yet. No inbox (no known recipient name).
if [ -n "$FORCE_TARGET" ]; then
    sess="${FORCE_TARGET%%:*}"
    if ! tmux -S "$SOT_TMUX_SOCK" has-session -t "$sess" 2>/dev/null; then
        echo "ERROR: tmux session '$sess' not found" >&2; exit 1
    fi
    f="$(mktemp "${TMPDIR:-/tmp}/comm-send.XXXXXX")"
    printf '%s' "$FORMATTED" > "$f"
    tmux -S "$SOT_TMUX_SOCK" load-buffer "$f"; tmux -S "$SOT_TMUX_SOCK" paste-buffer -t "$FORCE_TARGET"; rm -f "$f"
    sleep 0.3; tmux -S "$SOT_TMUX_SOCK" send-keys -t "$FORCE_TARGET" Enter
    echo "Sent to $FORCE_TARGET (force-target, no registry)."
    exit 0
fi

deliver() {  # $1 = target name
    local t="$1" thost tpane ttmux ts f
    thost="$(jq -r --arg n "$t" '.agents[$n].host    // empty' "$REGISTRY")"
    tpane="$(jq -r --arg n "$t" '.agents[$n].pane_id // empty' "$REGISTRY")"
    ttmux="$(jq -r --arg n "$t" '.agents[$n].tmux    // empty' "$REGISTRY")"
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
    jq -nc --arg from "$NAME" --arg to "$to_stamp" --arg repo "$REPO" --arg msg "$MSG" --arg ts "$ts" \
        '{from:$from, to:$to, repo:$repo, msg:$msg, ts:$ts}' >> "$INBOX_DIR/$t.jsonl"

    # 2) live paste, only if same host, pane alive, and NOT a broadcast.
    # A paste + Enter is a full interrupt (it submits into the recipient's
    # claude input — a model turn), so it must follow the same demotion rule
    # as the Monitor: broadcasts file silently, only directed sends interrupt.
    # The 2026-06-12 wake-storm fix originally stamped to:"" on the inbox line
    # but left this leg pasting — every same-host session still got woken per
    # broadcast (one peer was hit through exactly this path). Worse, a paste
    # into a pane whose claude has exited lands at a bash PROMPT and the
    # Enter executes message text as shell input.
    if [ "$BROADCAST" != true ] && [ "$thost" = "$HOST" ] && [ -n "$tpane" ] && [ -n "$ttmux" ] \
       && tmux -S "$SOT_TMUX_SOCK" list-panes -a -F '#{pane_id}' 2>/dev/null | grep -qx "$tpane"; then
        f="$(mktemp "${TMPDIR:-/tmp}/comm-send.XXXXXX")"
        printf '%s' "$FORMATTED" > "$f"
        tmux -S "$SOT_TMUX_SOCK" load-buffer "$f"; tmux -S "$SOT_TMUX_SOCK" paste-buffer -t "$ttmux"; rm -f "$f"
        sleep 0.3; tmux -S "$SOT_TMUX_SOCK" send-keys -t "$ttmux" Enter
        echo "  @$t: delivered live (+inbox)"
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
