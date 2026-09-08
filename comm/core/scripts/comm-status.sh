#!/usr/bin/env bash
# comm-status.sh — set this session's work-state + one-line summary in the
# sot-comm registry. Backbone of the ADE "state-nav" at-a-glance view: the
# nav renders .agents[<handle>].{state, summary} per session, aged by status_at.
#
# Usage:
#   comm-status.sh <state> ["summary"]
#     state      working | idle | blocked | done | waiting  — the WORK state
#                (distinct from the comm-lifecycle `status` field).
#                blocked = red (needs the USER to act); waiting = purple (a
#                job/subagent/peer you launched is still running — not free);
#                done = blue (finished, unread); working = green; idle = gray.
#     summary    one sentence of current (working) / just-finished (done) work.
#                OMITTED keeps the prior summary (so a floor write reads
#                "idle · last: <prior>"); pass "" to clear it.
#
# Two kinds of writer:
#   - the MODEL reports EXPLICITLY (COMM_STATUS_SOFT unset). Its word overrides
#     everything. `waiting` sets a sticky marker (.sticky/.sticky_at); any other
#     explicit state clears it, except `blocked`, which keeps it underneath —
#     waiting-on-a-job and blocked-on-the-user can both be true, and answering
#     the question must drop the row back to purple, not green.
#   - HOOKS write SOFT (COMM_STATUS_SOFT=1) and never override a deliberate
#     state:
#       soft `working` (the prompt hook, with COMM_STATUS_ORIGIN=user|machine —
#         default machine): HOLDS a `waiting` row with a live marker; HOLDS a
#         `blocked` or `done` row on a MACHINE turn (a relay message, Monitor
#         event or notification is not the user answering); and ALWAYS records
#         `turn_origin`, even when holding, so the floor below reads the running
#         turn's origin and never a stale one.
#       soft floor (`done` from the Stop hook; `idle` from older callers):
#         HOLDS `blocked`, `done`, and a `waiting` row with a live marker;
#         DEMOTES a `working` row that still carries a live marker back to
#         `waiting` (so purple survives turn cycles); paints BLUE only when the
#         row was `working` with `turn_origin == user` — absent or `machine`
#         FAILS GRAY; a marker older than STICKY_MAX_AGE_S self-heals to gray.
#   Display precedence: blocked > working > waiting > done > idle.
#   Decisions: ADR 0044 (blue/gray = unread/read, no aging), ADR 0023.
#
# READ-DECIDE-WRITE IS ONE CRITICAL SECTION (Codex review, #223): every guard
# reads the row INSIDE `with_lock`, so a writer that commits between our read
# and our write cannot be overwritten by a decision made against a stale row.
# The registry mutation's exit status is this script's exit status.
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
# ${2+set}: distinguish an omitted summary (keep prior) from an explicit "" (clear).
HAVE=0; [ "${2+set}" = set ] && HAVE=1
SUMMARY="${2-}"
# turn_origin is written by the SOFT working write only: the prompt hook is the
# one writer that can tell a human prompt from a wake, and says so explicitly.
TURN_ORIGIN=""
if [ "$STATE" = working ] && [ "$SOFT" = 1 ]; then TURN_ORIGIN="${COMM_STATUS_ORIGIN:-machine}"; fi

soft_floor() { [ "$SOFT" = 1 ] && { [ "$STATE" = idle ] || [ "$STATE" = done ]; }; }
row_field() { jq -r --arg n "$NAME" --arg f "$1" '.agents[$n][$f] // ""' "$REGISTRY" 2>/dev/null; }
# sticky_age_s — seconds since the row's sticky_at stamp; empty when no marker
# (or unparseable → 999999, i.e. treated as expired rather than immortal).
sticky_age_s() {
    local at now
    at="$(row_field sticky_at)"
    [ -n "$at" ] || { echo ""; return; }
    now=$(date -u +%s)
    at=$(date -u -d "$at" +%s 2>/dev/null) || { echo 999999; return; }
    echo $(( now - at ))
}
marker_live() { [ -n "$1" ] && [ "$1" -lt "$STICKY_MAX_AGE_S" ]; }

