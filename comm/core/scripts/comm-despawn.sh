#!/usr/bin/env bash
# comm-despawn.sh — tear down a spawned agent: remove it from sot-comm (if
# registered) and destroy its workspace row (the daemon ends the row's
# capsule leg and removes the workspace toml, so the FE strip row goes away).
#
# Usage: comm-despawn.sh <name|slug|workspace_id> [--endpoint tcp:H:P|unix:PATH]
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
# first — `sot_hello_frame` (comm-lib.sh, ADR 0046 decision 1).
sot_send() {
    local frame="$1" op="$2" hp
    case "$ENDPOINT" in
        tcp:*)  hp="${ENDPOINT#tcp:}"
                { sot_hello_frame; printf '%s\n' "$frame"; } | timeout 6 nc "${hp%:*}" "${hp##*:}" 2>/dev/null | grep -m1 "\"op\":\"$op\"" ;;
        unix:*) sot_oneshot_request "$frame" "$op" ;;
        *)      return 1 ;;
    esac
}

# 1) deregister from sot-comm if WHO is a known agent name.
#    Capture the agent's workspace id from its registry row FIRST, because
#    deregistering drops the row and step 2 would then have nothing to recover
#    it from. This is what makes despawn-by-HANDLE destroy the workspace even
#    when the workspace LABEL/slug deliberately differs from the comm handle
#    (display-prefix decoupling): a worktree row has handle `<repo>-wt-<short>`
#    but its own slug, so a direct slug/label/id==handle match finds nothing and
#    the row+kernel+toml would otherwise leak (the worktree-clean leak).
AGENT_WSID=""
if jq -e --arg n "$WHO" '.agents[$n]' "$REGISTRY" >/dev/null 2>&1; then
    AGENT_WSID="$(jq -r --arg n "$WHO" '.agents[$n].workspace_id // ""' "$REGISTRY" 2>/dev/null || true)"
    with_lock registry_del "$WHO"
    rm -f "$SELF_DIR/"*"$WHO"* 2>/dev/null || true
    echo "Removed @$WHO from sot-comm registry"
fi

# 2) destroy the workspace
if ! command -v nc >/dev/null 2>&1; then echo "nc not found; cannot reach daemon to destroy workspace" >&2; exit 1; fi
if ! ENDPOINT="$(resolve_endpoint)"; then echo "ERROR: no sotd daemon found; set --endpoint unix:/path or tcp:HOST:PORT" >&2; exit 1; fi

LIST="$(sot_send '{"v":1,"id":1,"kind":"req","op":"workspace.list","payload":{}}' workspace.list || true)"
WSID="$(printf '%s' "$LIST" | jq -r --arg w "$WHO" \
    '.payload.workspaces[] | select(.slug==$w or .label==$w or .workspace_id==$w) | .workspace_id' 2>/dev/null | head -1)"
# Fallback: WHO was an agent handle that doesn't itself match a workspace
# slug/label/id (display-prefix decoupling). Match by the workspace id its
# registry row recorded at join.
if [ -z "$WSID" ] && [ -n "$AGENT_WSID" ]; then
    WSID="$(printf '%s' "$LIST" | jq -r --arg w "$AGENT_WSID" \
        '.payload.workspaces[] | select(.workspace_id==$w) | .workspace_id' 2>/dev/null | head -1)"
    [ -n "$WSID" ] && echo "Resolved workspace via registry row '$AGENT_WSID' (handle @$WHO ≠ workspace slug)"
fi
if [ -z "$WSID" ]; then
    echo "No workspace matching '$WHO' (slug/label/id${AGENT_WSID:+, nor registry row '$AGENT_WSID'}). Nothing to destroy."
    exit 0
fi
DESTROY="$(jq -nc --arg id "$WSID" '{v:1,id:2,kind:"req",op:"workspace.destroy",payload:{workspace_id:$id}}')"
RESP="$(sot_send "$DESTROY" workspace.destroy || true)"
if printf '%s' "$RESP" | jq -e '.payload.workspace_id' >/dev/null 2>&1; then
    echo "Destroyed workspace: $(printf '%s' "$RESP" | jq -c '.payload')"
    echo "In the FE: refresh the session list (enter Sessions mode) to drop the row."
else
    echo "ERROR: workspace.destroy failed: $(printf '%s' "$RESP" | jq -c '.payload' 2>/dev/null || printf '%s' "$RESP")" >&2
    exit 1
fi
