#!/usr/bin/env bash
# codex-status-blocked.sh — codex `PermissionRequest` hook: a permission dialog
# waits on a person, so the row reads red in the state-nav (ADR 0044 amendment,
# `dialog`).
#
# It stamps the event `dialog "codex permission request: <tool>"`. The row clears
# it when the call returns (comm-status-heartbeat.sh on Codex's PostToolUse), or
# at the turn's end (Stop) or the next prompt; it never ends the turn itself, so
# an approved request no longer leaves red behind for the rest of it. The summary
# names the tool asking, so the row reads "blocked · codex permission request: Bash".
#
# Self-gating: comm-status.sh no-ops in any pane without a registry row, so
# non-SoT codex sessions are untouched. Always exits 0 — a hook must never
# wedge the permission flow (advisory only; it does not answer the request).
#
# Source of truth: comm/work_state/hooks/codex-status-blocked.sh in
# Ship of Tools, deployed to ~/.sot-comm/bin by ShipTools.update_comm().
set -uo pipefail
[ "${SOT_COMM_HOOKS:-}" = off ] && exit 0
COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
STATUS="$COMM_HOME/bin/comm-status.sh"
[ -x "$STATUS" ] || exit 0
tool="$(jq -r '.tool_name // ""' 2>/dev/null || true)"
"$STATUS" dialog "codex permission request${tool:+: $tool}" >/dev/null 2>&1 || true
exit 0
