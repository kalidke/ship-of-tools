#!/usr/bin/env bash
# comm-postcompact-reminder.sh — Claude Code `SessionStart` hook (matcher:
# compact): after a context COMPACTION, re-surface to the model that its
# sot-comm receive path survives compaction and is still armed, so it does NOT
# arm a redundant Monitor / watch loop.
#
# Why (2026-07-19, Keith): a harness Monitor + the durable listener are
# BACKGROUND tasks — they survive a context summary (only a full `--continue`
# RESTART drops them). But after compaction they are no longer described in the
# model's visible context, so a session can wrongly conclude "my Monitor died"
# and arm a second one (double-arm → duplicate wakes) or set up a blocking
# watch. hs-tirf did exactly this: it DOUBTED the Monitor survived because it
# couldn't SEE it. SKILL.md now states the fact; this hook re-states it at the
# exact moment the context was summarized — closing the KNOWING gap
# deterministically instead of relying on the model recalling the skill text.
#
# It only REMINDS — it does NOT re-run the bootstrap (that would double-arm the
# Monitor, which survives compaction and needs no re-arming). Re-arming is only
# needed after a `--continue` restart, which is SessionStart source=resume (the
# `ccb`/`ccbe` launcher runs the session-start skill there) — NOT compact.
#
# Output: plain stdout is captured as SessionStart context (docs: "Any text your
# hook script prints to stdout is added as context for Claude"). Self-gates to
# joined comm agents so a plain human session gets nothing. Fires only on
# source=compact — enforced by the settings.json matcher AND, defensively, by
# an internal guard (so a no-matcher mis-wire still can't fire on startup/
# resume, where re-arming genuinely IS needed).
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

# Self-gate: only a joined comm agent (a session with a registry row) should get
# the reminder. NAME comes from comm-context (the pane-keyed self file); empty /
# no row → this isn't a comm session → stay silent.
COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
REGISTRY="$COMM_HOME/registry.json"
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NAME=""
[ -x "$SELF_DIR/comm-context.sh" ] && eval "$("$SELF_DIR/comm-context.sh" 2>/dev/null)" 2>/dev/null || true
[ -n "${NAME:-}" ] || exit 0
[ -f "$REGISTRY" ] || exit 0
jq -e --arg n "$NAME" '.agents[$n]' "$REGISTRY" >/dev/null 2>&1 || exit 0

cat <<'EOF'
[sot-comm] Your context was just COMPACTED (summarized). Your sot-comm receive path — the durable listener AND the inbox Monitor — are BACKGROUND tasks that SURVIVE compaction and are STILL ARMED, even though they are no longer described in the context you can now see. Not seeing them does NOT mean they died.

Do NOT arm a new Monitor, start a `tail -F`, or set up a blocking watch "to be safe" — that double-arms (duplicate wakes, wasted turns). A peer's silence is normal think-time (a substantive reply takes minutes), not a dead path. Only a full session RESTART (`claude --continue`) drops the receive path, and that path re-runs the session-start skill for you.

If you genuinely need to confirm receipt, prove it cheaply with `~/.sot-comm/bin/comm-listen.sh --selftest` — it does NOT arm anything.
EOF
