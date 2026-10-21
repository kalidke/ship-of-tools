#!/usr/bin/env bash
# comm-session-start.sh — the receive bootstrap: resolve the identity, join, print one
# `BOOTSTRAP-ARM handle=<h> identity=<ok|MISMATCH|FAIL> WAKE: <daemon|n/a>` line and the start lines, and stop. The
# daemon wakes the row: there is nothing to arm. See comm/adapters/claude/sot-session-start/SKILL.md for what the skill
# does with each line. This is `sotd comm session-start` (rust/backend/src/comm/CLAUDE.md).
. "$(dirname "${BASH_SOURCE[0]}")/comm-lib.sh" || exit 1
sot_comm session-start "$@"
