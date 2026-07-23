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

# %q on an EMPTY value emits a literal '' — fine for the eval contract (both
# eval to empty), but a textual scraper (`sed -n 's/^NAME=//p'`, as session-start
# skills have used) captures the two quote chars as a NON-empty value, defeating
# ${NAME:-fallback}. Emit a bare KEY= when empty so both consumers are safe.
emit() { if [ -n "$2" ]; then printf '%s=%q\n' "$1" "$2"; else printf '%s=\n' "$1"; fi; }

emit HOST        "$HOST"
emit PANE_ID     "$PANE_ID"
emit TMUX_TARGET "$TMUX_TARGET"
emit REPO        "$REPO"
emit NAME        "$NAME"
emit SELF_FILE   "$SELF_FILE"
emit COMM_HOME   "$COMM_HOME"
emit REGISTRY    "$REGISTRY"
emit INBOX_DIR   "$INBOX_DIR"
emit READ_DIR    "$READ_DIR"
