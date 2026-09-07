#!/usr/bin/env bash
# comm-postcompact-reminder.sh — Claude Code `SessionStart` hook (matcher:
# compact): print short context after a context COMPACTION, never a
# re-bootstrap directive. Compaction does not kill the receive path (the
# watcher/listener are background tasks that outlive a summary) — only
# `comm-session-start.sh`'s own survival check decides whether to say so or to
# tell the session to actually rebootstrap, so this hook never repeats logic
# the script already owns. Prints `--context`'s output verbatim (a few short
# lines); self-gates to joined comm agents so a plain human session gets
# nothing. The WHOLE path here is read-only ($SOT_COMM_READONLY, honored by
# both comm-context.sh calls and by comm-session-start.sh --context: no
# ensure_home, no legacy self-file self-heal — Codex review finding 16).
#
# Source of truth: comm/adapters/claude/hooks/comm-postcompact-reminder.sh in
# Ship of Tools, deployed to ~/.sot-comm/bin by ShipTools.update_comm().
set -uo pipefail

payload="$(cat 2>/dev/null || true)"
src="$(printf '%s' "$payload" | jq -r '.source // ""' 2>/dev/null || echo "")"
case "$src" in
    startup|resume|clear) exit 0 ;;
esac

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
REGISTRY="$COMM_HOME/registry.json"

# Self-gate: only a comm-participating session gets a reminder. The FRONTEND
# is exempt from the registry test (shared with comm-postclear-reminder.sh —
# Codex review finding 16, this hook used to be stricter than that one for
# no reason): it doesn't share the backend's registry, and a cold FE that
# hasn't joined yet is exactly the session that most needs this.
SKILL="$("$SELF_DIR/comm-session-skill.sh" 2>/dev/null || true)"
if [ "$SKILL" != "/sot-fe-session-start" ]; then
    NAME=""
    [ -x "$SELF_DIR/comm-context.sh" ] && eval "$(SOT_COMM_READONLY=1 "$SELF_DIR/comm-context.sh" 2>/dev/null)" 2>/dev/null || true
    [ -n "${NAME:-}" ] || exit 0
    [ -f "$REGISTRY" ] || exit 0
    jq -e --arg n "$NAME" '.agents[$n]' "$REGISTRY" >/dev/null 2>&1 || exit 0
fi

"$SELF_DIR/comm-session-start.sh" --context
