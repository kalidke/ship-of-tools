#!/usr/bin/env bash
# comm-registry-lock-clear.sh — a person's recovery for a registry lock whose
# holder is dead but cannot be proved dead here: it ran on another machine
# where no comm command will run again, or on a platform with no proof.
#
# It takes the reclaim path exactly as a waiter does (comm-lib.sh's
# _sot_lock_step): the marker `.registry.lock.reclaim.<D>` for the holder D
# the lock names, the 1 s settle, the fresh re-read, and it removes the lock
# only if the lock still names D. Running it is the person's word that D is
# dead, and that word replaces ONLY the liveness proof: a holder this box
# proves alive is never cleared, and nothing else is skipped. Never a bare rm:
# a reclaimer still pending on another box would then remove the next
# holder's lock.
#
# Usage: comm-registry-lock-clear.sh
# Exit: 0 cleared, or already free; 1 not cleared, with the reason on stderr.
set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/comm-lib.sh"

_sot_lock_self_id
if [ ! -e "$_SOT_REG_LOCK" ]; then
    echo "registry lock $_SOT_REG_LOCK is free"
    exit 0
fi
if _sot_lock_step --forced; then
    if [ -n "$_SOT_LOCK_HOLDER" ]; then
        echo "cleared registry lock $_SOT_REG_LOCK, held by ${_SOT_LOCK_HOLDER%%:*} pid $(_sot_lock_field "$_SOT_LOCK_HOLDER" 5)"
    else
        echo "registry lock $_SOT_REG_LOCK is free"
    fi
    exit 0
fi
echo "NOT cleared: registry lock $_SOT_REG_LOCK: $_SOT_LOCK_WHY" >&2
exit 1
