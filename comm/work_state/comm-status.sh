#!/usr/bin/env bash
# comm-status.sh — write one fact about this session's turn into the sot-comm
# registry. Backbone of the ADE "state-nav" at-a-glance view: the nav renders
# .agents[<handle>].{state, summary} per session, aged by status_at.
#
# Usage:
#   comm-status.sh <verb> ["text"]
#
# Five verbs are EVENTS — sent by hooks and the join, never by the model:
#   prompt   a turn is starting. COMM_STATUS_ORIGIN=user|machine (default
#            machine) names who started it: sets `floor` to the origin; a
#            USER prompt also clears `question` and `done` (typing into the
#            session answers it and reads it); a machine prompt clears
#            nothing. Row or no row, it writes the turn mark (below).
#   stop     a turn just ended (sent by the Stop that lets the turn end, after
#            any marker stamp below has already run; a Stop that blocks sends
#            none): sets `done` when `floor` was `user` and neither `question`
#            nor `waiting` is set, then clears `floor` and the Stop hook's
#            `stop_at` mark. Row or no row, it removes the turn mark.
#   dialog "T"  the agent shows a dialog that waits on a person (Codex's
#            PermissionRequest hook sends it; for a Claude row, the daemon,
#            below): sets `dialog` to T, which reads red over the running turn;
#            no text is refused (rc 2). With SOT_WORKSPACE_ID or
#            CLAUDE_CODE_SESSION_ID set, its write also creates
#            state/dialog-<id>.marker, the cheap test the heartbeat makes on
#            every tool return.
#   resumed  the dialog is gone (the heartbeat sends it when it finds that
#            marker; for a Claude row, the daemon): clears `dialog`. `prompt`
#            and `stop` clear it too: it never outlives its turn. All three
#            remove the marker once their write has committed.
#   join     the session has just written its row (comm-join.sh sends it):
#            with a turn mark of its own session under five minutes old, sets
#            `floor` to machine as a machine `prompt` does; else writes nothing.
#
# The daemon's entry: SOT_COMM_ROW_HANDLE=<handle> names the row whose screen
# the daemon read (ADR 0044, dialog amendment). Only `dialog` and `resumed` take
# it (any other verb is refused, rc 2); the agent gate still runs; no identity
# is derived (NAME is the handle) and no marker is written or removed, so no
# heartbeat clears a fact the daemon set.
#
# The turn mark, $COMM_HOME/state/turn-<the self slot's path>, holds the time
# of the session's last `prompt` (epoch seconds) and its session
# ($CLAUDE_CODE_SESSION_ID, empty under Codex) until its `stop`. The turn that
# joins starts before its row: its `prompt` found no row (a first start) or
# set a floor that the join's new row replaced (a resume, a provisional row).
# So the join starts that turn's floor from the mark, and the turn's Stop ends
# it. A session whose hooks are off or missing sends no `prompt`, so it has no
# mark and its join starts nothing: no floor without a Stop to end it. The
# self slot keys the mark: it is the one name a session has before its handle,
# and the place its join writes that handle (the daemon pins one per row). The
# no-pane slot, shared by every shell outside a row on the box, gets no mark.
# A turn that ends through its Stop takes its mark with it. One that ends with
# no Stop (an interrupt, a failed request, a killed process) leaves it: the
# session in it keeps a join from no session (ccx before codex starts, a probe
# row) or from another session from taking it, and five minutes bounds it for
# its own; the joins it serves (the launcher's /sot-session-start, a resume,
# the bootstrap nudge) come in a turn's first minute. The floor is machine
# whatever the turn's origin: a turn that joins ends gray, as it did with no
# floor, never blue.
#
# Five verbs are DECLARATIONS — the model's own word (the Stop hook's marker
# stamp and the two yield hooks send these too, on the model's behalf):
#   working | idle     clear `question`, `waiting` and `done`
#   blocked ["q"]       sets `question` (keeps `waiting` — red outranks
#                       purple; the wait returns when the answer turn ends);
#                       no text keeps the question's own text, and with none
#                       of its own the stamp is refused (rc 2, nothing written).
#                       With $SOT_COMM_ASKQ_ID set (the AskUserQuestion hook
#                       passes its tool_use_id) it also creates
#                       state/askq-<id>.marker, the proof that the matching
#                       PostToolUse is THIS dialog's answer; a manual
#                       `blocked` sets no variable and writes no marker
#   waiting ["s"]       sets `waiting` and clears `question` (a wait says
#                       nothing needs the user); no text keeps the wait's own
#                       text, and with none of its own the stamp is refused,
#                       like blocked
#   done                sets `done`, clears `question` and `waiting`
#   TEXT omitted keeps the prior declaration line (`note`); pass "" to clear
#   it (working, idle and done only: blocked and waiting never clear a text).
#
# The registry row is a set of FACTS, not one state (ADR 0044 amendment,
# 2026-09-19: "the row is a set of facts reduced by display priority").
# `state` and `summary` are a REDUCTION over those facts, recomputed on every
# write, by priority:
#   dialog set                   -> blocked   summary = dialog
#   question set, floor absent  -> blocked   summary = question
#   floor present                -> working   summary = note
#   waiting set                  -> waiting   summary = waiting
#   done set                     -> done      summary = note
#   otherwise                    -> idle      summary = note
# `note` holds the last working/idle/done line so the summary can return to it
# once red or purple lifts; a blocked or waiting stamp WITH text deletes it.
# `dialog` is an event fact: only its setters (a Codex hook, the daemon for a
# Claude row) write it, and it touches no `question`, `waiting`, `done`, `note`
# or `floor`.
#
# READ-DECIDE-WRITE IS ONE CRITICAL SECTION (Codex review, #223): apply the
# verb, delete legacy keys, reduce, and stamp are ONE jq program run inside
# `with_lock`, so a writer that commits between another reader's read and
# write cannot be overwritten by a decision made against a stale row. The
# registry mutation's exit status is this script's exit status.
#
# Self-gating: a session with no registry row (not a joined comm agent — e.g.
# a plain human session where a global hook also fires) is a silent no-op for
# an EVENT (rc 0), the turn mark aside; a DECLARATION with nowhere to land
# fails loudly — the model meant it, and a stamp run from the wrong cwd
# resolved no identity and vanished with rc 0 once (field report, 2026-09-18).
# Merges into the existing row; never clobbers host/workspace_id/repo/expertise/
# status/joined.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=comm-lib.sh
source "$SCRIPT_DIR/comm-lib.sh"

