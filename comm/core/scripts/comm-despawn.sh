#!/usr/bin/env bash
# comm-despawn.sh — tear down a spawned agent: remove it from sot-comm (if
# registered) and destroy its sot workspace (kills sot-be-<slug> tmux +
# removes the workspace toml, so the FE strip row goes away).
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
# App-level auth (ADR 0010 hardening): daemon requires a token-valid hello first.
_sot_hello() {
    local tok; tok="${SOT_TOKEN:-$(cat "${XDG_CONFIG_HOME:-$HOME/.config}/sot/token" 2>/dev/null || true)}"
    printf '{"v":1,"id":1,"kind":"req","op":"hello","payload":{"client_id":"sot-comm","last_seen_revision":0,"protocol":1,"app_version":"comm","token":"%s"}}\n' "$tok"
}
sot_send() {
    local frame="$1" op="$2" hp
    case "$ENDPOINT" in
        tcp:*)  hp="${ENDPOINT#tcp:}"
                { _sot_hello; printf '%s\n' "$frame"; } | timeout 6 nc "${hp%:*}" "${hp##*:}" 2>/dev/null | grep -m1 "\"op\":\"$op\"" ;;
        unix:*) { _sot_hello; printf '%s\n' "$frame"; } | timeout 6 nc -U "${ENDPOINT#unix:}" 2>/dev/null | grep -m1 "\"op\":\"$op\"" ;;
        *)      return 1 ;;
    esac
}

# 1) deregister from sot-comm if WHO is a known agent name.
#    Capture the agent's workspace SLUG from its registry row FIRST (the row's
#    tmux target is `sot-be-<slug>:<win>.<pane>`), because deregistering drops the
#    row and step 2 would then have nothing to recover it from. This is what makes
#    despawn-by-HANDLE destroy the workspace even when the workspace LABEL/slug
#    deliberately differs from the comm handle (display-prefix decoupling): a
#    `.SoT-wt-<short>` worktree has handle `<repo>-wt-<short>` but slug
#    `_sot-wt-<short>`, so a direct slug/label/id==handle match finds nothing and
#    the tmux+kernel+toml would otherwise leak (the worktree-clean leak).
AGENT_SLUG=""
if jq -e --arg n "$WHO" '.agents[$n]' "$REGISTRY" >/dev/null 2>&1; then
    AGENT_TMUX="$(jq -r --arg n "$WHO" '.agents[$n].tmux // ""' "$REGISTRY" 2>/dev/null || true)"
    AGENT_SLUG="${AGENT_TMUX#sot-be-}"; AGENT_SLUG="${AGENT_SLUG%%:*}"   # sot-be-<slug>:pane -> <slug>
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
# slug/label/id (display-prefix decoupling). Match by the slug recovered from its
# registry row's tmux target above.
if [ -z "$WSID" ] && [ -n "$AGENT_SLUG" ]; then
    WSID="$(printf '%s' "$LIST" | jq -r --arg w "$AGENT_SLUG" \
        '.payload.workspaces[] | select(.slug==$w or .workspace_id==$w) | .workspace_id' 2>/dev/null | head -1)"
    [ -n "$WSID" ] && echo "Resolved workspace via registry slug '$AGENT_SLUG' (handle @$WHO ≠ workspace slug)"
fi
if [ -z "$WSID" ]; then
    echo "No workspace matching '$WHO' (slug/label/id${AGENT_SLUG:+, nor registry slug '$AGENT_SLUG'}). Nothing to destroy."
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
