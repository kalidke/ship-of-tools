#!/usr/bin/env bash
# comm-turn-auditor.sh — tiered turn-end auditor (v1, 2026-07-02, the maintainer:
# "a haiku agent that parses last output of each session on a hook and checks
# status and makes a message back if there is something missing").
#
# Called by the Stop hook (comm-status-idle.sh) for JOINED comm agents only.
#
#   Tier 1 (deterministic, free): scan the transcript tail for CANDIDATE misses:
#     question    the turn ends on a '?' (should the row be blocked-red?)
#     artifact    an image/doc artifact was produced but never surfaced to the
#                 FE (no show-result / sot-fe preview call) — the user's most
#                 repeated feedback to sessions
#     background  a background task / subagent / watcher was armed but the
#                 registry row isn't waiting/blocked (purple miss)
#   Tier 2 (Haiku, ONLY when tier 1 trips): one headless `claude -p` call
#   judges the candidates against the actual turn content. CONSERVATIVE BY
#   DESIGN — the prompt says drop-when-unsure: a false nudge wakes a big-model
#   turn that costs ~100x this call. Most turns never reach tier 2.
#
# Contract with the caller (comm-status-idle.sh):
#   exit 0, stdout non-empty  → confirmed findings (one line, pre-formatted);
#                               caller wraps them in the Stop decision:block.
#   exit 0, stdout empty      → audited CLEAN — caller skips the legacy grep
#                               nudge (Haiku already judged rhetorical-vs-real).
#   exit 3                    → auditor unavailable/disabled/rate-limited —
#                               caller falls back to the legacy '?' grep nudge.
#   Any internal failure      → exit 3 (fail open to the legacy path). The
#                               auditor must NEVER wedge or delay a turn beyond
#                               its own timeout.
#
# Kill switch: SOT_TURN_AUDITOR=0 (env) or ~/.sot-comm/auditor.off (file).
# Model override: SOT_AUDITOR_MODEL (default claude-haiku-4-5-20251001).
# Rate limit: identical finding-candidates within 30 min are not re-judged.
#
# Source of truth: comm/core/scripts/comm-turn-auditor.sh in Ship of Tools,
# deployed to ~/.sot-comm/bin by ShipTools.update_comm(). Edit it there.
set -uo pipefail

NAME="${1:-}"; TP="${2:-}"
COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
REGISTRY="$COMM_HOME/registry.json"
STATE_DIR="$COMM_HOME/state"; mkdir -p "$STATE_DIR" 2>/dev/null || true
MODEL="${SOT_AUDITOR_MODEL:-claude-haiku-4-5-20251001}"

[ -n "$NAME" ] && [ -n "$TP" ] && [ -r "$TP" ] || exit 3
[ "${SOT_TURN_AUDITOR:-1}" = 0 ] && exit 3
[ -e "$COMM_HOME/auditor.off" ] && exit 3
CLAUDE_BIN="$(command -v claude || echo "$HOME/.local/bin/claude")"
[ -x "$CLAUDE_BIN" ] || exit 3

# ---- extract the turn tail ---------------------------------------------------
tail_lines="$(tail -n 500 "$TP" 2>/dev/null)" || exit 3

last_text="$(printf '%s\n' "$tail_lines" \
    | jq -r 'select(.type=="assistant") | .message.content[]? | select(.type=="text") | .text' 2>/dev/null \
    | tail -c 2500)"
[ -n "$last_text" ] || exit 3

# Recent tool calls: name + a short arg excerpt (file_path/command/skill),
# + explicit background/monitor markers. This is what grounds the judge.
tools="$(printf '%s\n' "$tail_lines" \
    | jq -r 'select(.type=="assistant") | .message.content[]? | select(.type=="tool_use")
             | .name + "  " + (([.input.file_path, .input.command, .input.skill,
                                 (if .input.run_in_background == true then "RUN_IN_BACKGROUND" else empty end)]
                                | map(select(. != null)) | join(" | ")) | tostring | .[0:200])' 2>/dev/null \
    | tail -n 40)"

row_state="$(jq -r --arg n "$NAME" '.agents[$n] | (.state // "?") + " sticky=" + (.sticky // "-")' "$REGISTRY" 2>/dev/null)"

# ---- tier 1: candidate filters ----------------------------------------------
ARTIFACT_RE='\.(png|svg|jpe?g|gif|pdf|mp4|html)\b'
SHOWN_RE='show-result|sot-fe preview|sot-nav'
candidates=()

case "$last_text" in *\?*) candidates+=("question") ;; esac

if printf '%s\n%s' "$tools" "$last_text" | grep -qE "$ARTIFACT_RE"; then
    printf '%s' "$tools" | grep -qE "$SHOWN_RE" || candidates+=("artifact")
fi

# Stale waiting: the row carries a sticky waiting marker, but this turn
# CONSUMED a background completion (a task-notification in the tail) without
# re-arming anything — the wait likely ended and the marker now paints a
# false purple with a dead summary between turns (a peer session, 2026-07-04).
case "$row_state" in
  *sticky=[!-]*)
    if printf '%s\n%s' "$tools" "$last_text" | grep -qiE "task-notification|completed|finished" \
        && ! printf '%s' "$tools" | grep -qE 'RUN_IN_BACKGROUND|^Monitor '; then
        candidates+=("stale-waiting")
    fi ;;
esac

# Blind badge: an image WAS surfaced this turn, but no Read/view of an image
# appears in the turn context — the session badged what a filename suggested,
# not what it saw (2026-07-03 incident: three near-black renders in a row).
if printf '%s' "$tools" | grep -E "$SHOWN_RE" | grep -qiE "$ARTIFACT_RE"; then
    printf '%s' "$tools" | grep -E "^Read " | grep -qiE "$ARTIFACT_RE" \
        || candidates+=("blind-badge")
