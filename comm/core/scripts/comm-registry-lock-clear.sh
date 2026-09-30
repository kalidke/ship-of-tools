#!/usr/bin/env bash
# comm-registry-lock-clear.sh — a person's recovery for a registry lock whose
# holder is dead but cannot be proved dead here: it ran on another machine
# where no comm command will run again, or on a platform with no proof.
#
# It takes the reclaim path exactly as a waiter does (comm-lib.sh's
# _sot_lock_step): the marker `.registry.lock.reclaim.<D>` for the holder D
# the lock names, the 1 s settle, the fresh re-read, and it removes the lock
# only if the lock still names D. Running it is the person's word that D is
# dead, and that word replaces ONLY D's liveness proof: a holder this box
# proves alive is never cleared, a reclaimer holding D's marker that this box
# cannot prove dead is never passed, and nothing else is skipped. Never a
# bare rm: a reclaimer still pending on another box would then remove the
# next holder's lock. The one exception is a blocking record with no proof
# fields (a clear killed on macOS or git-bash leaves one in its marker): no
# box can ever prove it dead, so the refusal says to remove the lock by hand.
#
# Usage: comm-registry-lock-clear.sh
# Exit: 0 cleared, or already free; 1 not cleared, with the reason on stderr.
set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/comm-lib.sh"

_sot_lock_self_id
if _sot_lock_step --forced; then
    echo "cleared registry lock $_SOT_REG_LOCK, held by ${_SOT_LOCK_HOLDER%%:*} pid $(_sot_lock_field "$_SOT_LOCK_HOLDER" 5)"
    exit 0
fi
if [ ! -e "$_SOT_REG_LOCK" ]; then
    echo "registry lock $_SOT_REG_LOCK is free"
    exit 0
fi
if [[ "${_SOT_LOCK_WHO:-$_SOT_LOCK_HOLDER}" =~ ^[^:]*:-:-:-:[0-9]+:-$ ]]; then
    echo "NOT cleared: registry lock $_SOT_REG_LOCK: $_SOT_LOCK_WHY. That record has no proof fields, so no box can prove it dead: if it is, remove the lock by hand." >&2
    exit 1
fi
echo "NOT cleared: registry lock $_SOT_REG_LOCK: $_SOT_LOCK_WHY" >&2
exit 1