VERB="${1:-}"
case "$VERB" in
    prompt|stop|join|dialog|resumed|working|idle|blocked|done|waiting) ;;
    "") echo "usage: comm-status.sh <prompt|stop|join|dialog|resumed|working|idle|blocked|done|waiting> [\"text\"]" >&2; exit 2 ;;
    *)  echo "comm-status.sh: invalid verb '$VERB' (want prompt|stop|join|dialog|resumed|working|idle|blocked|done|waiting)" >&2; exit 2 ;;
esac

ROW_HANDLE="${SOT_COMM_ROW_HANDLE:-}"
if [ -n "$ROW_HANDLE" ]; then
    case "$VERB" in dialog|resumed) ;; *) echo "comm-status.sh: SOT_COMM_ROW_HANDLE stamps dialog or resumed only" >&2; exit 2 ;; esac
fi

# A second agent inside the session never stamps the row: an event is a silent
# no-op, a declaration a loud failure. The gate comes before the context call, so
# a refused child touches nothing.
if ! _why="$(sot_require_agent)"; then
    case "$VERB" in prompt|stop|join|dialog|resumed) exit 0 ;; esac
    echo "comm-status.sh: $_why" >&2; exit 1
fi
if [ "$VERB" = dialog ] && [ -z "${2-}" ]; then
    echo "comm-status.sh: dialog needs its text -- stamp discarded" >&2; exit 2
