#!/usr/bin/env bash
# comm-status.sh — set this session's work-state + one-line summary in the
# sot-comm registry. Backbone of the ADE "state-nav" at-a-glance view: the
# nav renders .agents[<handle>].{state, summary} per session, aged by status_at.
#
# Usage:
#   comm-status.sh <state> ["summary"]
#     state      working | idle | blocked | done | waiting   — the WORK state
#                (distinct from the comm-lifecycle `status` field). blocked = red
#                (needs the USER to act); waiting = purple (a long job / subagent
#                is still running — the session is idle-of-its-own-work but NOT
#                free, so don't read it as available); idle = free / done.
#     summary    one sentence of current (working) / just-finished (done) work.
#                OMITTED keeps the prior summary (so `comm-status.sh idle` from a
#                Stop hook reads "idle · last: <prior>"); pass "" to clear it.
#
# Two writers by design (state-nav design note):
#   - the model runs `comm-status.sh working "<one-liner>"` when it judges the
#     upcoming work will run >~30s (pre-announce — model decides "is it long");
#   - a global Stop hook runs `comm-status.sh idle` at every turn-end (the
#     deterministic floor) so the glance never lies if the model stays quiet.
#
# STICKY WAITING (2026-07-02, "why are you not showing purple"): a deliberate
# `waiting` must survive TURN CYCLES, not just the turn it was set in. Before
# this fix the sequence  waiting → user prompt (hook: working) → turn end
# (hook: soft idle)  landed on GREEN while the background job still ran — the
# soft-idle guard protected `waiting` only until the next prompt clobbered it
# via `working`. Mechanics now:
#   - `comm-status.sh waiting "<summary>"` also stamps a sticky marker
#     (.sticky = summary, .sticky_at = now) in the registry row;
#   - the SOFT working write (COMM_STATUS_SOFT=1, from the UserPromptSubmit
#     hook) sets state=working but PRESERVES the marker — actively processing
#     a turn is true, but the wait isn't over;
#   - the SOFT idle write (Stop hook) DEMOTES to state=waiting (purple, sticky
#     summary restored) while the marker is live, instead of dropping to idle;
#   - any EXPLICIT (non-soft) state report — idle/done/working/blocked — CLEARS
#     the marker: the model consciously said the wait is over or superseded;
#   - a marker older than STICKY_MAX_AGE_S (2h) self-heals: soft idle clears it
#     and goes green, so a forgotten purple can't lie forever.
#
# CANONICAL HIERARCHY (maintainer decision, 2026-07-04): blocked/red > working/green >
# waiting/purple > idle. Red = a question pending ON THE USER: it survives
# machine-initiated turns (the working hook's machine-turn guard) and turn
# ends (soft idle's blocked guard), clearing only on a genuine user prompt or
# an explicit report. Green = actively working: any tool activity promotes a
# waiting row (heartbeat hook) for the turn's duration. Purple = idle with a
# live wait (sticky demote at Stop). The hooks enforce this; the model's
# explicit reports override everything.
#
# Self-gating: a session with no registry row (not a joined comm agent — e.g. a
# plain human session where a global hook also fires) is a silent no-op (rc 0).
# Merges into the existing row; never clobbers host/tmux/pane/repo/expertise/
# status/joined.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=comm-lib.sh
source "$SCRIPT_DIR/comm-lib.sh"
eval "$("$SCRIPT_DIR/comm-context.sh")"
ensure_home

STATE="${1:-}"
case "$STATE" in
    working|idle|blocked|done|waiting) ;;
    "") echo "usage: comm-status.sh <working|idle|blocked|done|waiting> [\"summary\"]" >&2; exit 2 ;;
    *)  echo "comm-status.sh: invalid state '$STATE' (want working|idle|blocked|done|waiting)" >&2; exit 2 ;;
esac

# Self-gate: only a joined comm agent (a session with a self row) reports. NAME
# comes from comm-context (the pane-keyed self file); empty / no row → no-op.
[ -n "${NAME:-}" ] || exit 0
jq -e --arg n "$NAME" '.agents[$n]' "$REGISTRY" >/dev/null 2>&1 || exit 0

SOFT="${COMM_STATUS_SOFT:-0}"
STICKY_MAX_AGE_S=7200   # a forgotten sticky-waiting self-heals after 2h

# sticky_age_s — seconds since the row's sticky_at stamp; empty when no marker
# (or unparseable → 999999, i.e. treated as expired rather than immortal).
sticky_age_s() {
    local at now
    at="$(jq -r --arg n "$NAME" '.agents[$n].sticky_at // ""' "$REGISTRY" 2>/dev/null)"
    [ -n "$at" ] || { echo ""; return; }
    now=$(date -u +%s)
    at=$(date -u -d "$at" +%s 2>/dev/null) || { echo 999999; return; }
    echo $(( now - at ))
}

