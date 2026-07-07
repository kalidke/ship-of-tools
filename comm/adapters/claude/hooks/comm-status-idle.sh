#!/usr/bin/env bash
# comm-status-idle.sh — Claude Code `Stop` hook for comm agents. Two jobs:
#
#   (1) NUDGE (reinforce self-report). If a JOINED comm agent ends a turn whose
#       last reply contains a `?` and it did NOT already self-mark blocked/waiting,
#       remind it — via a Stop `decision:block` whose reason is fed back to the
#       model — to run `comm-status.sh blocked "<q>"` IF that `?` was a real
#       question for the user. It is a REMINDER, never an auto-mark: the MODEL
#       decides whether the `?` was actually a blocking question (the hook cannot
#       tell rhetorical from real), so there is no false-positive block — at worst a one-line
#       "that was rhetorical" continuation. Plain-text questions fire no automatic
#       signal (only the AskUserQuestion tool does), so without this the row looks
#       idle while the agent is actually waiting.
#
#   (2) IDLE FLOOR. Otherwise mark the agent idle (soft — never clobbers a
#       deliberate blocked/waiting; see comm-status.sh's soft-idle guard).
#
# Wired as a global Stop hook in ~/.claude/settings.json (comm.jl / update_comm).
# It fires at every turn-end in EVERY session. CRITICAL SAFETY: the nudge (which
# BLOCKS the stop / forces a continuation) fires ONLY for a joined comm agent — a
# non-comm session (human shell, etc.) takes the plain idle-floor path and is
# NEVER blocked. Every failure path also falls through to the floor + exit 0, so
# the hook can never wedge a turn.
#
# Source of truth: comm/adapters/claude/hooks/comm-status-idle.sh in Ship of Tools,
# deployed to ~/.sot-comm/bin by ShipTools.update_comm(). Edit it there.
set -uo pipefail
HOME_DIR="${SOT_COMM_HOME:-$HOME/.sot-comm}"
STATUS="$HOME_DIR/bin/comm-status.sh"
REGISTRY="$HOME_DIR/registry.json"
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

idle_floor() { [ -x "$STATUS" ] && COMM_STATUS_SOFT=1 "$STATUS" idle >/dev/null 2>&1 || true; }

# Stop-hook input (JSON on stdin): {stop_hook_active, transcript_path, ...}.
input="$(cat 2>/dev/null || true)"
jqget() { printf '%s' "$input" | jq -r "$1" 2>/dev/null || true; }

# Loop guard: if we are ALREADY in a stop-hook continuation, never re-nudge —
# floor + let the turn end (one nudge per turn, no infinite continue-loop).
[ "$(jqget '.stop_hook_active // false')" = "true" ] && { idle_floor; exit 0; }

# Comm-agent gate (the safety line): resolve our handle; ONLY a joined comm agent
# with a registry row is eligible for the nudge. Anyone else → plain idle floor,
# NEVER a block.
NAME=""
[ -x "$SELF_DIR/comm-context.sh" ] && eval "$("$SELF_DIR/comm-context.sh" 2>/dev/null)" 2>/dev/null || true
if [ -z "${NAME:-}" ] || ! jq -e --arg n "${NAME:-}" '.agents[$n]' "$REGISTRY" >/dev/null 2>&1; then
    idle_floor; exit 0
fi

# Skip the nudge if the agent already self-marked blocked/waiting this turn —
# disciplined turns cost nothing; the nudge fires only when it FORGOT.
cur="$(jq -r --arg n "$NAME" '.agents[$n].state // ""' "$REGISTRY" 2>/dev/null || echo "")"
{ [ "$cur" = blocked ] || [ "$cur" = waiting ]; } && { idle_floor; exit 0; }

tp="$(jqget '.transcript_path // empty')"

# TIERED TURN AUDITOR (v1, 2026-07-02): deterministic pre-filters + a
# conservative Haiku judge (comm-turn-auditor.sh) check the turn for misses —
# real turn-ending question w/o blocked, user-facing artifact never surfaced
# to the FE (show-result), background task armed w/o waiting. Contract:
#   rc 0 + output → confirmed findings, nudge with them;
#   rc 0 + empty  → audited CLEAN (Haiku judged rhetorical-vs-real etc.) —
#                   skip the legacy grep nudge, plain floor;
#   rc 3          → auditor off/unavailable → legacy '?' grep nudge below.
AUDITOR="$SELF_DIR/comm-turn-auditor.sh"
if [ -x "$AUDITOR" ] && [ -n "$tp" ]; then
    findings="$("$AUDITOR" "$NAME" "$tp" 2>/dev/null)"; arc=$?
    if [ "$arc" -eq 0 ]; then
        if [ -n "$findings" ]; then
            jq -nc --arg f "$findings" '{
              decision: "block",
              reason: ("Turn-end audit: " + $f + " -- IF a finding is real, act on it now AND clearly RESTATE it for the user: blocked -> restate the exact question you are awaiting (one standalone sentence, as BOTH the comm-status summary and the final line of your reply); waiting -> state plainly what is being monitored and what completion looks like (same two places); artifact -> badge it via the show-result skill. IF a finding is wrong (rhetorical question, artifact already shown, job already done), just end the turn normally. This audit will not re-fire for the same situation.")
            }'
            exit 0
        fi
        idle_floor; exit 0
    fi
fi

# LEGACY FALLBACK (auditor disabled/unavailable): grep the last reply for `?`.
last_text=""
if [ -n "$tp" ] && [ -r "$tp" ]; then
    last_text="$(tail -n 200 "$tp" 2>/dev/null \
        | jq -r 'select(.type=="assistant") | .message.content[]? | select(.type=="text") | .text' 2>/dev/null \
        | tail -1)"
fi

if printf '%s' "$last_text" | grep -q '?'; then
    # NUDGE — block the stop with a reminder. The model gates: self-report or not.
    jq -nc '{
      decision: "block",
      reason: "Reminder: your last reply contains a question mark, and a plain-text question (not the AskUserQuestion tool) fires no automatic frontend signal. IF you are ending this turn AWAITING THE USER on a blocking question, run  ~/.sot-comm/bin/comm-status.sh blocked \"<the question>\"  now so your row shows red on the frontend. IF the question(s) were rhetorical or already answered, just end the turn normally — this nudge will not fire again this turn."
    }'
    exit 0
fi

# No question → plain idle floor.
idle_floor
exit 0
