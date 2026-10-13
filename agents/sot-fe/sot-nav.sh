#!/usr/bin/env bash
# sot-nav.sh — drive the Ship of Tools frontend nav from inside a session.
#
# A Claude session running in a sot workspace pane can point the FE at a
# file: switch to Files mode, select the path, show its preview. It is a UI
# notification, not mail: one `agent.send` with an empty `to`, carrying a
# structured {"sot_ui":{...}} envelope, which the daemon at this box's relay
# endpoint publishes to every connection as `agent.message`. No inbox is
# written and nobody is woken. The frontend intercepts the envelope (gated on
# workspace) and drives nav locally; it is NEVER rendered as chat. Each FE acts
# only if the envelope's workspace matches the workspace it is viewing.
#
# Awareness: requires SOT_WORKSPACE (the workspace slug), which the backend
# stamps into the capsule leg's env when it spawns the row. SOT_SESSION=1
# marks "you are inside sot";
# SOT_WORKSPACE_ROOT is the project root (used to relativize an absolute
# path). The home-base default session has no workspace slug, so nav.preview is
# not available from it.
#
# Usage:
#   sot-nav.sh preview <path>   # path workspace-relative, or absolute under
#                                  # the workspace root (auto-relativized)
#
# Contract (locked with the FE, win-fe, 2026-06-17):
#   {"sot_ui":{"v":1,"cmd":"nav.preview","workspace":"<slug>",
#                 "mode":"files","path":"<ws-relative>"}}
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

die() { echo "sot-nav: $*" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || die "jq is required"

SUB="${1:-}"; [ $# -gt 0 ] && shift || true
case "$SUB" in
    preview)
        REL="${1:-}"
        [ -n "$REL" ] || die "usage: sot-nav.sh preview <path>"
        ;;
    ""|-h|--help|help)
        echo "usage: sot-nav.sh preview <path>   # open Files nav + preview <path>" >&2
        [ "$SUB" = preview ] || exit 0
        ;;
    *)
        die "unknown command '$SUB' (only 'preview' supported); see --help"
        ;;
esac

# Slug: the backend-stamped SOT_WORKSPACE.
SLUG="${SOT_WORKSPACE:-}"
[ -n "$SLUG" ] || die "SOT_WORKSPACE unset — not in a sot workspace session (or the home-base default, which has no workspace)"

# The FE's files: node ids are workspace-relative, so send a relative path.
# An absolute path is relativized against the workspace root, the two compared
# in one spelling (sot_relative_to: on Windows the daemon spells the root C:\...
# and the shell a path /c/...); a path that is already relative passes through.
source "$SCRIPT_DIR/comm-lib.sh"
ROOT="${SOT_WORKSPACE_ROOT:-}"
case "$REL" in
    /*|[A-Za-z]:[\\/]*)
        [ -n "$ROOT" ] || die "absolute path given but SOT_WORKSPACE_ROOT unset"
        _rel="$(sot_relative_to "$ROOT" "$REL")" || die "path '$REL' is not under workspace root '$ROOT'"
        REL="$_rel"
        ;;
esac

ENVELOPE="$(jq -nc --arg ws "$SLUG" --arg p "$REL" \
    '{sot_ui:{v:1,cmd:"nav.preview",workspace:$ws,mode:"files",path:$p}}')"

# SOT_NAV_DRY_RUN: print the envelope and skip the broadcast — for tests and
# for a session to preview what it would send without driving any FE.
if [ -n "${SOT_NAV_DRY_RUN:-}" ]; then
    printf '%s\n' "$ENVELOPE"
    exit 0
fi

# One request on a fresh daemon connection (comm-lib's sot_oneshot_request). Its `ok` means the daemon published the
# notification, not that any FE acted (the FE acts only if its current workspace matches "$SLUG"). Identity is the
# one a send needs: a process that may not act as this row's handle drives no FE either.
eval "$("$SCRIPT_DIR/comm-context.sh")"
why="$(sot_require_routable_identity)" || die "$why"
ENDPOINT="$(sot_relay_endpoint || true)"
[ -n "$ENDPOINT" ] || die "no sotd daemon found; \`sotd topology relay-endpoint\` gave no endpoint for this box"
FRAME="$(printf '%s' "$ENVELOPE" | jq -Rsc --arg f "$NAME" '{v:1,id:1,kind:"req",op:"agent.send",payload:{from:$f,to:"",text:.}}')"
RESP="$(sot_oneshot_request "$FRAME" agent.send)" || RESP=""
if ! printf '%s' "$RESP" | jq -e '.payload.ok == true' >/dev/null 2>&1; then
    why="$(printf '%s' "$RESP" | jq -r '.payload.error // empty' 2>/dev/null)" || why=""
    die "the daemon at $ENDPOINT did not publish the notification${why:+: $why}"
fi
echo "sot-nav: nav.preview workspace=$SLUG path=$REL (broadcast)"
