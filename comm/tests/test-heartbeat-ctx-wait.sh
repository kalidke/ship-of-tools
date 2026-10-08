#!/usr/bin/env bash
# Guarded entry; Python observes exit, both EOFs and finite fixture cleanup.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-hbctx-test-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || exit 2
WORK="$(cd "$WORK" && pwd)"
export SOT_COMM_HOME="$WORK/comm"
guard_fresh_home "$WORK"
guard_refuse_live_home "$SOT_COMM_HOME"
STAGE="$(guard_stage_bin "$WORK")" || exit 2
PYTHON="$(command -v python3)" || { echo 'FATAL: Python is missing' >&2; exit 2; }
"$PYTHON" -B "$SCRIPT_DIR/test-heartbeat-ctx-wait.py" "$WORK" "$STAGE" "$@"
rc=$?
# The driver certifies cleanup before authorizing removal, including failures.
if [ -f "$WORK/cleanup-confirmed" ]; then
    rm -rf "${WORK:?}"
else
    echo 'FATAL: fixture cleanup unconfirmed; retaining scratch' >&2
    [ "$rc" -ne 0 ] || rc=2
fi
exit "$rc"
