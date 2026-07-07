#!/usr/bin/env bash
# comm-status-working.sh — Claude Code `UserPromptSubmit` hook: mark this comm
# agent working.
#
# Wired as a global UserPromptSubmit hook in ~/.claude/settings.json (see comm.jl
# / update_comm). It fires the INSTANT a turn starts — automatic, deterministic,
# zero model cooperation. This is the event-driven work-state signal that
# replaces pane-scraping: a turn starting IS the agent beginning to work, known
# the moment it happens rather than guessed from the screen 2 seconds later.
#
# comm-status.sh keeps the prior summary when none is passed, so the model's last
# "working on X" note (if it set one) rides along with the working state.
#
# Safety rests entirely on comm-status.sh's own self-gating: in any session that
# is NOT a joined comm agent ($NAME empty, or no registry row) it is a silent
# no-op with rc 0. We swallow output and always exit 0 so the hook can never
# block or delay a turn.
#
# Source of truth: comm/adapters/claude/hooks/comm-status-working.sh in Ship of Tools,
# deployed to ~/.sot-comm/bin by ShipTools.update_comm(). Edit it there.
# SOFT write: a turn starting is truthfully "working", but it must PRESERVE a
# live sticky-waiting marker (see comm-status.sh header) — the user prompting
# the session doesn't finish its background job; before this flag, the
# waiting → prompt(working) → stop(idle) cycle landed a still-waiting session
# on green. Only the model's explicit non-soft report clears the marker.
# HIERARCHY GUARD (maintainer decision, 2026-07-04: "question/red always first priority"):
# a BLOCKED row means a question is pending ON THE USER. Machine-initiated
# turns — teammate relay messages, task/monitor notifications — also fire
# UserPromptSubmit, and before this guard they flipped red to green with the
# question still unanswered. So: when the row is blocked AND the prompt looks
# like a machine turn, DON'T touch the state. A genuine human prompt (the
# answer) still clears red to working. Detection is by prompt shape; stdin is
# the hook's JSON envelope ({"prompt": ...}).
COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
STATUS="$COMM_HOME/bin/comm-status.sh"
[ -x "$STATUS" ] || exit 0
prompt="$(jq -r '.prompt // ""' 2>/dev/null || true)"   # consumes hook stdin
if [ -n "$prompt" ]; then
    case "$prompt" in
        "[SYSTEM NOTIFICATION"*|*"<task-notification>"*|"[relay] from"*|        \[*:*\]\ *)   # teammate messages arrive as "[handle:team] ..."
            NAME=""
            SELF_DIR="$COMM_HOME/bin"
            eval "$("$SELF_DIR/comm-context.sh" 2>/dev/null)" 2>/dev/null || true
            if [ -n "${NAME:-}" ]; then
                cur="$(jq -r --arg n "$NAME" '.agents[$n].state // ""' "$COMM_HOME/registry.json" 2>/dev/null || true)"
                [ "$cur" = blocked ] && exit 0
            fi
            ;;
    esac
fi
COMM_STATUS_SOFT=1 "$STATUS" working >/dev/null 2>&1 || true
exit 0
