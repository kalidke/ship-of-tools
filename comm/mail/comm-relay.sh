#!/usr/bin/env bash
# comm-relay.sh — the send, as comm-relay.sh always took it: `comm-relay.sh send @to "message"`, the first `@` word
# anywhere the recipient and every other word the text. A handle this box's registry does not list goes to the hub,
# which files it or offers it to its linked daemons; one it lists is filed here. The verdict is comm-send.sh's. This is
# `sotd comm relay send` (rust/backend/src/comm/CLAUDE.md).

# `bridge` is a retired verb: loops started before 0.6.6 still re-run it from their own text. Say so once, then sleep;
# a loop tied to a dead session ends by its own tether check, an untied one idles until a reboot.
if [ "${1:-}" = bridge ]; then
    echo "comm-relay: bridge retired in 0.6.6; this leftover loop now sleeps (a reboot clears it)" >&2
    exec sleep 2147483647
fi
. "$(dirname "${BASH_SOURCE[0]}")/comm-lib.sh" || exit 1
sot_comm relay "$@"