fi
if [ -n "$ROW_HANDLE" ]; then NAME="$ROW_HANDLE"; else eval "$("$SCRIPT_DIR/comm-context.sh")"; fi

# The turn mark (header), kept before the row check: the turn that joins has no
# row when it starts. `join` ends here or goes on as a machine `prompt`.
TURN_MARK=""
case "${SELF_FILE:-}" in
    ""|*__nopane.txt) ;;
    *) TURN_MARK="$COMM_HOME/state/turn-$(printf '%s' "$SELF_FILE" | tr -c 'A-Za-z0-9._-' '_')" ;;
esac
case "$VERB" in
    prompt) [ -z "$TURN_MARK" ] || { mkdir -p "$COMM_HOME/state" \
                && printf '%s %s\n' "$(date +%s)" "${CLAUDE_CODE_SESSION_ID:-}" > "$TURN_MARK"; } 2>/dev/null || true ;;
    stop)   [ -z "$TURN_MARK" ] || rm -f "$TURN_MARK" 2>/dev/null || true ;;
    join)
        { read -r MARK_AT MARK_SESSION < "$TURN_MARK"; } 2>/dev/null || exit 0
        case "$MARK_AT" in ''|*[!0-9]*) exit 0 ;; esac
        [ "$(( $(date +%s) - MARK_AT ))" -le 300 ] || { rm -f "$TURN_MARK" 2>/dev/null; exit 0; }
        [ "$MARK_SESSION" = "${CLAUDE_CODE_SESSION_ID:-}" ] || exit 0
        VERB=prompt COMM_STATUS_ORIGIN=machine ;;
esac

# Self-gate: only a joined comm agent (a session with a self row) reports.
# NAME comes from comm-context (the pane-keyed self file); empty / no row →
# no-op for an EVENT, a loud failure for a DECLARATION.
_no_row() {
    case "$VERB" in prompt|stop|dialog|resumed) exit 0 ;; esac
    echo "comm-status.sh: no registry row for '${NAME:-<no identity>}' from cwd $PWD -- stamp discarded; run it from the session's project root" >&2
    exit 1
}
# An unreadable registry is no evidence either way: every verb FAILs.
UNREADABLE="FAILED: the registry could not be read; stamp discarded"
[ -n "${NAME:-}" ] || _no_row
rc=0; sot_registry_read "$NAME" >/dev/null || rc=$?
case "$rc" in 0) ;; 1) _no_row ;; *) echo "$UNREADABLE" >&2; exit 1 ;; esac
# The AskUserQuestion answer marker, behind the gate and the row check above (the
# id is sanitized: it becomes a filename).
if [ "$VERB" = blocked ] && [ -n "${SOT_COMM_ASKQ_ID:-}" ]; then
    mkdir -p "$COMM_HOME/state" 2>/dev/null || true
    : > "$COMM_HOME/state/askq-$(printf '%s' "$SOT_COMM_ASKQ_ID" | tr -c 'A-Za-z0-9._-' '_').marker" 2>/dev/null || true
fi

# ${2+set}: distinguish an omitted text (keep the prior note) from an
# explicit "" (clear it).
HAVE=0; [ "${2+set}" = set ] && HAVE=1
SUM="${2-}"
ORIGIN="${COMM_STATUS_ORIGIN:-machine}"

