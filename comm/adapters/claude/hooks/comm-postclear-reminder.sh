#!/usr/bin/env bash
# comm-postclear-reminder.sh — Claude Code `SessionStart` hook (matcher:
# clear): print short context after a `/clear`, never a re-bootstrap
# directive. `/clear` does not kill the receive path either (same survival
# semantics as a compaction) — `comm-session-start.sh --context` decides
# whether to say so or to tell the session to actually rebootstrap, so this
# hook never repeats that logic. Self-gates to comm sessions (the FRONTEND is
# exempt from the registry test: it doesn't share the backend's registry, and
# a cold FE that hasn't joined yet is exactly the session this most needs to
# reach) so a plain human session gets nothing. The WHOLE path here is
# read-only ($SOT_COMM_READONLY, honored by both comm-context.sh calls and by
# comm-session-start.sh --context: no ensure_home, no legacy self-file
# self-heal — Codex review finding 16).
#
# Source of truth: comm/adapters/claude/hooks/comm-postclear-reminder.sh in
# Ship of Tools, deployed to ~/.sot-comm/bin by ShipTools.update_comm().
set -uo pipefail

payload="$(cat 2>/dev/null || true)"
if command -v jq >/dev/null 2>&1; then
    src="$(printf '%s' "$payload" | jq -r '.source // ""' 2>/dev/null || echo "")"
    [ "$src" = "clear" ] || exit 0
else
    case "$payload" in
        *'"source"'*'"clear"'*) : ;;
        *) exit 0 ;;
    esac
fi

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
REGISTRY="$COMM_HOME/registry.json"

SKILL="$("$SELF_DIR/comm-session-skill.sh" 2>/dev/null || true)"
if [ "$SKILL" != "/sot-fe-session-start" ]; then
    NAME=""
    [ -x "$SELF_DIR/comm-context.sh" ] && eval "$(SOT_COMM_READONLY=1 "$SELF_DIR/comm-context.sh" 2>/dev/null)" 2>/dev/null || true
    [ -n "${NAME:-}" ] || exit 0
    [ -f "$REGISTRY" ] || exit 0
    command -v jq >/dev/null 2>&1 || exit 0
    jq -e --arg n "$NAME" '.agents[$n]' "$REGISTRY" >/dev/null 2>&1 || exit 0
fi

"$SELF_DIR/comm-session-start.sh" --context
