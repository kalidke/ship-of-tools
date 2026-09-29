#!/usr/bin/env bash
# comm-status.sh — write one fact about this session's turn into the sot-comm
# registry. Backbone of the ADE "state-nav" at-a-glance view: the nav renders
# .agents[<handle>].{state, summary} per session, aged by status_at.
#
# Usage:
#   comm-status.sh <verb> ["text"]
#
# Three verbs are EVENTS — sent by hooks, never by the model:
#   prompt      a turn is starting. COMM_STATUS_ORIGIN=user|machine (default
#               machine) names who started it: sets `floor` to the origin; a
#               USER prompt also clears `question`, `done` and
#               `question_prompt` (typing into the session answers it and
#               reads it); a machine prompt clears nothing. COMM_STATUS_PROMPT_ID,
#               when set, is stamped into `floor_prompt` — the harness's own
#               per-turn id, used only for identity comparisons below.
#   stop        a turn just ended (sent at EVERY Stop, after any marker stamp
#               below has already run): sets `done` when `floor` was `user`
#               and neither `question` nor `waiting` is set, then clears
#               `floor` and `floor_prompt`.
#   interrupted a turn died with no Stop (a user interrupt, or a bare "No"
#               permission denial — Claude writes the same transcript marker
#               for both, and this script does not distinguish them).
#               COMM_STATUS_PROMPT_ID names the dying turn: `floor` clears
#               only if `floor_prompt` still matches it (a stale marker from
#               an earlier kill can never clear a newer turn's floor), and
#               `question` clears only if `question_prompt` matches it (so a
#               permission dialog that turn opened, still unanswered when the
#               turn died, stops blocking the row — never blind: a
#               self-reported `blocked` with no recorded turn is never
#               touched). Never sets `done` — a killed turn did not complete.
#
# Five verbs are DECLARATIONS — the model's own word (the Stop hook's marker
# stamp and the two yield hooks send these too, on the model's behalf):
#   working | idle     clear `question`, `waiting`, `done` and `question_prompt`
#   blocked ["q"]       sets `question` (keeps `waiting` — red outranks
#                       purple; the wait returns when the answer turn ends);
#                       also stamps `question_prompt` from COMM_STATUS_PROMPT_ID
#                       (empty when the model self-reports, so `interrupted`
#                       above can never mistake a self-report for a dialog it
#                       may auto-clear)
#   waiting ["s"]       sets `waiting` (keeps `question`)
#   done                sets `done`, clears `question`, `waiting` and
#                       `question_prompt`
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
    prompt|stop|interrupted|working|idle|blocked|done|waiting) ;;
    "") echo "usage: comm-status.sh <prompt|stop|interrupted|working|idle|blocked|done|waiting> [\"text\"]" >&2; exit 2 ;;
    *)  echo "comm-status.sh: invalid verb '$VERB' (want prompt|stop|interrupted|working|idle|blocked|done|waiting)" >&2; exit 2 ;;
esac

# Self-gate: only a joined comm agent (a session with a self row) reports.
# NAME comes from comm-context (the pane-keyed self file); empty / no row →
# no-op for an EVENT, a loud failure for a DECLARATION.
_no_row() {
    case "$VERB" in prompt|stop|interrupted) exit 0 ;; esac
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
# The harness's own per-turn id (empty outside prompt/blocked/interrupted
# callers) — an identity token, never free text, so --arg is fine.
PROMPT_ID="${COMM_STATUS_PROMPT_ID:-}"
# The live transcript path, persisted below on a `prompt` only — comm-wake.sh's
# silent-exit watcher reads it back to scan for a kill marker.
TRANSCRIPT="${COMM_STATUS_TRANSCRIPT:-}"

# The id is the WHOLE authority of `interrupted`: with none it must do
# nothing. An empty id compares EQUAL to a field that was never stamped --
# `floor_prompt` after a machine `prompt` from an adapter that passes no id,
# or `question_prompt` after a self-reported `blocked` -- so a call with no id
# would clear a live floor, or a question nobody proved was dead, which is the
# blind clear the id exists to prevent. comm-wake.sh's watcher already skips
# an empty id; this is the same refusal in the script that OWNS the fields, so
# a second caller can never reintroduce it. Silent rc 0, like every other
# EVENT no-op.
if [ "$VERB" = interrupted ] && [ -z "$PROMPT_ID" ]; then
    exit 0
