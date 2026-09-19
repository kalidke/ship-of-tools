#!/usr/bin/env bash
# codex-status-blocked.sh — codex `PermissionRequest` hook: mark the session
# BLOCKED (red) in the state-nav (ADR 0031), and end the turn like a Stop.
#
# Codex has no AskUserQuestion tool; a permission prompt is its nearest
# "needs the USER to act" — exactly what red means. The summary names the
# tool asking, so the row reads "blocked · codex permission: Bash …".
# comm-status.sh's `blocked` sets `question` and PRESERVES `waiting` (both
# can be true; red outranks purple in the reduction). The prompt then PAUSES
# for the user, so this hook also sends `stop` — the session is yielding to
# the owner exactly like a real turn end (ADR 0044 amendment) — after
# clearing the heartbeat's throttle tick (same key formula as
# comm-status-heartbeat.sh's own) so the 10s early throttle can't swallow the
# answer's next tool call.
#
# Self-gating: comm-status.sh no-ops in any pane without a registry row, so
# non-SoT codex sessions are untouched. Always exits 0 — a hook must never
# wedge the permission flow (advisory only; it does not answer the request).
#
# Source of truth: comm/adapters/codex/hooks/codex-status-blocked.sh in
# Ship of Tools, deployed to ~/.sot-comm/bin by ShipTools.update_comm().
set -uo pipefail
COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
STATUS="$COMM_HOME/bin/comm-status.sh"
[ -x "$STATUS" ] || exit 0
rm -f "$COMM_HOME/state/hb-$(printf '%s' "${CLAUDE_CODE_SESSION_ID:-${SOT_WORKSPACE_ID:-$PPID}}" | tr -c 'A-Za-z0-9._-' '_').tick" 2>/dev/null
tool="$(jq -r '.tool_name // ""' 2>/dev/null || true)"
"$STATUS" blocked "codex permission request${tool:+: $tool}" >/dev/null 2>&1 || true
"$STATUS" stop >/dev/null 2>&1 || true
exit 0
