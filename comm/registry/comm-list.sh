#!/usr/bin/env bash
# comm-list.sh — every handle this box's registry lists, live or stale, with its work-state line, then the frontend
# boxes this daemon talks to. This is `sotd comm list` (rust/backend/src/comm/CLAUDE.md).
. "$(dirname "${BASH_SOURCE[0]}")/comm-lib.sh" || exit 1
sot_comm list "$@"