fi

# The whole read-decide-write, run under the registry lock.
status_txn() {
    jq -e --arg n "$NAME" '.agents[$n]' "$REGISTRY" >/dev/null 2>&1 || return 0   # row gone: no-op
    # MSYS2 argv-conversion guard (comm-lib.sh's sot_jq_rawfile): SUM is
    # free-text and must never reach jq via --arg.
    local sum_file; sum_file="$(sot_jq_rawfile "$SUM")" || return 1
    local ts rc=0
    ts="$(now_iso)"
    jq --arg n "$NAME" --arg st "$VERB" --arg o "$ORIGIN" --arg t "$ts" --arg h "$HAVE" --arg pid "$PROMPT_ID" \
       --rawfile sum "$sum_file" '
      .agents[$n] |= (
        del(.turn_origin, .sticky, .sticky_at)
        | if $st == "prompt" then .floor = $o | .floor_prompt = $pid
            | (if $o == "user" then del(.question, .done, .question_prompt) else . end)
          elif $st == "stop" then
            (if .floor == "user" and .question == null and .waiting == null then .done = true else . end)
            | del(.floor, .floor_prompt)
          elif $st == "interrupted" then   # a turn died with no Stop — clear
                                            # only what THIS turn owns, proven
                                            # by the recorded id, never blind
            (if .floor != null and .floor_prompt == $pid then del(.floor, .floor_prompt) else . end)
            | (if .question != null and .question_prompt == $pid then del(.question, .question_prompt) else . end)
          else   # declarations — blocked/waiting set ONLY their own fact and
                  # leave .note alone: it is the last line the session itself
                  # declared, and aliasing it to the question text left that
                  # text as the summary fallback long after the question was
                  # answered and gone (a row read blocked/idle while `note`
                  # still held dead question text — field report, 2026-09-27).
            if $st == "blocked" then .question = (if $h == "1" then $sum else (.note // "") end) | .question_prompt = $pid
              elif $st == "waiting" then .waiting = (if $h == "1" then $sum else (.note // "") end)
              elif $st == "done" then (if $h == "1" then .note = $sum else . end) | .done = true | del(.question, .waiting, .question_prompt)
              else (if $h == "1" then .note = $sum else . end) | del(.question, .waiting, .done, .question_prompt) end   # working, idle
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
# `if ... ; then : ; else rc=$? ; fi`, not a bare call then `$?` on the next
# line: under `set -e` a nonzero exit from the bare call aborts the script
# right there, at that statement, skipping every line below it — including a
# capture on the line after it (comm-lib.sh's with_lock docstring has the
# full mechanism this idiom guards against).
if with_lock status_txn; then RC=0; else RC=$?; fi

# Persist the live transcript path for comm-wake.sh's silent-exit watcher —
# outside the lock (a plain per-session path, not registry state) and only on
# a `prompt`: this is the one event a genuine turn start always sends, and a
# fresh path here means a fresh turn to scan. Same-directory temp + `mv` for
# the same atomicity reason as everywhere else in this file: never a window
# where a concurrent reader sees a torn path. Best-effort — a watcher that
# reads no file just skips the check (comm-wake.sh's own no-op path).
if [ "$RC" -eq 0 ] && [ "$VERB" = prompt ] && [ -n "$TRANSCRIPT" ]; then
    STATE_DIR="$COMM_HOME/state"
    mkdir -p "$STATE_DIR" 2>/dev/null || true
    tp_tmp="$STATE_DIR/transcript-$NAME.path.tmp.$$"
    if printf '%s' "$TRANSCRIPT" > "$tp_tmp" 2>/dev/null; then
        mv -f "$tp_tmp" "$STATE_DIR/transcript-$NAME.path" 2>/dev/null || rm -f "$tp_tmp"
    fi
fi
exit "$RC"
