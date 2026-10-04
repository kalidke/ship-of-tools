#!/usr/bin/env bash
# comm-status-working.sh — Claude Code `UserPromptSubmit` hook: tell
# comm-status.sh a turn is starting, and WHO started it.
#
# Wired as a global UserPromptSubmit hook in ~/.claude/settings.json (see comm.jl
# / update_comm). It fires the INSTANT a turn starts — automatic, deterministic,
# zero model cooperation. This is the event-driven work-state signal that
# replaces pane-scraping: a turn starting IS the agent beginning to work, known
# the moment it happens rather than guessed from the screen 2 seconds later.
#
# Safety rests entirely on comm-status.sh's own self-gating: in any session that
# is NOT a joined comm agent ($NAME empty, or no registry row) the `prompt`
# event is a silent no-op with rc 0. We swallow output and always exit 0 so
# the hook can never block or delay a turn.
#
# Source of truth: comm/adapters/claude/hooks/comm-status-working.sh in Ship of Tools,
# deployed to ~/.sot-comm/bin by ShipTools.update_comm(). Edit it there.
#
# TURN ORIGIN (ADR 0044): this hook is the ONE writer that can tell a genuine
# human prompt from a machine-initiated turn — teammate relay messages,
# task/monitor notifications, system notifications also fire
# UserPromptSubmit. It classifies by prompt shape (stdin is the hook's JSON
# envelope, {"prompt": ...}) and passes COMM_STATUS_ORIGIN=user|machine to the
# `prompt` event: comm-status.sh sets `floor` to that origin, and a `user`
# origin also clears an open `question` and a stale `done` (typing into the
# session answers it and reads it). A machine origin clears nothing — the
# reduction (comm-status.sh) puts `floor` above `waiting`, so a purple row
# never needs a hold: a machine wake simply paints green for the turn and the
# closing Stop puts the row back where the still-set facts say it belongs.
set -uo pipefail
# A headless claude launched BY comm tooling (the turn auditor's tier-2 call)
# runs these same hooks under the parent's identity: its prompt hook painted
# the parent's row working and its Stop hook floored it to done, one second
# after the parent's own marker had stamped blocked (field report, 2026-09-18).
# The launcher sets SOT_COMM_HOOKS=off; every status hook stands down on it.
[ "${SOT_COMM_HOOKS:-}" = off ] && exit 0
COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
STATUS="$COMM_HOME/bin/comm-status.sh"
[ -x "$STATUS" ] || exit 0
prompt="$(jq -r '.prompt // ""' 2>/dev/null || true)"   # consumes hook stdin
ORIGIN=user
# Twin copy (2026-09-15): comm-status-idle.sh's turn-origin correction
# classifies a transcript prompt record with this same pattern list, kept in
# sync by hand -- both hooks stay standalone, no shared library.
case "$prompt" in
    "[SYSTEM NOTIFICATION"*|*"<task-notification>"*|"[relay] from"*|"[sot-comm] "*|\[*:*\]\ *)   # teammate messages arrive as "[handle:team] ..."
        ORIGIN=machine ;;
    # The harness's own wrappers (2026-09-14, owner: "the stop hook is
    # triggering an almost identical rehash of the SITREP"): a subagent or
    # peer-session report, a cross-session message, and the Stop hook's own
    # send-back are machine wakes too -- a human typed none of them, so none
    # of them owes a closing block at turn end.
    "Another Claude session sent a message"*|*"<teammate-message"*|*"<agent-message"*|*"<cross-session-message"*|"Stop hook feedback:"*)
        ORIGIN=machine ;;
esac
COMM_STATUS_ORIGIN="$ORIGIN" "$STATUS" prompt >/dev/null 2>&1 || true
exit 0
