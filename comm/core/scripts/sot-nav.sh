#!/usr/bin/env bash
# sot-nav.sh — drive the Ship of Tools frontend nav from inside a session.
#
# A Claude session running in a sot workspace pane can point the FE at a
# file: switch to Files mode, select the path, show its preview. It rides the
# sot-comm relay as a BROADCAST agent.message carrying a structured
# {"sot_ui":{...}} envelope. The frontend intercepts that envelope (gated on
# workspace) and drives nav locally — the envelope is NEVER rendered as chat.
#
# Why broadcast (vs a directed send): the daemon broadcasts every agent.message
# to all connected FEs anyway, and a to=="" frame FILES SILENTLY for sessions
# (it does not wake them — see comm-relay.sh / the inbox Monitor's broadcast
# demotion). So one broadcast reaches every FE without spamming peer sessions;
# each FE acts only if the envelope's workspace matches the workspace it is
# currently viewing.
#
# Awareness: requires SOT_WORKSPACE (the workspace slug), which the backend
# stamps into the tmux session env when it spawns the pane (see pty.rs
# spawn_tmux_pair). SOT_SESSION=1 marks "you are inside sot";
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

# Slug: prefer the backend-stamped SOT_WORKSPACE; else derive it from the tmux
# session name (sot-be-<slug>) — covers panes that predate the stamp or lost it
# on a re-shell, instead of forcing the caller to guess the repo name.
SLUG="${SOT_WORKSPACE:-$(tmux display-message -p '#S' 2>/dev/null | sed -n 's/^sot-be-//p')}"
[ -n "$SLUG" ] || die "SOT_WORKSPACE unset and slug not derivable from tmux session name (expected sot-be-<slug>) — not in a sot workspace session (or the home-base default, which has no workspace)"

# The FE's files: node ids are workspace-relative, so send a relative path.
# An absolute path is relativized against the workspace root; a path that is
# already relative passes through unchanged.
ROOT="${SOT_WORKSPACE_ROOT:-}"
case "$REL" in
    /*)
        [ -n "$ROOT" ] || die "absolute path given but SOT_WORKSPACE_ROOT unset"
        case "$REL" in
            "$ROOT"/*) REL="${REL#"$ROOT"/}" ;;
            "$ROOT")   REL="." ;;
            *) die "path '$REL' is not under workspace root '$ROOT'" ;;
        esac
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

# Fire-and-forget broadcast over the relay. send_frame prints "relayed -> <all>"
# on daemon ack; that means the daemon accepted the frame, not that any FE acted
# (the FE acts only if its current workspace matches "$SLUG").
"$SCRIPT_DIR/comm-relay.sh" send --all "$ENVELOPE"
echo "sot-nav: nav.preview workspace=$SLUG path=$REL (broadcast)"
