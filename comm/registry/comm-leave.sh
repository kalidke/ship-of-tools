#!/usr/bin/env bash
# comm-leave.sh — give up this session's handle, or (--name <handle>) remove another handle's registry entry only, as
# an orphan a dead spawn left; a handle a row holds is refused. This is `sotd comm leave`
# (rust/backend/src/comm/CLAUDE.md).
. "$(dirname "${BASH_SOURCE[0]}")/comm-lib.sh" || exit 1
sot_comm leave "$@"
