#!/usr/bin/env bash
# comm-postclear-reminder.sh — Claude Code `SessionStart` hook (matcher: clear):
# after a `/clear`, tell the session to re-run THE session-start skill that fits
# this machine, so its sot-comm operating context is restored.
#
# Why (2026-07-25, maintainer): `/clear` is strictly more destructive to context than
# a compaction — a compaction leaves a summary, `/clear` leaves NOTHING. The
# session wakes up not knowing its handle, the send/poll/status verbs, or the
# work-state rules, while its listener and inbox Monitor are still running and
# still delivering. That combination is the dangerous one: a session that is
# still being messaged but no longer knows how to answer. The pre-existing
# post-COMPACT hook explicitly `exit 0`d on `source=clear`, so this case fired
# nothing at all.
#
# Receive-path survival (measured, not assumed): `/clear` resets the
# conversation but does NOT kill the session process, so the Monitor's
# `comm-watch.sh` child survives — verified 2026-07-25 on a cleared backend
# session (watcher still parented to the live claude pid, and a post-clear
# `comm-listen.sh --selftest` woke the cleared context). So `/clear` has the same
# survival semantics as a compaction, and the skill's Step 0 pgrep guard is the
# correct arbiter for both: it finds the live watcher and STOPS before the
# bootstrap, restoring the instructions by being re-read WITHOUT re-arming the
# Monitor (double wakes), re-`comm-poll`ing (replaying handled messages), or
# re-`comm-join`ing (whose row-replace would wipe the live work-state).
#
# Names exactly ONE skill, resolved by `comm-session-skill.sh` rather than listed
# for the model to choose from — a wrong pick sends a backend session through the
# frontend bootstrap (win-fe handle, tcp tunnel, fe-inbox Monitor), which fails
# quietly on a box that has none of those. See that script for the detection.
#
# Output: plain stdout is captured as SessionStart context (docs: "Any text your
# hook script prints to stdout is added as context for Claude"). Self-gates so a
# plain human session gets nothing.
#
# Source of truth: comm/adapters/claude/hooks/comm-postclear-reminder.sh in
# Ship of Tools, deployed to ~/.sot-comm/bin by ShipTools.update_comm().
set -uo pipefail

# SessionStart delivers a JSON payload on stdin carrying `.source`. Fire ONLY on
# `clear` — deliberately stricter than the post-compact hook (which also proceeds
# on an empty/unknown source). Both hooks accepting "unknown" would make a
# no-matcher mis-wire print TWO conflicting directives on the same event. When
# `jq` is missing, fall back to a substring test on the raw payload rather than
# guessing.
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

COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
REGISTRY="$COMM_HOME/registry.json"
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

skill="$("$SELF_DIR/comm-session-skill.sh" 2>/dev/null || true)"
[ -n "$skill" ] || skill="/sot-session-start"

# Self-gate: only a comm-participating session should be told to re-bootstrap.
# The FRONTEND is exempt from the registry test on purpose — the FE machine does
# not share the backend's registry, and a cold FE that has not joined yet is
# precisely the session that most needs this directive. Everything else must have
# a registry row (NAME from the pane-keyed self file); no row -> not a comm
# session -> stay silent.
NAME=""
if [ "$skill" != "/sot-fe-session-start" ]; then
    [ -x "$SELF_DIR/comm-context.sh" ] && eval "$("$SELF_DIR/comm-context.sh" 2>/dev/null)" 2>/dev/null || true
    [ -n "${NAME:-}" ] || exit 0
    [ -f "$REGISTRY" ] || exit 0
    command -v jq >/dev/null 2>&1 || exit 0
    jq -e --arg n "$NAME" '.agents[$n]' "$REGISTRY" >/dev/null 2>&1 || exit 0
fi

who=""
[ -n "${NAME:-}" ] && who=" You are @${NAME}."

cat <<EOF
[sot-comm] ACTION REQUIRED — your context was just CLEARED (/clear). Unlike a compaction there is no summary: you retain NOTHING of your sot-comm operating context — handle, the send/poll/status verbs, the work-state rules — while your listener and inbox Monitor are still running and still delivering messages to you.${who}

Re-run your session-start skill now, BEFORE other work:
  ${skill}

That is the correct skill for THIS machine (resolved by comm-session-skill.sh from your handle, platform, tmux context, and repo identity — do not substitute another one; the frontend and backend bootstraps are not interchangeable).

Safe to re-run: /clear does not kill the session process, so your Monitor and listener SURVIVED it. The skill's Step 0 detects that and STOPS — it does NOT re-arm (double wakes), re-poll (replaying handled messages), or re-join (whose row-replace would wipe your work-state). It simply restores your operating context by being re-read. Run it once, then continue.
EOF
