#!/usr/bin/env bash
# comm-status-permission.sh — Claude Code `PermissionRequest` hook (no
# matcher: a permission prompt can gate any tool): mark this comm agent
# blocked — the owner is being asked to approve or deny a tool call.
#
# The defect this closes: a permission dialog raises no `question` at all
# today (nothing was wired to `PermissionRequest`), so the row stays green
# — `floor` is still set from the running turn — while the owner is the one
# being asked to act. `AskUserQuestion` (comm-status-blocked.sh) already
# covers the model's OWN structured question; this is the harness's
# permission gate, which can sit in front of any tool, so it cannot key off
# a tool name the way that hook does.
#
# Like AskUserQuestion, the dialog PAUSES the turn — no PostToolUse for the
# gated tool until the owner answers — so this hook also sends `stop`
# exactly as comm-status-blocked.sh does, for the same reason: without it
# the row would sit `working` (floor still set) and mask the question
# underneath it in the reduction.
#
# THE ANSWER, both arms:
#   - Approve: the tool then runs and its OWN PostToolUse fires. A marker
#     dropped below, keyed by this call's `tool_use_id`, is consumed there
#     by comm-status-heartbeat.sh (the same mechanism, and the same
#     identity guard, as the AskUserQuestion answer) — see that hook.
#   - Deny: no tool runs, so no PostToolUse. `prompt_id` (read below) is
#     stamped into `question_prompt` so a turn that dies with this dialog
#     still open — Escape, or a bare "No" — can clear its own question and
#     only its own, via the SAME path a user interrupt takes; see
#     comm-status-blocked.sh's own DENY paragraph for the mechanism and its
#     one known gap (a "No" WITH feedback, where the turn keeps running).
#
# Safety rests on comm-status.sh's self-gating: a non-comm session is a
# silent no-op (rc 0). Output swallowed, always exit 0 so the hook can
# never block the permission flow (advisory only; it does not answer it).
#
# Source of truth: comm/adapters/claude/hooks/comm-status-permission.sh in
# Ship of Tools, deployed to ~/.sot-comm/bin by ShipTools.update_comm().
# Edit it there.
# A headless claude launched BY comm tooling (the turn auditor's tier-2 call)
# runs these same hooks under the parent's identity: its prompt hook painted
# the parent's row working and its Stop hook floored it to done, one second
# after the parent's own marker had stamped blocked (field report, 2026-09-18).
# The launcher sets SOT_COMM_HOOKS=off; every status hook stands down on it.
[ "${SOT_COMM_HOOKS:-}" = off ] && exit 0
COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
STATUS="$COMM_HOME/bin/comm-status.sh"
# One read of stdin (the hook envelope).
_envelope="$(cat)"
tool="$(printf '%s' "$_envelope" | jq -r '.tool_name // ""' 2>/dev/null || true)"
tool_use_id="$(printf '%s' "$_envelope" | jq -r '.tool_use_id // ""' 2>/dev/null || true)"
prompt_id="$(printf '%s' "$_envelope" | jq -r '.prompt_id // ""' 2>/dev/null || true)"
# Drop the approve-answer marker, keyed by this call's own tool_use_id — the
# SAME naming convention as the askq marker, deliberately in a different
# namespace (`perm-`) so the two can never collide or cross-consume.
if [ -n "$tool_use_id" ]; then
    mkdir -p "$COMM_HOME/state" 2>/dev/null || true
    : > "$COMM_HOME/state/perm-$(printf '%s' "$tool_use_id" | tr -c 'A-Za-z0-9._-' '_').marker" 2>/dev/null || true
fi
if [ -x "$STATUS" ]; then
    COMM_STATUS_PROMPT_ID="$prompt_id" "$STATUS" blocked "permission request${tool:+: $tool}" >/dev/null 2>&1 || true
    "$STATUS" stop >/dev/null 2>&1 || true
fi
exit 0
