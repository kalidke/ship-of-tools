#!/usr/bin/env bash
# codex-status-blocked.sh — codex `PermissionRequest` hook: mark the session
# BLOCKED (red) in the state-nav (ADR 0031).
#
# Codex has no AskUserQuestion tool; a permission prompt is its nearest
# "needs the USER to act" — exactly what red means. The summary names the
# tool asking, so the row reads "blocked · codex permission: Bash …".
# comm-status.sh explicit `blocked` PRESERVES a live sticky-waiting marker
# (both can be true; red wins display precedence — the canonical hierarchy).
#
# Self-gating: comm-status.sh no-ops in any pane without a registry row, so
# non-SoT codex sessions are untouched. Always exits 0 — a hook must never
# wedge the permission flow (advisory only; it does not answer the request).
#
# Source of truth: comm/adapters/codex/hooks/codex-status-blocked.sh in
# Ship of Tools, deployed to ~/.sot-comm/bin by ShipTools.update_comm().
set -uo pipefail
STATUS="${SOT_COMM_HOME:-$HOME/.sot-comm}/bin/comm-status.sh"
[ -x "$STATUS" ] || exit 0
tool="$(jq -r '.tool_name // ""' 2>/dev/null || true)"
"$STATUS" blocked "codex permission request${tool:+: $tool}" >/dev/null 2>&1 || true
exit 0
