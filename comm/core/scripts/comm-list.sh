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
ensure_home

STALE_SECS="${SOT_COMM_STALE_SECS:-600}"
nows="$(date -u +%s)"

# fmt_age SECONDS — compact relative age ("just now"/"2m ago"/"1h ago"/"3d ago").
fmt_age() {
    local s="$1"
    if   [ "$s" -lt 60 ];    then echo "just now"
    elif [ "$s" -lt 3600 ];  then echo "$((s / 60))m ago"
    elif [ "$s" -lt 86400 ]; then echo "$((s / 3600))h ago"
    else                          echo "$((s / 86400))d ago"
    fi
}

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
    if [ "$age" -le "$STALE_SECS" ]; then status="live"; else status="stale ${age}s"; fi
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
done < <(jq -r '.agents | to_entries[]
        | [.key, .value.host, .value.repo, .value.last_seen, (.value.expertise | join("/")),
           (.value.state // ""), (.value.summary // ""), (.value.status_at // "")]
        | join("")' "$REGISTRY")

if [ "$any" = false ]; then echo "  (none)"; fi
