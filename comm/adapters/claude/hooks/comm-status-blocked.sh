#!/usr/bin/env bash
# comm-status-blocked.sh — Claude Code `PreToolUse` hook (matcher: AskUserQuestion):
# mark this comm agent blocked — it just opened a question for the user.
#
# Wired as a PreToolUse hook matched to the AskUserQuestion tool (see comm.jl /
# update_comm). It fires the instant the agent opens a structured question, so
# `blocked` (red) ALWAYS means a real pending question — never the idle-nudge
# false-positive the old `Notification` wiring produced (Notification also fires
# after a stretch of plain idle, which lit agents as blocked while merely waiting). The
# tool PAUSES the turn (no Stop) while awaiting the answer, so the block holds
# until the user replies (UserPromptSubmit -> working clears it).
#
# Questions asked in PLAIN TEXT (no tool) have no automatic signal — Claude emits
# no "asked a question" event distinct from idle. For those an agent self-reports
# with `comm-status.sh blocked "<the question>"` right before asking (the question
# becomes the row summary). The Stop hook's idle floor will NOT clobber that block
# (comm-status.sh soft-idle guard), so it survives to the user's reply.
#
# Safety rests on comm-status.sh's self-gating: a non-comm session is a silent
# no-op (rc 0). Output swallowed, always exit 0 so the hook can never block.
#
# Source of truth: comm/adapters/claude/hooks/comm-status-blocked.sh in Ship of Tools,
# deployed to ~/.sot-comm/bin by ShipTools.update_comm(). Edit it there.
# A headless claude launched BY comm tooling (the turn auditor's tier-2 call)
# runs these same hooks under the parent's identity: its prompt hook painted
# the parent's row working and its Stop hook floored it to done, one second
# after the parent's own marker had stamped blocked (field report, 2026-09-18).
# The launcher sets SOT_COMM_HOOKS=off; every status hook stands down on it.
[ "${SOT_COMM_HOOKS:-}" = off ] && exit 0
STATUS="${SOT_COMM_HOME:-$HOME/.sot-comm}/bin/comm-status.sh"
[ -x "$STATUS" ] && "$STATUS" blocked >/dev/null 2>&1 || true
exit 0
