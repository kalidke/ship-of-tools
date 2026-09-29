#!/usr/bin/env bash
# comm-status-blocked.sh — Claude Code `PreToolUse` hook (matcher: AskUserQuestion):
# mark this comm agent blocked — it just opened a question for the user.
#
# Wired as a PreToolUse hook matched to the AskUserQuestion tool (see comm.jl /
# update_comm). It fires the instant the agent opens a structured question, so
# `blocked` (red) ALWAYS means a real pending question — never the idle-nudge
# false-positive the old `Notification` wiring produced (Notification also fires
# after a stretch of plain idle, which lit agents as blocked while merely waiting).
#
# The tool then PAUSES the turn — no PostToolUse until the user answers — so
# this hook also sends `stop`: the session is yielding to the owner exactly
# like a real turn end (ADR 0044 amendment), and `stop` sets `floor`-derived
# `done` only when nothing else is pending, never touching the `question` it
# just set. Without a `stop` here the row would sit `working` (floor still
# set) until the answer, masking the question underneath it in the reduction.
#
# Questions asked in PLAIN TEXT (no tool) have no automatic signal — Claude emits
# no "asked a question" event distinct from idle. For those an agent self-reports
# with `comm-status.sh blocked "<the question>"` right before asking (the question
# becomes the row summary).
#
# The answer arrives as this same tool's PostToolUse, handled by
# comm-status-heartbeat.sh's own AskUserQuestion branch (which runs before
# its early-throttle tick check, review finding 2026-09-19: a teammate's or
# subagent's tool call sharing this session id must never be able to swallow
# the owner's answer by re-touching that tick) — proof that completion is
# THIS dialog, not a foreign AskUserQuestion resolving while this row's real
# question is still open, is a marker dropped below, keyed by the envelope's
# own `tool_use_id`: only the matching PostToolUse may consume it and treat
# its completion as an answer (field report, 2026-09-27: a badge showed idle
# while a real question was open, root cause a PostToolUse that cleared the
# question unconditionally on ANY completed AskUserQuestion).
#
# DENY: a permission denial fires no hook of its own (true of Escape and of
# an explicit "No" alike), so nothing else clears `question` when the tool
# never runs. A bare "No" (the turn dies) writes the same transcript marker a
# user interrupt does; comm-wake.sh's silent-exit watcher reads it, and
# comm-status.sh's `interrupted` event clears `question` because it matches
# `question_prompt`, stamped above from THIS call's own `prompt_id` — never
# from a guess. KNOWN GAP: "No" WITH feedback writes no marker (the turn
# keeps running instead), so this row sits red for the rest of that turn —
# comm-status-idle.sh's own end-of-turn nudge treats the still-open
# `question` as parked and asks the model to restate it, which will read as
# stale here since the denial already answered it. It self-heals at the
# NEXT genuine user prompt (that event already clears `question`), never
# earlier. Fixing the "green while it continues" half needs a signal this
# hook does not have: no hook fires between a feedback-deny and whatever
# tool call the model tries next.
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
COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
STATUS="$COMM_HOME/bin/comm-status.sh"
# One read of stdin (the hook envelope): the tool_use_id below for the
# answer marker, and prompt_id for the silent-exit detector (comm-wake.sh) —
# stamped into `question_prompt` so a turn that dies with this dialog still
# open (Escape, or a bare "No") can clear its own question, and only its own
# (comm-status.sh's `interrupted` event compares this id, never guesses).
_envelope="$(cat)"
tool_use_id="$(printf '%s' "$_envelope" | jq -r '.tool_use_id // ""' 2>/dev/null || true)"
prompt_id="$(printf '%s' "$_envelope" | jq -r '.prompt_id // ""' 2>/dev/null || true)"
# Drop the answer marker before stamping `blocked`, keyed by this dialog's
# own tool_use_id (sanitized: an id is free-text as far as we know, and this
# becomes a filename). No id, no marker — the PostToolUse side then finds
# nothing to consume and this dialog's answer is simply never fast-tracked
# (comm-status.sh's own self-gating still applies to everything else).
if [ -n "$tool_use_id" ]; then
    mkdir -p "$COMM_HOME/state" 2>/dev/null || true
    : > "$COMM_HOME/state/askq-$(printf '%s' "$tool_use_id" | tr -c 'A-Za-z0-9._-' '_').marker" 2>/dev/null || true
fi
if [ -x "$STATUS" ]; then
    COMM_STATUS_PROMPT_ID="$prompt_id" "$STATUS" blocked >/dev/null 2>&1 || true
    "$STATUS" stop >/dev/null 2>&1 || true
fi
exit 0
