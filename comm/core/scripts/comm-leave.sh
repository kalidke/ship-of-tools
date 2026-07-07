#!/usr/bin/env bash
# comm-leave.sh — remove this session from the network, or (--name) remove a
# specific handle's registry row (e.g. an orphan left by a dead spawned agent).
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/comm-lib.sh"
eval "$("$SCRIPT_DIR/comm-context.sh")"
ensure_home

# Arg handling is strict: this script once took no args and silently ignored
# them, so `comm-leave.sh --name X` removed the CALLER's row instead of X's
# (regression found 2026-06-12). Unknown args are now fatal.
WHO=""
while [ $# -gt 0 ]; do
    case "$1" in
        --name)   WHO="${2:?--name needs a handle}"; shift 2 ;;
        --name=*) WHO="${1#--name=}"; [ -n "$WHO" ] || { echo "--name= needs a handle" >&2; exit 1; }; shift ;;
        *) echo "usage: comm-leave.sh [--name <handle>]" >&2; exit 1 ;;
    esac
done

if [ -n "$WHO" ] && [ "$WHO" != "$NAME" ]; then
    # Removing someone else's row touches the registry only — their SELF_FILE
    # belongs to their session, and comm-despawn.sh is the full teardown tool.
    jq -e --arg n "$WHO" '.agents[$n]' "$REGISTRY" >/dev/null 2>&1 \
        || { echo "@$WHO not in registry — nothing to do."; exit 0; }
    with_lock registry_del "$WHO"
    echo "Removed @$WHO from the registry (row only; comm-despawn.sh does full teardown)."
    exit 0
fi

[ -z "$NAME" ] && { echo "Not joined — nothing to do."; exit 0; }
with_lock registry_del "$NAME"
rm -f "$SELF_FILE"
echo "Left sot-comm (@$NAME removed)."
