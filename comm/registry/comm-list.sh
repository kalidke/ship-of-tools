#!/usr/bin/env bash
# comm-list.sh — list registered agents with live/stale status and the
# ADE "state-nav" at-a-glance work-state: each row shows the session's WORK
# state ([working]/[idle]/[blocked]/[done]), a one-line summary, and the age
# of that status (e.g. "2m ago"), derived from .agents[<handle>].status_at.
# Older rows that predate state-nav (no state/summary/status_at) degrade to a
# neutral [idle] with no summary and no age.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/comm-lib.sh"
eval "$("$SCRIPT_DIR/comm-context.sh")"

nows="$(date -u +%s)"

# fmt_age (compact relative age) now lives in comm-lib.sh, sourced above —
# shared with sot-fe's `version` command so the one ageing rule serves
# every state-nav printer instead of two copies drifting.

REG="$(sot_registry_read)" || { echo "FAILED: the registry could not be read; nothing was listed" >&2; exit 1; }
echo "sot-comm agents  ($REGISTRY):"
any=false
# Fields are joined with US (0x1f), not tab: tab is IFS-whitespace, so an empty
# field (e.g. a row with no expertise) collapses and shifts later columns —
# which silently mis-slots state/summary/status_at. US is non-whitespace, so
# `read` preserves empty fields, and it can never occur inside the data.
while IFS=$'\037' read -r name host repo seen exp state summary status_at; do
    [ -z "$name" ] && continue
    any=true
    seens="$(date -u -d "$seen" +%s 2>/dev/null || echo 0)"
    age=$((nows - seens))
    if sot_heartbeat_fresh "$seen"; then status="live"; else status="stale ${age}s"; fi
    me=""; [ "$name" = "$NAME" ] && me="  <- me"
    printf "  @%-18s %-10s %-14s %-12s [%s]%s\n" "$name" "$host" "$repo" "$status" "$exp" "$me"

    # state-nav line: [work-state] summary · age. Degrade gracefully when the
    # row predates state-nav — no state means neutral [idle], no summary, no age.
    [ -z "$state" ] && state="idle"
    line="    [$state]"
    [ -n "$summary" ] && line="$line $summary"
    if [ -n "$status_at" ]; then
        sat="$(date -u -d "$status_at" +%s 2>/dev/null || echo 0)"
        [ "$sat" -gt 0 ] && line="$line · $(fmt_age $((nows - sat)))"
    fi
    echo "$line"
done < <(printf '%s' "$REG" | sot_jq -r '.agents | to_entries[]
        | [.key, .value.host, .value.repo, .value.last_seen, (.value.expertise | join("/")),
           (.value.state // ""), (.value.summary // ""), (.value.status_at // "")]
        | join("")')

if [ "$any" = false ]; then echo "  (none)"; fi

# --- attached frontend boxes ------------------------------------------------
# The registry above holds the handles THIS box can name. A frontend on another
# box attaches to this daemon and files for its own sessions, but nothing
# declares those handles here yet, so a fleet manager reading only the registry
# sees silence where there are live sessions. This section names the BOXES the
# daemon is already talking to, so the silence is legible.
#
# `sot-fe version` is that roster, published by the daemon itself. It reports no
# idle age on purpose (deleted by the 2026-09-08 review: deriving "how idle" in
# shell would duplicate the daemon's own expiry and tie-break policy), so
# nothing here invents a last-seen it cannot know; `active` marks the one
# connection an untargeted frontend command would reach right now. Bounded and
# best-effort: this is a listing, and it must not hang or fail because a daemon
# is slow or absent.
#
# Session-listing brief: each `fe@<host>` row `sot-fe version` prints is now
# followed by its own INDENTED lines — one `@<handle> [state] summary · age`
# per session that box's frontend declared (`fe.sessions`), or one
# "(declares no sessions — older frontend, or no daemon on that box)" line
# when that box has never declared at all. A plain `grep -E '^fe@'` would
# capture only the header and silently drop every one of those — the awk
# below keeps each header AND the space-indented lines immediately under
# it, stopping at the next line that isn't indented.
echo ""
fe_out="$(timeout 5 "$SCRIPT_DIR/sot-fe" version 2>/dev/null || true)"
# Session-listing brief: the header names how far back THIS daemon's
# memory of a disconnected box reaches, so a restart's forgetting is
# visible rather than read as "no sessions" -- pulled out of the daemon
# row's own build column ("..., up 3h)"), never re-derived, and omitted
# (falling back to the plain header) for a daemon predating the field.
daemon_uptime="$(printf '%s\n' "$fe_out" | grep -oE ', up [^)]+\)' | head -1 | sed -E 's/^, up //; s/\)$//')"
if [ -n "$daemon_uptime" ]; then
    echo "attached frontend boxes (this daemon, up $daemon_uptime):"
else
    echo "attached frontend boxes (this daemon):"
fi
fe_rows="$(printf '%s\n' "$fe_out" | awk '
    /^fe@/ { print; keep=1; next }
    keep && /^ / { print; next }
    { keep=0 }
')"
if [ -n "$fe_rows" ]; then
    printf '%s\n' "$fe_rows" | sed 's/^/  /'
else
    echo "  (none attached, or no daemon answered)"
fi
