#!/usr/bin/env bash
# comm-lib.sh — shared helpers for sot-comm. SOURCED, not executed.
# Implements the v1 protocol (see comm/PROTOCOL.md). Runtime data lives under
# $SOT_COMM_HOME (default ~/.sot-comm).

source "$(dirname "${BASH_SOURCE[0]}")/comm-lib-base.sh" || return 1
source "$(dirname "${BASH_SOURCE[0]}")/comm-lib-client.sh" || return 1
source "$(dirname "${BASH_SOURCE[0]}")/comm-lib-registry-lock.sh" || return 1
source "$(dirname "${BASH_SOURCE[0]}")/comm-lib-registry.sh" || return 1
source "$(dirname "${BASH_SOURCE[0]}")/comm-lib-inbox.sh" || return 1
source "$(dirname "${BASH_SOURCE[0]}")/comm-lib-identity.sh" || return 1
source "$(dirname "${BASH_SOURCE[0]}")/comm-lib-agent-layers.sh" || return 1
