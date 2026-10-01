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
# first — `sot_hello_frame` (comm-lib.sh, ADR 0046 decision 1). Both
# schemes delegate to sot_oneshot_request (comm-lib.sh), which already
# carries a tested arm for each.
sot_send() {
    local frame="$1" op="$2"
    case "$ENDPOINT" in
        ssh:*|unix:*) sot_oneshot_request "$frame" "$op" ;;
        *)            return 1 ;;
    esac
}

# 1) Read WHO's registry row, if any. Nothing is removed until the workspace
#    is destroyed: a despawn that fails changes nothing. The row's workspace id
#    is what lets despawn-by-HANDLE find a worktree row whose label/slug
#    deliberately differs from the handle (display-prefix decoupling).
AGENT_WSID=""; HAS_ROW=false
# Unreadable is not "no row".
reg_rc=0; ROW="$(sot_registry_read "$WHO")" || reg_rc=$?
[ "$reg_rc" -le 1 ] || { echo "FAILED: the registry could not be read; nothing was despawned" >&2; exit 1; }
if [ "$reg_rc" -eq 0 ]; then
    HAS_ROW=true
    AGENT_WSID="$(printf '%s' "$ROW" | sot_jq -r '.workspace_id // ""' 2>/dev/null || true)"
fi

# Belt-and-braces for the owned lifetimes (messaging ruling §3): the watcher
# now ends with its owner by itself, so this should find nothing — but a row
# being torn down is exactly where a survivor would look like a live receiver
# for a handle that no longer exists, so the marker is read and cleared here
# too.
_reap_markers() {
    local who="$1" pid
    # sot_watcher_pid_for, never the bare pid on the marker's first line: these
    # markers sit on a shared home and survive reboots, so a REUSED pid would
    # make this kill an unrelated process of the same user. It must still BE a
    # watcher for this handle; anything else means the marker is stale and only
    # the file is removed.
    if pid="$(sot_watcher_pid_for "$who")"; then
        kill "$pid" 2>/dev/null && echo "Stopped watcher pid $pid for @$who"
    fi
    rm -f "${COMM_HOME:?}/state/$who.watch" 2>/dev/null || true
}

# WHO names no workspace: refuse loudly, having changed nothing.
_unresolved() {  # why
    echo "FAILED: comm-despawn could not resolve '$WHO' to a workspace via $ENDPOINT: $1; nothing was despawned." >&2
    if [ "$HAS_ROW" = true ]; then
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
WSID="$(printf '%s' "$LIST" | sot_jq -r --arg w "$WHO" \
    '[.payload.workspaces[]? | select(.slug==$w or .label==$w or .workspace_id==$w) | .workspace_id][0] // empty' 2>/dev/null)"
# Fallback: WHO was an agent handle that doesn't itself match a workspace
# slug/label/id (display-prefix decoupling). Match by the workspace id its
# registry row recorded at join.
if [ -z "$WSID" ] && [ -n "$AGENT_WSID" ]; then
    WSID="$(printf '%s' "$LIST" | sot_jq -r --arg w "$AGENT_WSID" \
        '[.payload.workspaces[]? | select(.workspace_id==$w) | .workspace_id][0] // empty' 2>/dev/null)"
    [ -n "$WSID" ] && echo "Resolved workspace via registry row '$AGENT_WSID' (handle @$WHO ≠ workspace slug)"
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
    if [ "$HAS_ROW" = true ]; then
        with_lock registry_del "$WHO"
        rm -f "${SELF_DIR:?}/"*"$WHO"* 2>/dev/null || true
        echo "Removed @$WHO from sot-comm registry"
    fi
    _reap_markers "$WHO"
    echo "In the FE: refresh the session list (enter Sessions mode) to drop the row."
else
    echo "ERROR: workspace.destroy failed: $(printf '%s' "$RESP" | jq -c '.payload' 2>/dev/null || printf '%s' "$RESP")" >&2
    exit 1
fi
