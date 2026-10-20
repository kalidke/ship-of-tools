#!/usr/bin/env bash
# test-subagent-guard.sh — comm-subagent-guard.sh refuses a sot-comm script run by a subagent (agent_id in the hook
# input), and allows everything else silently. Fixtures only: the hook is fed its stdin JSON; no Claude Code runs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOOK="$SCRIPT_DIR/../work_state/hooks/comm-subagent-guard.sh"
fail=0
DENY='{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"A subagent does not use sot-comm; report to your parent session, which sends."}}'

# input AGENT_ID COMMAND — the hook's stdin JSON; an empty AGENT_ID leaves the field out.
input() {
    if [ -n "$1" ]; then
        jq -nc --arg a "$1" --arg c "$2" '{agent_id:$a, agent_type:"general-purpose", tool_name:"Bash", tool_input:{command:$c}}'
    else
        jq -nc --arg c "$2" '{tool_name:"Bash", tool_input:{command:$c}}'
    fi
}
# check NAME EXPECTED_OUTPUT AGENT_ID COMMAND
check() {
    local out rc
    out="$(input "$3" "$4" | "$HOOK" 2>/dev/null)"; rc=$?
    if [ "$rc" -ne 0 ] || [ "$out" != "$2" ]; then
        echo "FAIL: $1 (rc=$rc, output: $out)"; fail=1
    else
        echo "ok: $1"
    fi
}

check "subagent comm-send" "$DENY" agent-1 "~/.sot-comm/bin/comm-send.sh @x 'hi'"
check "subagent bash -c comm-poll" "$DENY" agent-1 'bash -c "comm-poll.sh"'
check "subagent comm-status via HOME" "$DENY" agent-1 '$HOME/.sot-comm/bin/comm-status.sh waiting x'
check "subagent quoted word" "$DENY" agent-1 "'/h/.sot-comm/bin/comm-'\"send\".sh @x hi"
check "subagent after other work" "$DENY" agent-1 'ls && cd /tmp; comm-poll.sh'
check "main session comm-send" "" "" "~/.sot-comm/bin/comm-send.sh @x 'hi'"
check "main session comm-poll" "" "" 'bash -c "comm-poll.sh"'
check "main session comm-status" "" "" '$HOME/.sot-comm/bin/comm-status.sh waiting x'
check "subagent ls" "" agent-1 'ls'
check "subagent git commit" "" agent-1 'git commit -m "comm work"'
check "subagent path with comm" "" agent-1 'cat /x/comm/notes.sh'

out="$(printf '%s' 'not json {' | "$HOOK" 2>/dev/null)"; rc=$?
[ "$rc" -eq 0 ] && [ -z "$out" ] && echo "ok: malformed JSON" || { echo "FAIL: malformed JSON (rc=$rc, output: $out)"; fail=1; }

# No jq on PATH: an allow, even for a subagent's mail command.
nojq="$(mktemp -d "${TMPDIR:-/tmp}/sot-subagent-guard-XXXXXX")" && [ -d "$nojq" ] || exit 1
trap 'rm -rf "${nojq:?}"' EXIT
for t in bash cat printf tr grep; do ln -s "$(command -v "$t")" "$nojq/$t"; done
out="$(input agent-1 'comm-send.sh @x hi' | PATH="$nojq" "$(command -v bash)" "$HOOK" 2>/dev/null)"; rc=$?
[ "$rc" -eq 0 ] && [ -z "$out" ] && echo "ok: no jq" || { echo "FAIL: no jq (rc=$rc, output: $out)"; fail=1; }

exit "$fail"
