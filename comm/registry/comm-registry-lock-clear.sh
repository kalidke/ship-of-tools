#!/usr/bin/env bash
# comm-registry-lock-clear.sh — clear, at once, a registry lock whose holder
# this box proves dead.
#
# It takes the reclaim path exactly as a waiter does (comm-lib.sh's
# _sot_lock_step), with no deadline: the proof that the holder D the lock
# names is dead, the marker `.registry.lock.reclaim.<D>`, the 1 s settle, the
# fresh re-read, and it removes the lock only if the lock still names D. A
# holder this box cannot prove dead (on another machine, or where this box has
# no proof) may be alive, and clearing it would give the registry two writers:
# it is refused with the recovery a waiter's FAILED line names (_sot_lock_fix):
# a comm command on its host, or by hand where its host will not run again or
# its record has no proof fields (git-bash's, a box whose comm home's mount
# proves no fresh read, or an older clear's marker). Never a bare rm: a
# reclaimer still pending on another box would then remove the next holder's
# lock. A lock no reclaim can clear (a record read whole that does not parse,
# an older version's directory, or a marker naming a record its chain already
# holds) is refused as one to remove by hand.
#
# Usage: comm-registry-lock-clear.sh
# Exit: 0 cleared, or already free; 1 not cleared, with the reason on stderr.
set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/comm-lib.sh"

_sot_lock_self_id
if _sot_lock_step; then
    echo "cleared registry lock $_SOT_REG_LOCK, held by ${_SOT_LOCK_HOLDER%%:*} pid $(_sot_lock_field "$_SOT_LOCK_HOLDER" 5)"
    exit 0
fi
if [ ! -e "$_SOT_REG_LOCK" ]; then
    echo "registry lock $_SOT_REG_LOCK is free"
    exit 0
fi
if [ -n "$_SOT_LOCK_BYHAND" ]; then
    echo "NOT cleared: registry lock $_SOT_REG_LOCK: $_SOT_LOCK_WHY. No reclaim can clear it: if its holder is dead, remove the lock by hand." >&2
    exit 1
fi
held="" fix=""
[ -z "$_SOT_LOCK_HOLDER" ] || held=", held by ${_SOT_LOCK_HOLDER%%:*} pid $(_sot_lock_field "$_SOT_LOCK_HOLDER" 5)"
[ "${_SOT_LOCK_VERDICT:-}" != UNPROVABLE ] || fix=" Only a box that proves it dead clears it. $(_sot_lock_fix "$_SOT_LOCK_WHO")"
echo "NOT cleared: registry lock $_SOT_REG_LOCK$held: $_SOT_LOCK_WHY.$fix" >&2
exit 1
