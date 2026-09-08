#!/usr/bin/env bash
# comm-status-working.sh — Claude Code `UserPromptSubmit` hook: mark this comm
# agent working, and say WHO started the turn.
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
#
# SOFT write: a turn starting is truthfully "working", but it must PRESERVE a
# live sticky-waiting marker (see comm-status.sh header) — the user prompting
# the session doesn't finish its background job. Only the model's explicit
# non-soft report clears the marker.
#
# TURN ORIGIN (ADR 0044, 2026-09-08): this hook is the ONE writer that can tell
# a genuine human prompt from a machine-initiated turn — teammate relay
# messages, task/monitor notifications, system notifications also fire
# UserPromptSubmit. It classifies by prompt shape (stdin is the hook's JSON
# envelope, {"prompt": ...}) and passes COMM_STATUS_ORIGIN=user|machine. Every
# decision that depends on it lives in comm-status.sh, not here:
#   - HIERARCHY GUARD (maintainer 2026-07-04, "question/red always first
#     priority"): a machine turn must not flip a BLOCKED (question pending on
#     the user) or DONE (finished, unread) row to green; a genuine human prompt
#     still does. comm-status.sh holds the state on a machine origin — and
#     still records the origin, so the turn-end floor can never read a stale
#     "user" from an earlier turn and paint a machine turn blue.
#   - The turn-end floor paints blue only for origin=user (ADR 0044).
set -uo pipefail
COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
STATUS="$COMM_HOME/bin/comm-status.sh"
[ -x "$STATUS" ] || exit 0
prompt="$(jq -r '.prompt // ""' 2>/dev/null || true)"   # consumes hook stdin
ORIGIN=user
case "$prompt" in
    "[SYSTEM NOTIFICATION"*|*"<task-notification>"*|"[relay] from"*|\[*:*\]\ *)   # teammate messages arrive as "[handle:team] ..."
        ORIGIN=machine ;;
esac
COMM_STATUS_SOFT=1 COMM_STATUS_ORIGIN="$ORIGIN" "$STATUS" working >/dev/null 2>&1 || true
exit 0
