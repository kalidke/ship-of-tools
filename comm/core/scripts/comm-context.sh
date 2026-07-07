#!/usr/bin/env bash
# comm-context.sh — detect this session's comm identity. Output is eval-able:
#   eval "$(.../comm-context.sh)"
# Sets HOST PANE_ID TMUX_TARGET REPO NAME SELF_FILE COMM_HOME REGISTRY INBOX_DIR READ_DIR.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=comm-lib.sh
source "$SCRIPT_DIR/comm-lib.sh"
ensure_home

HOST="$(hostname -s 2>/dev/null || hostname)"

PANE_ID=""
TMUX_TARGET=""
if [ -n "${TMUX_PANE:-}" ]; then
    PANE_ID="$(tmux display-message -t "$TMUX_PANE" -p '#{pane_id}' 2>/dev/null || true)"
    TMUX_TARGET="$(tmux display-message -t "$TMUX_PANE" -p '#{session_name}:#{window_index}.#{pane_index}' 2>/dev/null || true)"
fi

REPO="$(basename "$(git rev-parse --show-toplevel 2>/dev/null || pwd)")"

PANE_SAFE="${PANE_ID//%/}"
SELF_FILE="$SELF_DIR/${HOST}__${PANE_SAFE:-nopane}.txt"
NAME=""
[ -f "$SELF_FILE" ] && NAME="$(cat "$SELF_FILE")"

printf 'HOST=%q\n'        "$HOST"
printf 'PANE_ID=%q\n'     "$PANE_ID"
printf 'TMUX_TARGET=%q\n' "$TMUX_TARGET"
printf 'REPO=%q\n'        "$REPO"
printf 'NAME=%q\n'        "$NAME"
printf 'SELF_FILE=%q\n'   "$SELF_FILE"
printf 'COMM_HOME=%q\n'   "$COMM_HOME"
printf 'REGISTRY=%q\n'    "$REGISTRY"
printf 'INBOX_DIR=%q\n'   "$INBOX_DIR"
printf 'READ_DIR=%q\n'    "$READ_DIR"
