#!/usr/bin/env bash
# comm-postcompact-reminder.sh — Claude Code `SessionStart` hook (matcher:
# compact): after a context COMPACTION, tell the session to RE-RUN its full
# session-start skill so its complete sot-comm operating context is restored.
#
# Why (2026-07-19, Keith): compaction summarizes the conversation and can strip
# the operating INSTRUCTIONS themselves — your handle, the send/poll/status
# verbs, the work-state rules — not just the "trust your Monitor" note. So a bare
# reminder isn't enough: even a session that trusts its (surviving) Monitor may
# no longer know HOW to operate comm. Re-running the session-start skill restores
# all of it. (Earlier this hook only printed a trust reminder; that was
# insufficient for exactly this reason.)
#
# Safe to re-run on every compaction: the session-start skill opens with a
# "Step 0" that detects survival — it `pgrep`s (end-anchored) for the live
# watcher that outlives a summary and, when found, STOPS before the bootstrap.
# So a compaction re-run re-reads the doc (restoring the operating instructions)
# but does NOT re-arm the Monitor, re-`comm-poll` (which would replay
# already-handled messages), or re-`comm-join` (whose row-replace would wipe the
# live work-state). The full bootstrap runs only on a real `--continue` restart,
# where Step 0 finds no watcher.
#
# Output: plain stdout is captured as SessionStart context (docs: "Any text your
# hook script prints to stdout is added as context for Claude"). Self-gates to
# joined comm agents so a plain human session gets nothing. Fires only on
# source=compact — enforced by the settings.json matcher AND, defensively, by an
# internal guard (so a no-matcher mis-wire can't fire on startup/resume, where
# the launcher's own session-start run already covers it).
#
# Source of truth: comm/adapters/claude/hooks/comm-postcompact-reminder.sh in
# Ship of Tools, deployed to ~/.sot-comm/bin by ShipTools.update_comm().
set -uo pipefail

# SessionStart delivers a JSON payload on stdin carrying `.source`. Skip the
# NON-compaction sources explicitly; proceed on `compact` OR an empty/unknown
# source (trusting the compact-scoped matcher). This makes the hook correct
# whether or not the settings.json entry carries `"matcher": "compact"`.
payload="$(cat 2>/dev/null || true)"
src="$(printf '%s' "$payload" | jq -r '.source // ""' 2>/dev/null || echo "")"
case "$src" in
    startup|resume|clear) exit 0 ;;
esac

# Self-gate: only a joined comm agent (a session with a registry row) should be
# told to re-bootstrap. NAME comes from comm-context (the pane-keyed self file);
# empty / no row → this isn't a comm session → stay silent.
COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
REGISTRY="$COMM_HOME/registry.json"
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NAME=""
[ -x "$SELF_DIR/comm-context.sh" ] && eval "$("$SELF_DIR/comm-context.sh" 2>/dev/null)" 2>/dev/null || true
[ -n "${NAME:-}" ] || exit 0
[ -f "$REGISTRY" ] || exit 0
jq -e --arg n "$NAME" '.agents[$n]' "$REGISTRY" >/dev/null 2>&1 || exit 0

cat <<'EOF'
[sot-comm] ACTION REQUIRED — your context was just COMPACTED (summarized). Compaction can strip the sot-comm operating instructions themselves (your handle, the send/poll/status verbs, the work-state rules), not only the "trust your Monitor" note — so a bare reminder is not enough.

Re-run your session-start skill now, BEFORE other work, to restore your full comm operating context:
  • /sot-session-start      (generic — any backend/session)
  • /sot-be-session-start   (Ship of Tools backend)
  • /sot-fe-session-start   (Ship of Tools frontend)

Safe to re-run: the skill's Step 0 detects that you SURVIVED this compaction (your Monitor + listener are background tasks that outlive a summary) and STOPS — it does NOT re-arm, re-poll, or re-join, so it can't double-arm your Monitor, replay already-handled messages, or wipe your work-state. On a compaction it simply restores your operating context by being re-read. (Had this been a real --continue restart, Step 0 would find the dead Monitor and run the full bootstrap instead.) Run it once, then continue.
EOF