fi

# The persistent comm inbox Monitor (comm-watch.sh) is the session's RECEIVE
# PATH, not a background job — every joined session arms one at bootstrap and it
# runs for the session's lifetime. Exclude it here, or every bootstrap turn gets
# a false "you armed a watcher — set waiting" nudge (ISD report, 2026-07-23).
# It still counts as "re-armed something" for the stale-waiting check above —
# suppressing a nudge is the conservative direction.
bg_tools="$(printf '%s' "$tools" | grep -vE 'comm-watch\.sh')"
if printf '%s' "$bg_tools" | grep -qE 'RUN_IN_BACKGROUND|^Monitor |^Agent |^Task '; then
    case "$row_state" in waiting*|blocked*|*sticky=[!-]*) ;; *) candidates+=("background") ;; esac
fi

# Clean at tier 1 → audited clean (empty stdout): cheaper AND stricter than the
# legacy grep (a '?' is a candidate here, not an automatic nudge).
[ ${#candidates[@]} -eq 0 ] && exit 0

# ---- rate limit: don't re-judge the same situation within 30 min -------------
sig="$(printf '%s|%s' "${candidates[*]}" "$last_text" | cksum | awk '{print $1}')"
sigfile="$STATE_DIR/auditor-$NAME"
if [ -f "$sigfile" ]; then
    read -r old_sig old_ts < "$sigfile" || true
    now=$(date +%s)
    if [ "${old_sig:-}" = "$sig" ] && [ $(( now - ${old_ts:-0} )) -lt 1800 ]; then
        exit 0   # same situation, recently judged/nudged — stay quiet
    fi
fi
printf '%s %s\n' "$sig" "$(date +%s)" > "$sigfile" 2>/dev/null || true

# ---- tier 2: one conservative Haiku judgment ---------------------------------
prompt="$(cat <<EOF
You audit the END of a coding-assistant session turn for housekeeping misses.
Be CONSERVATIVE: report a finding ONLY when clearly confident; when unsure, drop
it — a false alarm is much worse than a miss. Output ONLY JSON, no fences:
{"findings":[{"kind":"question|artifact|background","message":"<one short imperative sentence>"}]}
Empty findings array when clean.

Checks (only these; candidates pre-flagged by cheap filters — judge each):
$(printf -- '- %s\n' "${candidates[@]}")
- question: the final text asks the USER a real, turn-ending question they must
  answer (not rhetorical, not already answered, not "let me know if...") AND the
  work-state below is not already blocked. Finding message: tell the session to
  (a) run comm-status.sh blocked "<the question RESTATED as one clear standalone
  sentence>" and (b) end its continuation by restating that question plainly to
  the user — the user must see, at a glance, exactly what is being asked.
- artifact: the turn produced a clearly NEW user-facing result (fresh plot,
  figure, screenshot, render, PDF, report) and it was NOT surfaced in the
  user's nav/preview pane (no show-result / sot-fe preview call). The rule
  (per the maintainer): a new result MUST be shown in the nav pane — naming the path in
  text is not showing it. Intermediate/temp/test files do not count; when
  clearly a new result, DO intervene. Finding message: tell the session to
  badge <path> into the nav pane via the show-result skill NOW.
- background: the turn armed a background task/watcher/subagent that is still
  running at turn end, and the work-state below is not waiting/blocked. A task
  whose completion notification already appears in the turn does NOT count.
  Finding message: tell the session to run comm-status.sh waiting "<one clear
  sentence: WHAT is being monitored and what completion looks like>" and to
  state that same sentence to the user in its continuation.
- stale-waiting: the registry row carries a sticky waiting marker, but this
  turn consumed the completion of the awaited work (task-notification handled)
  and armed nothing new — the marker is now stale and paints a false purple
  with a dead summary between turns. Finding message: tell the session to run
  comm-status.sh working "<current activity>" (or idle/done) to CLEAR the
  finished wait — or, if it genuinely still waits on something else, to
  re-state it: comm-status.sh waiting "<the actual current wait>".
- blind-badge: an image WAS surfaced this turn (show-result / sot-fe preview)
  but no Read/view of an image appears in the turn context — the session
  badged what a filename suggested, not what it saw. NEVER suggest un-showing
  or delaying a badge (showing is unconditional). Finding message: tell the
  session to Read-view the surfaced file NOW and tell the user what they are
  looking at (a one-line critical read of the figure); if a different export
  is the legible one, badge that too and say which is which.

Work-state registry row: $row_state

Recent tool calls (name + arg excerpt):
$tools

Final assistant text:
$last_text
EOF
)"

resp="$(printf '%s' "$prompt" \
    | ( unset CLAUDECODE AI_AGENT CLAUDE_CODE_ENTRYPOINT 2>/dev/null || true
        for v in $(env | grep -oE '^CLAUDE_CODE_[A-Z_]*' 2>/dev/null); do unset "$v" 2>/dev/null || true; done
        exec timeout 45 "$CLAUDE_BIN" -p --model "$MODEL" --max-turns 1 ) 2>/dev/null)" || exit 3

# Strip optional markdown fences, parse findings; any parse failure → fail open.
json="$(printf '%s' "$resp" | sed -e 's/^```json//' -e 's/^```//' -e 's/```$//' | tr -d '\r')"
findings="$(printf '%s' "$json" | jq -r '.findings[]? | "[" + .kind + "] " + .message' 2>/dev/null)" || exit 3

[ -z "$findings" ] && exit 0
printf '%s' "$findings" | tr '\n' ' ; '
exit 0
