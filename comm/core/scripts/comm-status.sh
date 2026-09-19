#!/usr/bin/env bash
# comm-status.sh — write one fact about this session's turn into the sot-comm
# registry. Backbone of the ADE "state-nav" at-a-glance view: the nav renders
# .agents[<handle>].{state, summary} per session, aged by status_at.
#
# Usage:
#   comm-status.sh <verb> ["text"]
#
# Two verbs are EVENTS — sent by hooks, never by the model:
#   prompt   a turn is starting. COMM_STATUS_ORIGIN=user|machine (default
#            machine) names who started it: sets `floor` to the origin; a
#            USER prompt also clears `question` and `done` (typing into the
#            session answers it and reads it); a machine prompt clears
#            nothing.
#   stop     a turn just ended (sent at EVERY Stop, after any marker stamp
#            below has already run): sets `done` when `floor` was `user` and
#            neither `question` nor `waiting` is set, then clears `floor`.
#
# Five verbs are DECLARATIONS — the model's own word (the Stop hook's marker
# stamp and the two yield hooks send these too, on the model's behalf):
#   working | idle     clear `question`, `waiting` and `done`
#   blocked ["q"]       sets `question` (keeps `waiting` — red outranks
#                       purple; the wait returns when the answer turn ends)
#   waiting ["s"]       sets `waiting` (keeps `question`)
#   done                sets `done`, clears `question` and `waiting`
#   TEXT omitted keeps the prior declaration line (`note`); pass "" to clear
#   it.
#
# The registry row is a set of FACTS, not one state (ADR 0044 amendment,
# 2026-09-19: "the row is a set of facts reduced by display priority").
# `state` and `summary` are a REDUCTION over those facts, recomputed on every
# write, by priority:
#   question set, floor absent  -> blocked   summary = question
#   floor present                -> working   summary = note
#   waiting set                  -> waiting   summary = waiting
#   done set                     -> done      summary = note
#   otherwise                    -> idle      summary = note
# `note` holds the declaration's own line so the summary can return to it
# once red or purple lifts — the daemon never touches it.
#
# READ-DECIDE-WRITE IS ONE CRITICAL SECTION (Codex review, #223): apply the
# verb, delete legacy keys, reduce, and stamp are ONE jq program run inside
# `with_lock`, so a writer that commits between another reader's read and
# write cannot be overwritten by a decision made against a stale row. The
# registry mutation's exit status is this script's exit status.
#
# Self-gating: a session with no registry row (not a joined comm agent — e.g.
# a plain human session where a global hook also fires) is a silent no-op for
# an EVENT (rc 0); a DECLARATION with nowhere to land fails loudly — the
# model meant it, and a stamp run from the wrong cwd resolved no identity and
# vanished with rc 0 once (field report, 2026-09-18).
# Merges into the existing row; never clobbers host/workspace_id/repo/expertise/
# status/joined.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=comm-lib.sh
source "$SCRIPT_DIR/comm-lib.sh"
eval "$("$SCRIPT_DIR/comm-context.sh")"
ensure_home

VERB="${1:-}"
case "$VERB" in
    prompt|stop|working|idle|blocked|done|waiting) ;;
    "") echo "usage: comm-status.sh <prompt|stop|working|idle|blocked|done|waiting> [\"text\"]" >&2; exit 2 ;;
    *)  echo "comm-status.sh: invalid verb '$VERB' (want prompt|stop|working|idle|blocked|done|waiting)" >&2; exit 2 ;;
esac

# Self-gate: only a joined comm agent (a session with a self row) reports.
# NAME comes from comm-context (the pane-keyed self file); empty / no row →
# no-op for an EVENT, a loud failure for a DECLARATION.
_no_row() {
    case "$VERB" in prompt|stop) exit 0 ;; esac
    echo "comm-status.sh: no registry row for '${NAME:-<no identity>}' from cwd $PWD -- stamp discarded; run it from the session's project root" >&2
    exit 1
}
[ -n "${NAME:-}" ] || _no_row
jq -e --arg n "$NAME" '.agents[$n]' "$REGISTRY" >/dev/null 2>&1 || _no_row

# ${2+set}: distinguish an omitted text (keep the prior note) from an
# explicit "" (clear it).
HAVE=0; [ "${2+set}" = set ] && HAVE=1
SUM="${2-}"
ORIGIN="${COMM_STATUS_ORIGIN:-machine}"

# The whole read-decide-write, run under the registry lock.
status_txn() {
    jq -e --arg n "$NAME" '.agents[$n]' "$REGISTRY" >/dev/null 2>&1 || return 0   # row gone: no-op
    # MSYS2 argv-conversion guard (comm-lib.sh's sot_jq_rawfile): SUM is
    # free-text and must never reach jq via --arg.
    local sum_file; sum_file="$(sot_jq_rawfile "$SUM")" || return 1
    local ts rc=0
    ts="$(now_iso)"
    jq --arg n "$NAME" --arg st "$VERB" --arg o "$ORIGIN" --arg t "$ts" --arg h "$HAVE" \
       --rawfile sum "$sum_file" '
      .agents[$n] |= (
        del(.turn_origin, .sticky, .sticky_at)
        | if $st == "prompt" then .floor = $o
            | (if $o == "user" then del(.question, .done) else . end)
          elif $st == "stop" then
            (if .floor == "user" and .question == null and .waiting == null then .done = true else . end)
            | del(.floor)
          else   # declarations
            (if $h == "1" then .note = $sum else . end)
            | if $st == "blocked" then .question = (if $h == "1" then $sum else (.note // "") end)
              elif $st == "waiting" then .waiting = (if $h == "1" then $sum else (.note // "") end)
              elif $st == "done" then .done = true | del(.question, .waiting)
              else del(.question, .waiting, .done) end   # working, idle
          end
        | .state = (if .question != null and .floor == null then "blocked"
                    elif .floor != null then "working"
                    elif .waiting != null then "waiting"
                    elif .done == true then "done" else "idle" end)
        | .summary = (if .state == "blocked" then .question
                      elif .state == "waiting" then .waiting else (.note // "") end)
        | .status_at = $t | .last_seen = $t)
    ' "$REGISTRY" > "$REGISTRY.tmp" && mv "$REGISTRY.tmp" "$REGISTRY" || rc=$?
    rm -f "$sum_file"
    return $rc
}
with_lock status_txn