# Soft working (the UserPromptSubmit hook): a re-invocation — a background task
# notification, a monitor wake, an inbox event — starts a turn but the session is
# still waiting on the agents/job it spawned. The soft `working` write must NOT
# clobber a live sticky-`waiting` marker to green: stay `waiting` (purple) while
# the marker is live. This is the counterpart to the soft-idle demote below and
# is what the header's "SOFT working does NOT clobber a live waiting marker"
# contract requires — without it, every re-invocation flipped the row green and
# the demote only restored purple on a CLEAN (non-nudged) turn-end, so the row
# read idle/green while spawned agents ran. An EXPIRED marker falls through to
# plain working (self-heal); an EXPLICIT `working` (SOFT=0, the model resuming
# its own work) is unaffected and clears the marker below.
if [ "$STATE" = working ] && [ "$SOFT" = 1 ]; then
    cur="$(jq -r --arg n "$NAME" '.agents[$n].state // ""' "$REGISTRY" 2>/dev/null)"
    if [ "$cur" = waiting ]; then
        age="$(sticky_age_s)"
        if [ -z "$age" ] || [ "$age" -lt "$STICKY_MAX_AGE_S" ]; then exit 0; fi
    fi
fi

# Soft idle (the Stop hook): the turn-end idle floor must NOT overwrite a
# deliberate `blocked` (pending question — would wipe red the instant it's set)
# or a live `waiting`. And while a sticky-waiting marker is live, soft idle
# DEMOTES to waiting (purple, sticky summary restored) instead of going green —
# this is what makes `waiting` survive turn cycles (see header). An expired
# marker is cleared and falls through to plain idle.
if [ "$STATE" = idle ] && [ "$SOFT" = 1 ]; then
    cur="$(jq -r --arg n "$NAME" '.agents[$n].state // ""' "$REGISTRY" 2>/dev/null)"
    [ "$cur" = blocked ] && exit 0
    age="$(sticky_age_s)"
    if [ "$cur" = waiting ]; then
        # Already purple: stay purple while the marker is live — or when there
        # is no marker at all (a pre-sticky manual waiting; ages out visually).
        # An EXPIRED marker falls through to idle + clear (the self-heal).
        if [ -z "$age" ] || [ "$age" -lt "$STICKY_MAX_AGE_S" ]; then exit 0; fi
    elif [ -n "$age" ] && [ "$age" -lt "$STICKY_MAX_AGE_S" ]; then
        # Live marker but state was clobbered to working by a turn cycle:
        # DEMOTE back to waiting (purple), restoring the sticky summary.
        STATE=waiting
        set -- waiting "$(jq -r --arg n "$NAME" '.agents[$n].sticky // ""' "$REGISTRY" 2>/dev/null)"
    fi
    # expired/absent marker: fall through as idle; STICKY_OP=clear removes it.
fi

# Sticky marker lifecycle: explicit `waiting` sets it; explicit idle/done/
# working clears it (the model consciously reported — the wait is over or
# superseded). Explicit `blocked` PRESERVES it: waiting-on-job and blocked-on-
# user can both be true (blocked wins display precedence), and when the user
# answers, the row must demote back to purple, not green. Soft writes (hooks)
# preserve it, except the expired-idle case.
STICKY_OP=keep
if [ "$SOFT" = 0 ]; then
    case "$STATE" in
        waiting) STICKY_OP=set ;;
        blocked) STICKY_OP=keep ;;
        *)       STICKY_OP=clear ;;
    esac
fi
if [ "$STATE" = idle ] && [ "$SOFT" = 1 ]; then STICKY_OP=clear; fi   # only reached when marker absent/expired

# registry_status NAME STATE HAVE_SUMMARY SUMMARY STICKY_OP — merge the
# work-state into the row (only if present), mirroring registry_touch's
# read-merge-write. Object `+=` preserves every other field. status_at +
# last_seen both get the stamp so the nav can age a stale "working" that never
# got a closing Stop.
registry_status() {
    local n="$1" st="$2" have="$3" sum="$4" sticky_op="$5" ts; ts="$(now_iso)"
    local base sticky
    if [ "$have" = 1 ]; then
        base='{state:$st, summary:$sum, status_at:$t, last_seen:$t}'
    else
        base='{state:$st, status_at:$t, last_seen:$t}'
    fi
    case "$sticky_op" in
        set)   sticky=' + {sticky: (if $sum != "" then $sum else (.agents[$n].summary // "") end), sticky_at: $t}' ;;
        clear) sticky=' | if .agents[$n] then .agents[$n] |= del(.sticky, .sticky_at) else . end' ;;
        *)     sticky='' ;;
    esac
    if [ "$sticky_op" = clear ]; then
        jq --arg n "$n" --arg st "$st" --arg sum "$sum" --arg t "$ts" \
           "(if .agents[\$n] then .agents[\$n] += $base else . end) $sticky" \
           "$REGISTRY" > "$REGISTRY.tmp" && mv "$REGISTRY.tmp" "$REGISTRY"
    else
        jq --arg n "$n" --arg st "$st" --arg sum "$sum" --arg t "$ts" \
           "if .agents[\$n] then .agents[\$n] += ($base$sticky) else . end" \
           "$REGISTRY" > "$REGISTRY.tmp" && mv "$REGISTRY.tmp" "$REGISTRY"
    fi
}

# ${2+set}: distinguish an omitted summary (keep prior) from an explicit "" (clear).
HAVE=0; [ "${2+set}" = set ] && HAVE=1
with_lock registry_status "$NAME" "$STATE" "$HAVE" "${2-}" "$STICKY_OP"
