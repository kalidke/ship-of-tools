#!/usr/bin/env bash
# comm-session-start-hook.sh — Claude Code `SessionStart` hook for the `clear` and `compact` sources: run the comm
# session start again, so a session that lost its context is told its handle and, when it exists, its handoff file.
#
# Carried by a row's leg (and a `ccb` session) in its own `--settings`, built by the daemon's launch recipe
# (rust/backend/src/agents/layer.rs). Claude Code adds this hook's stdout to the new context. The first start of a leg
# and a resume are not these sources: the leg's bootstrap prompt (`/sot-session-start`) runs the session start there.
# The session start re-declares the row's pinned handle, so running it again in a live row changes nothing.
#
# Always exits 0 and prints only the session start's stdout (its diagnostics go to stderr, dropped here), so the hook
# can never block or delay a session.
#
# Source of truth: comm/work_state/hooks/comm-session-start-hook.sh in Ship of Tools, deployed to ~/.sot-comm/bin by
# ShipTools.update_comm(). Edit it there.
set -uo pipefail
# The launcher sets SOT_COMM_HOOKS=off for a headless claude it runs under a parent's identity; every comm hook
# stands down on it, and this one would otherwise join as the parent.
[ "${SOT_COMM_HOOKS:-}" = off ] && exit 0
cat > /dev/null 2>&1 || true   # the hook's JSON envelope, unused
START="$(dirname "${BASH_SOURCE[0]}")/comm-session-start.sh"
[ -x "$START" ] || exit 0
"$START" 2> /dev/null || true
exit 0
