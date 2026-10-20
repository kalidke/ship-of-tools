#!/usr/bin/env bash
# comm-subagent-guard.sh — Claude Code `PreToolUse` hook (matcher: Bash):
# refuse a sot-comm script run by a subagent.
#
# A subagent uses no mail (decision 0031): it reports to its parent, which sends.
# Nothing in a subagent's shell environment or ancestry differs from its
# session's, so the scripts cannot tell it apart; the hook input can. Claude
# Code adds `agent_id` to the input of a hook that fires inside a subagent, and
# the main session's input has none. When `agent_id` is non-empty and the Bash
# command names a `comm-*.sh` word (any path, any quoting), this prints the
# documented deny decision and exits 0.
#
# Every other case allows silently: no `agent_id`, any other command, a missing
# jq, input that does not parse. A session is never locked out of its own mail.
#
# Source of truth: comm/work_state/hooks/comm-subagent-guard.sh in Ship of Tools,
# deployed to ~/.sot-comm/bin by ShipTools.update_comm(). Edit it there.
payload="$(cat 2>/dev/null || true)"
agent_id="$(printf '%s' "$payload" | jq -r '.agent_id // ""' 2>/dev/null || true)"
[ -n "$agent_id" ] || exit 0
cmd="$(printf '%s' "$payload" | jq -r '.tool_input.command // ""' 2>/dev/null || true)"
# Quotes and backslashes inside a word (comm-"send".sh, comm\-send.sh) do not hide it.
cmd="$(printf '%s' "$cmd" | tr -d "'\"\\\\")"
if printf '%s' "$cmd" | grep -Eq 'comm-[A-Za-z0-9_-]+\.sh'; then
    printf '%s\n' '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"A subagent does not use sot-comm; report to your parent session, which sends."}}'
fi
exit 0
