#!/usr/bin/env bash
# comm-lib.sh — the shell library every installed comm script sources. SOURCED, not executed.
# Messaging itself is `sotd comm` (rust/backend/src/comm/CLAUDE.md); this library holds the shell helpers the scripts
# share, the daemon client the agents' CLIs use, and the door through which every messaging function runs sotd.

source "$(dirname "${BASH_SOURCE[0]}")/comm-lib-base.sh" || return 1
source "$(dirname "${BASH_SOURCE[0]}")/comm-lib-client.sh" || return 1
source "$(dirname "${BASH_SOURCE[0]}")/comm-lib-sotd.sh" || return 1
