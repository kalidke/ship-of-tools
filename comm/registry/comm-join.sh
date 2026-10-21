#!/usr/bin/env bash
# comm-join.sh — take this session's handle: [--name NAME] [--expertise "a, b"] [--repin]; --help says more. With no
# arguments the handle is the one the session already holds, else derived from the project folder and the box name.
# Prints "Joined sot-comm as @<handle>". This is `sotd comm join` (rust/backend/src/comm/CLAUDE.md).
. "$(dirname "${BASH_SOURCE[0]}")/comm-lib.sh" || exit 1
sot_comm join "$@"
