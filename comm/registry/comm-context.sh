#!/usr/bin/env bash
# comm-context.sh — this process's comm identity, eval-able: eval "$(comm-context.sh)" sets HOST HANDLE_HOST
# WORKSPACE_ID ROW_ID REPO PROJECT_ROOT NAME NAME_HELD_BY SELF_FILE COMM_HOME REGISTRY INBOX_DIR READ_DIR. NAME is empty
# when the session has not joined, or when its handle is another row's (NAME_HELD_BY names that row). `--rebind` is a
# join's. This is `sotd comm context` (rust/backend/src/comm/CLAUDE.md).
. "$(dirname "${BASH_SOURCE[0]}")/comm-lib.sh" || exit 1
sot_comm context "$@"