# The whole read-decide-write, run under the registry lock.
status_txn() {
    local row_rc=0 row
    row="$(sot_registry_read "$NAME")" || row_rc=$?
    case "$row_rc" in 0) ;; 1) return 0 ;; *) echo "$UNREADABLE" >&2; return 1 ;; esac   # row gone: no-op
    # A question or wait always carries readable text: with none given and none
    # of its own on the row, the stamp is refused (inside the lock, with no
    # write).
    if [ -z "$SUM" ] && { [ "$VERB" = blocked ] || [ "$VERB" = waiting ]; }; then
        local fld=waiting out; [ "$VERB" = blocked ] && fld=question
        # One read, one character: does the verb's own text have any? Only the
        # first character is tested, so a CR a native jq adds after it is harmless.
        out="$(printf '%s' "$row" | jq -r --arg f "$fld" 'if ((.[$f] // "") | explode | all(. == 10)) then "0" else "1" end' 2>/dev/null)"
        if [ "${out:0:1}" != 1 ]; then
            echo "comm-status.sh: $VERB needs its text -- stamp discarded" >&2; return 2
        fi
    fi
    # SUM is free text: it reaches jq on stdin, after the registry (registry_replace --text; comm-lib-base.sh, free text and jq).
    local ts
    ts="$(now_iso)"
    registry_replace --text "$SUM" '
      input as $sum
      | .agents[$n] |= (
        del(.turn_origin, .sticky, .sticky_at)
        | if $st == "prompt" then del(.dialog) | .floor = $o
            | (if $o == "user" then del(.question, .done) else . end)
          elif $st == "stop" then
            (if .floor == "user" and .question == null and .waiting == null then .done = true else . end)
            | del(.floor, .stop_at, .dialog)
          elif $st == "dialog" then .dialog = $sum
          elif $st == "resumed" then del(.dialog)
          else   # declarations — blocked/waiting set their own fact and never
                  # copy their text into .note: aliasing it to the question
                  # left that text as the summary fallback long after the
                  # question was answered and gone (field report, 2026-09-27).
                  # Text they carry does supersede the older note, which goes,
                  # so a woken or answered row never shows a line written
                  # before its latest declaration (a row read working with a
                  # days-old note — field report, 2026-10-01).
            if $st == "blocked" then (.question = (if $sum != "" then $sum else .question end))
                | (if $sum != "" then del(.note) else . end)
              elif $st == "waiting" then (.waiting = (if $sum != "" then $sum else .waiting end))
                | del(.question)
                | (if $sum != "" then del(.note) else . end)
              elif $st == "done" then (if $h == "1" then .note = $sum else . end) | .done = true | del(.question, .waiting)
              else (if $h == "1" then .note = $sum else . end) | del(.question, .waiting, .done) end   # working, idle
          end
        | .state = (if .dialog != null then "blocked"
                    elif .question != null and .floor == null then "blocked"
                    elif .floor != null then "working"
                    elif .waiting != null then "waiting"
                    elif .done == true then "done" else "idle" end)
        | .summary = (if .dialog != null then .dialog
                      elif .state == "blocked" then .question
                      elif .state == "waiting" then .waiting else (.note // "") end)
        | .status_at = $t | .last_seen = $t)
    ' --arg n "$NAME" --arg st "$VERB" --arg o "$ORIGIN" --arg t "$ts" --arg h "$HAVE"
}
rc=0; with_lock status_txn || rc=$?

# The dialog marker follows the fact, after the write that commits it: `dialog`
# creates it, and `resumed`, `prompt` and `stop` remove it. The heartbeat only
# tests it, so a write that failed leaves the marker as the fact is. The key is
# sanitized (it becomes a filename); with no key there is no marker, and the
# `prompt` or `stop` that ends the turn clears the fact.
DIALOG_KEY=""; [ -n "$ROW_HANDLE" ] || DIALOG_KEY="${SOT_WORKSPACE_ID:-${CLAUDE_CODE_SESSION_ID:-}}"
if [ "$rc" = 0 ] && [ -n "$DIALOG_KEY" ]; then
    DIALOG_MARKER="$COMM_HOME/state/dialog-$(printf '%s' "$DIALOG_KEY" | tr -c 'A-Za-z0-9._-' '_').marker"
    case "$VERB" in
        dialog) { mkdir -p "$COMM_HOME/state" && : > "$DIALOG_MARKER"; } 2>/dev/null || true ;;
        resumed|prompt|stop) rm -f "$DIALOG_MARKER" 2>/dev/null || true ;;
    esac
fi
exit "$rc"
