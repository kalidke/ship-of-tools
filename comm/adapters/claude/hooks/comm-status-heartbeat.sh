#!/usr/bin/env bash
# comm-status-heartbeat.sh — Claude Code `PostToolUse` hook: keep a WORKING
# session's state-nav stamp fresh during LONG turns.
#
# Why: the nav wilts (whitens) a `working` row whose status_at is older than
# 10 min (AGENT_STALE_MINUTES) — the "claims working but silent" signal. But
# status_at was only written at turn START, so a legitimately-busy session on
# a long turn (heavy Julia runs) wilted white while working (the maintainer, 2026-07-03:
# "why does a peer session keep reverting to white while it's working"). This hook
# re-stamps on tool activity, THROTTLED to once per 60s, so:
#   - a busy session's row stays solid working-green however long the turn;
#   - wilt now fires only on 10+ min of ZERO tool activity — a real stall.
#
# Cheap by construction: the no-op path (not a comm agent / not working /
# stamp fresh) is a couple of jq reads; the registry write happens at most
# once a minute. Always exits 0 — a hook must never wedge a turn.
#
# Source of truth: comm/adapters/claude/hooks/comm-status-heartbeat.sh in
# Ship of Tools, deployed to ~/.sot-comm/bin by ShipTools.update_comm().
set -uo pipefail
COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
REGISTRY="$COMM_HOME/registry.json"
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

[ -f "$REGISTRY" ] || exit 0
NAME=""
[ -x "$SELF_DIR/comm-context.sh" ] && eval "$("$SELF_DIR/comm-context.sh" 2>/dev/null)" 2>/dev/null || true
[ -n "${NAME:-}" ] || exit 0

row="$(jq -r --arg n "$NAME" '.agents[$n] | if . then (.state // "") + "|" + (.status_at // "") else "" end' "$REGISTRY" 2>/dev/null || true)"
[ -n "$row" ] || exit 0
state="${row%%|*}"; at="${row#*|}"
# HIERARCHY (red > green > purple, maintainer 2026-07-04, refined 2026-07-17):
# tool activity means the session is ACTIVELY WORKING, so a `waiting` row with NO
# live sticky marker promotes to working-green for the duration (covers turns
# that start WITHOUT a UserPromptSubmit — monitor/notification wakes — which
# previously sat purple through real work). BUT a `waiting` row WITH a live
# sticky marker STAYS purple: the session explicitly declared it's waiting on a
# spawned job, so tool activity is polling those agents, not its own work
# (maintainer 2026-07-17: "green while only waiting for subagents"). See the
# `hold_purple` logic below. `blocked` is NEVER touched: red persists through any
# background activity until the user answers or the model explicitly clears.
case "$state" in
    working) ;;      # refresh path below (throttled)
    waiting) ;;      # stay-purple (live marker) or promote (expired/none) — below
    *) exit 0 ;;
esac

# A live sticky-`waiting` marker holds the row PURPLE regardless of current state:
#   - on a `waiting` row it PREVENTS the promote-to-green (the session declared it
#     is waiting on a spawned job/agents, so tool activity is polling them, not
#     its own work);
#   - on a `working` row it DEMOTES back to purple — the row was promoted to green
#     by a hook while the wait was still on (an EXPLICIT working/idle/done clears
#     the marker, so a live marker means the wait genuinely continues).
# Refines the 2026-07-04 promote-on-activity rule per the maintainer (2026-07-17:
# "green while only waiting for subagents"). No live marker → tool activity is
# real work → green. The marker's 2h TTL self-heals a forgotten waiting.
STICKY_MAX_AGE_S=7200
hold_purple=0
sat="$(jq -r --arg n "$NAME" '.agents[$n].sticky_at // ""' "$REGISTRY" 2>/dev/null)"
if [ -n "$sat" ]; then
    sat_s=$(date -u -d "$sat" +%s 2>/dev/null || echo 0)
    now_hb=$(date -u +%s)
    [ "$sat_s" -gt 0 ] && [ $((now_hb - sat_s)) -lt "$STICKY_MAX_AGE_S" ] && hold_purple=1
fi
if [ "$hold_purple" = 1 ]; then newstate=waiting; else newstate=working; fi

# Throttle a plain REFRESH (newstate == current state) to once per 60s so a busy
# row's anti-wilt stamp doesn't churn the registry. A STATE CHANGE (promote
# waiting->working, or demote working->waiting) bypasses the throttle so the
# color flips at the first tool call of the turn.
if [ "$newstate" = "$state" ]; then
    now_s=$(date -u +%s)
    at_s=$(date -u -d "$at" +%s 2>/dev/null || echo 0)
    [ $((now_s - at_s)) -ge 60 ] || exit 0
fi

ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
# Best-effort merge under the registry's mkdir-spinlock convention
# (comm-lib.sh's with_lock uses LOCKDIR="$COMM_HOME/.registry.lock" — a
# DIRECTORY). No spinning here: if the lock is held, just skip — the next
# tool call retries within a minute anyway.
LOCKDIR="$COMM_HOME/.registry.lock"
if mkdir "$LOCKDIR" 2>/dev/null; then
    trap 'rmdir "$LOCKDIR" 2>/dev/null' EXIT
    jq --arg n "$NAME" --arg t "$ts" --arg st "$newstate" \
       'if .agents[$n] and (.agents[$n].state == "working" or .agents[$n].state == "waiting")
        then .agents[$n] += {state:$st, status_at:$t, last_seen:$t} else . end' \
       "$REGISTRY" > "$REGISTRY.hb.tmp" 2>/dev/null && mv "$REGISTRY.hb.tmp" "$REGISTRY"
    rmdir "$LOCKDIR" 2>/dev/null
    trap - EXIT
fi
exit 0
