#!/usr/bin/env bash
# comm-subagent-guard.sh — Claude Code `PreToolUse` hook (matcher: Bash): refuse a sot-comm script run by a subagent.
#
# A subagent uses no mail (decision 0031): it reports to its parent, which sends. Nothing in a subagent's shell
# environment or ancestry differs from its session's; the hook input can: Claude Code adds `agent_id` to the input of
# a hook that fires inside a subagent, and the main session's input has none. The decision is `sotd comm
# subagent-guard` (rust/backend/src/comm/CLAUDE.md): with `agent_id` set, a Bash command that runs an installed comm
# script gets the documented deny decision. Every other case allows silently, a missing sotd included: a session is
# never locked out of its own mail, and an input with no `agent_id` starts nothing.
#
# Source of truth: comm/work_state/hooks/comm-subagent-guard.sh in Ship of Tools, deployed to ~/.sot-comm/bin by
# ShipTools.update_comm(). Edit it there.
payload="$(cat 2>/dev/null || true)"
case "$payload" in *'"agent_id"'*) ;; *) exit 0 ;; esac
. "$(dirname "${BASH_SOURCE[0]}")/comm-lib.sh" 2>/dev/null || exit 0
printf '%s' "$payload" | sot_comm subagent-guard 2>/dev/null
exit 0
