#!/usr/bin/env bash
# comm-postcompact-reminder.sh — Claude Code `SessionStart` hook (matcher:
# compact): print short context after a context COMPACTION, never a
# re-bootstrap directive. Compaction does not kill the receive path (the
# watcher is a background task that outlives a summary) — only
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

# Self-gate: only a comm-participating session gets a reminder (a registry
# membership check, shared with comm-postclear-reminder.sh).
NAME=""
[ -x "$SELF_DIR/comm-context.sh" ] && eval "$(SOT_COMM_READONLY=1 "$SELF_DIR/comm-context.sh" 2>/dev/null)" 2>/dev/null || true
[ -n "${NAME:-}" ] || exit 0
# comm-lib.sh's sot_registry_read, sourced in a subshell: 0 a row, 1 no row,
# 2 unreadable, a missing file included (a lib that cannot be sourced is 2
# too, never "no row"). Only no row ends here: an unreadable registry still
# reminds — a spurious reminder costs a line, a missed one the identity. No
# `test -f` first: a stat can fail during another host's rename.
_reg_rc=0; ( . "$SELF_DIR/comm-lib.sh" >/dev/null 2>&1 || exit 2; sot_registry_read "$NAME" >/dev/null ) || _reg_rc=$?
[ "$_reg_rc" -eq 1 ] && exit 0

"$SELF_DIR/comm-session-start.sh" --context