# write_origin ORIGIN — record the running turn's provenance without touching
# the state (a soft working write that HOLDS the current colour).
write_origin() {
    jq --arg n "$NAME" --arg o "$1" 'if .agents[$n] then .agents[$n] += {turn_origin:$o} else . end' \
       "$REGISTRY" > "$REGISTRY.tmp" && mv "$REGISTRY.tmp" "$REGISTRY"
}
# write_state STATE HAVE_SUMMARY SUMMARY STICKY_OP — merge the work-state into
# the row. Object `+=` preserves every other field. status_at + last_seen both
# get the stamp so the nav can age a stale "working" that never got a closing
# Stop. Returns the mutation's status (the trailing cleanup must not mask it).
write_state() {
    local st="$1" have="$2" sum="$3" sticky_op="$4" ts base sticky sum_file rc=0
    ts="$(now_iso)"
    if [ "$have" = 1 ]; then
        base='{state:$st, summary:$sum, status_at:$t, last_seen:$t}'
    else
        base='{state:$st, status_at:$t, last_seen:$t}'
    fi
    if [ -n "$TURN_ORIGIN" ]; then base="($base + {turn_origin:\$o})"; fi
    case "$sticky_op" in
        set)   sticky=' + {sticky: (if $sum != "" then $sum else (.agents[$n].summary // "") end), sticky_at: $t}' ;;
        clear) sticky=' | if .agents[$n] then .agents[$n] |= del(.sticky, .sticky_at) else . end' ;;
        *)     sticky='' ;;
    esac
    # MSYS2 argv-conversion guard (comm-lib.sh's sot_jq_rawfile): sum is a
    # free-text work-state summary and must never reach jq via --arg.
    sum_file="$(sot_jq_rawfile "$sum")" || return 1
    if [ "$sticky_op" = clear ]; then
        jq --arg n "$NAME" --arg st "$st" --rawfile sum "$sum_file" --arg t "$ts" --arg o "$TURN_ORIGIN" \
           "(if .agents[\$n] then .agents[\$n] += $base else . end) $sticky" \
           "$REGISTRY" > "$REGISTRY.tmp" && mv "$REGISTRY.tmp" "$REGISTRY" || rc=$?
    else
        jq --arg n "$NAME" --arg st "$st" --rawfile sum "$sum_file" --arg t "$ts" --arg o "$TURN_ORIGIN" \
           "if .agents[\$n] then .agents[\$n] += ($base$sticky) else . end" \
           "$REGISTRY" > "$REGISTRY.tmp" && mv "$REGISTRY.tmp" "$REGISTRY" || rc=$?
    fi
    rm -f "$sum_file"
    return $rc
}

# The whole read-decide-write, run under the registry lock.
status_txn() {
    local st="$STATE" have="$HAVE" sum="$SUMMARY" cur age sticky_op=keep
    jq -e --arg n "$NAME" '.agents[$n]' "$REGISTRY" >/dev/null 2>&1 || return 0   # row gone: no-op
    cur="$(row_field state)"
    if [ "$st" = working ] && [ "$SOFT" = 1 ]; then
        local hold=0
        case "$cur" in
            waiting)      age="$(sticky_age_s)"; { [ -z "$age" ] || marker_live "$age"; } && hold=1 ;;
            blocked|done) [ "$TURN_ORIGIN" = machine ] && hold=1 ;;
        esac
        if [ "$hold" = 1 ]; then write_origin "$TURN_ORIGIN"; return; fi
    fi
    if soft_floor; then
        case "$cur" in blocked|done) return 0 ;; esac
        age="$(sticky_age_s)"
        if [ "$cur" = waiting ]; then
            # No marker at all = a pre-sticky manual waiting: also held.
            { [ -z "$age" ] || marker_live "$age"; } && return 0
        elif marker_live "$age"; then
            st=waiting; have=1; sum="$(row_field sticky)"
        fi
        if [ "$st" = done ]; then
            { [ "$cur" = working ] && [ "$(row_field turn_origin)" = user ]; } || st=idle
        fi
        sticky_op=clear   # only reached with the marker absent/expired (or demoting, where it stays: see below)
        [ "$st" = waiting ] && sticky_op=keep
    elif [ "$SOFT" = 0 ]; then
        case "$st" in
            waiting) sticky_op=set ;;
            blocked) sticky_op=keep ;;
            *)       sticky_op=clear ;;
        esac
    fi
    write_state "$st" "$have" "$sum" "$sticky_op"
}
with_lock status_txn
