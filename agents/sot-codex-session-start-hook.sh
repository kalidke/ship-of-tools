#!/usr/bin/env bash
# Return the existing read-only bootstrap as model context for the row's own context transitions.
exec 2>/dev/null
set -o pipefail
skip() { cat >/dev/null; exit 0; }
[ -n "${SOT_CODEX_THREAD_FILE:-}" ] && [ "${SOT_AGENT_KIND:-}" = codex ] && [ -n "${SOT_WORKSPACE_ID:-}" ] || skip
[ "${SOT_COMM_HOOKS:-}" != off ] && [ -f "${SOT_COMM_SELF_FILE:-}" ] || skip
jq -se 'length == 1 and (.[0] | .hook_event_name == "SessionStart" and
    (.source == "clear" or .source == "compact" or .source == "resume") and
    (.session_id | type == "string" and length > 0) and .agent_id == null and .agent_type == null)' >/dev/null || skip

bin=$(dirname "$0") || exit 0
. "$bin/comm-lib.sh" >/dev/null || exit 0
SOT_COMM_SELF_FILE= sot_require_agent </dev/null >/dev/null || exit 0
# The bootstrap also returns zero for identity refusals. Only its successful header admits the complete output.
output=$("$bin/comm-session-start.sh" --again </dev/null |
    jq -Rsec 'select(test("^BOOTSTRAP-ARM handle=[^[:space:]]+ identity=ok WAKE: daemon\n")) |
        {hookSpecificOutput:{hookEventName:"SessionStart",additionalContext:.}}') || exit 0
trap '' PIPE
printf '%s\n' "$output" || exit 0
