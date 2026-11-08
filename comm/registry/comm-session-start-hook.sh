#!/usr/bin/env bash
# comm-session-start-hook.sh — Claude Code `SessionStart` hook for the `clear` and `compact` sources: run the comm
# session start in its read-only form (`comm-session-start.sh --again`), so a session that lost its context is told its
# handle, its handoff file and any question its row kept, and nothing is joined or written: the row stays joined across
# a clear or a compaction, and a join would rewrite its registry entry and wipe its work-state.
#
# Carried by a row's leg (and a `ccb` session) in its own `--settings`, built by the daemon's launch recipe
# (rust/backend/src/agents/layer.rs). Claude Code adds this hook's stdout to the new context, so a refusal's reason goes
# to stdout too. The first start of a leg and a leg started with `--resume <id>` run the session start through the
# bootstrap prompt (`/sot-session-start`); `/resume` typed in a live row is the `resume` source, which this hook is not
# carried for.
#
# Always exits 0, so the hook never fails a session; it is bounded by the start's own waits, not instant.
#
# Source of truth: comm/registry/comm-session-start-hook.sh in Ship of Tools, deployed to ~/.sot-comm/bin by
# ShipTools.update_comm(). Edit it there.
set -uo pipefail
# The launcher sets SOT_COMM_HOOKS=off for a headless claude it runs under a parent's identity; every comm hook
# stands down on it.
[ "${SOT_COMM_HOOKS:-}" = off ] && exit 0
START="$(dirname "${BASH_SOURCE[0]}")/comm-session-start.sh"
[ -x "$START" ] || exit 0
"$START" --again 2>&1 || true
exit 0
