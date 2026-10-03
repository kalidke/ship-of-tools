#!/usr/bin/env bash
# comm-despawn.sh — tear down a spawned agent: destroy its workspace row
# (the daemon ends the row's capsule leg and removes the workspace toml, so the
# FE strip row goes away), then remove its sot-comm handle. A name it cannot
# resolve to a workspace fails (exit 1) and changes nothing.
#
# Usage: comm-despawn.sh <name|slug|workspace_id> [--endpoint ssh:target[/host]|unix:PATH]
#
# The default workspace cannot be destroyed (daemon refuses).
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/comm-lib.sh"
# A second agent inside the session neither despawns nor spawns rows; the gate
# comes before any write, so a refused child touches nothing.
_why="$(sot_require_agent)" || { echo "comm-despawn.sh: $_why" >&2; exit 1; }
ensure_home

WHO=""; ENDPOINT=""
while [ $# -gt 0 ]; do
    case "$1" in
        --endpoint) ENDPOINT="$2"; shift 2 ;;
        *)          [ -z "$WHO" ] && WHO="$1"; shift ;;
    esac
done
[ -z "$WHO" ] && { echo "usage: comm-despawn.sh <name|slug|workspace_id> [--endpoint ...]" >&2; exit 1; }

resolve_endpoint() {
    sot_daemon_endpoint "${ENDPOINT:-${SOT_SPAWN_ENDPOINT:-}}"
}
# App-level auth (ADR 0010 hardening): daemon requires a token-valid hello
# first — `sot_hello_frame` (comm-lib.sh, ADR 0046 decision 1). Every
# scheme (unix, ssh, pipe) is handled by sot_oneshot_request (comm-lib.sh),
# which refuses any other, so there is no scheme list to keep here.
sot_send() {
    local frame="$1" op="$2"
    sot_oneshot_request "$frame" "$op"
}

# 1) Read WHO's registry row, if any. Nothing is removed until the workspace
#    is destroyed: a despawn that fails changes nothing. The row's workspace id
#    is what lets despawn-by-HANDLE find a worktree row whose label/slug
#    deliberately differs from the handle (display-prefix decoupling).
AGENT_WSID=""; ROW_HOST=""; HAS_ROW=false
# Unreadable is not "no row".
reg_rc=0; ROW="$(sot_registry_read "$WHO")" || reg_rc=$?
[ "$reg_rc" -le 1 ] || { echo "FAILED: the registry could not be read; nothing was despawned" >&2; exit 1; }
if [ "$reg_rc" -eq 0 ]; then
    HAS_ROW=true
    AGENT_WSID="$(printf '%s' "$ROW" | sot_jq -r '.workspace_id // ""' 2>/dev/null || true)"
    ROW_HOST="$(printf '%s' "$ROW" | sot_jq -r '.host // ""' 2>/dev/null || true)"
fi
# The host part of a self-file name, by the expression comm-context.sh uses.
if [ -n "${SOT_COMM_TEST_HOST:-}" ]; then
    LOCAL_HOST="$SOT_COMM_TEST_HOST"
else
    LOCAL_HOST="$(hostname -s 2>/dev/null || hostname)"
fi

# WHO names no workspace: refuse loudly, having changed nothing. The
# comm-leave hint is printed only when a successful workspace.list proved the
# workspace absent (LIST_OK) and the row is this host's: a failed list proves
# nothing, and another host's row may well name a live session.
LIST_OK=false
_unresolved() {  # why
    echo "FAILED: comm-despawn could not resolve '$WHO' to a workspace via $ENDPOINT: $1; nothing was despawned." >&2
    if [ "$HAS_ROW" = true ] && [ "$LIST_OK" = true ] && [ "$ROW_HOST" = "$LOCAL_HOST" ]; then
        echo "  To remove a handle that has no workspace: $COMM_HOME/bin/comm-leave.sh --name $WHO" >&2
    fi
    exit 1
}

# 2) destroy the workspace
if ! ENDPOINT="$(resolve_endpoint)"; then echo "ERROR: no sotd daemon found; set --endpoint unix:/path or ssh:target[/host]" >&2; exit 1; fi
# nc is needed only for a unix: daemon (sot_oneshot_request's unix: arm) --
# an ssh: endpoint needs nothing but ssh itself (C10).
case "$ENDPOINT" in
    unix:*) command -v nc >/dev/null 2>&1 || { echo "nc not found; cannot reach daemon to destroy workspace" >&2; exit 1; } ;;
esac

if ! LIST="$(sot_send '{"v":1,"id":1,"kind":"req","op":"workspace.list","payload":{}}' workspace.list)" \
    || ! printf '%s' "$LIST" | jq -e '.payload.workspaces' >/dev/null 2>&1; then
    _unresolved "workspace.list returned no workspace list"
fi
LIST_OK=true
# The registry row's recorded workspace id comes FIRST: a handle that equals
# ANOTHER workspace's label must still destroy its own workspace.
WSID=""
if [ -n "$AGENT_WSID" ]; then
    WSID="$(printf '%s' "$LIST" | sot_jq -r --arg w "$AGENT_WSID" \
        '[.payload.workspaces[]? | select(.workspace_id==$w) | .workspace_id][0] // empty' 2>/dev/null)"
    [ -n "$WSID" ] && echo "Resolved workspace via registry row '$AGENT_WSID' (handle @$WHO)"
fi
# No row, an empty id, or an id the daemon does not list: match WHO itself
# against a workspace slug, label or id.
if [ -z "$WSID" ]; then
    WSID="$(printf '%s' "$LIST" | sot_jq -r --arg w "$WHO" \
        '[.payload.workspaces[]? | select(.slug==$w or .label==$w or .workspace_id==$w) | .workspace_id][0] // empty' 2>/dev/null)"
fi
if [ -z "$WSID" ]; then
    if [ "$HAS_ROW" = false ]; then
        _unresolved "no registry row names it and no workspace slug, label or id matches it"
    elif [ -z "$AGENT_WSID" ]; then
        _unresolved "its registry row records no workspace_id and no workspace slug, label or id matches it"
    else
        _unresolved "its registry row names workspace '$AGENT_WSID', which the daemon does not list, and no workspace slug, label or id matches it"
    fi
fi
DESTROY="$(jq -nc --arg id "$WSID" '{v:1,id:2,kind:"req",op:"workspace.destroy",payload:{workspace_id:$id}}')"
RESP="$(sot_send "$DESTROY" workspace.destroy || true)"
if printf '%s' "$RESP" | jq -e '.payload.workspace_id' >/dev/null 2>&1; then
    echo "Destroyed workspace: $(printf '%s' "$RESP" | jq -c '.payload')"
    # The destroyed workspace's own identity slot, by its exact name (the
    # writer's sanitisation, comm-context.sh): never a substring match.
    WS_SAFE="$(printf '%s' "$WSID" | tr -c 'A-Za-z0-9._-' '_')"
    rm -f "${SELF_DIR:?}/${ROW_HOST:-$LOCAL_HOST}__${WS_SAFE:?}.txt" 2>/dev/null || true
    if [ "$HAS_ROW" = true ]; then
        with_lock registry_del "$WHO"
        echo "Removed @$WHO from sot-comm registry"
    fi
    echo "In the FE: refresh the session list (enter Sessions mode) to drop the row."
else
    echo "ERROR: workspace.destroy failed: $(printf '%s' "$RESP" | jq -c '.payload' 2>/dev/null || printf '%s' "$RESP")" >&2
    exit 1
fi
