#!/usr/bin/env bash
# comm-poll.sh — show this session's mail past its read cursor, then move the cursor past it: the one reader of
# message bodies. Each line is `[ts] [from:repo] message`; `No new messages.` when there are none; exit 75 when the
# inbox is being written (nothing was read: run it again). This is `sotd comm poll` (rust/backend/src/comm/CLAUDE.md).
. "$(dirname "${BASH_SOURCE[0]}")/comm-lib.sh" || exit 1
sot_comm poll "$@"
