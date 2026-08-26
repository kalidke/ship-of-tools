#!/usr/bin/env bash
# comm-bootstrap.sh — first contact. Paste a join+reply nudge into another
# session's tmux pane so it enrolls itself in sot-comm. Use when the target
# has the skill installed but hasn't joined (so it isn't addressable by @name).
#
# Usage: comm-bootstrap.sh <tmux-target> [suggested-name]
#   <tmux-target>   e.g. sot-be-lab-guide:1.1  or  %13
#   suggested-name  optional handle to propose for the target
#
# Discover targets with (note -S: sot sessions live on the PRIVATE per-user
# server — ADR 0038 keeper — not tmux's default socket):
#   source ~/.sot-comm/bin/comm-lib.sh   # for sot_tmux_socket
#   tmux -S "$(sot_tmux_socket)" \
#     list-panes -a -F '#{session_name}:#{window_index}.#{pane_index}  #{pane_id}'
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/comm-lib.sh"
eval "$("$SCRIPT_DIR/comm-context.sh")"
ensure_home

TGT="${1:-}"; SUGG="${2:-}"
[ -z "$TGT" ] && { echo "usage: comm-bootstrap.sh <tmux-target> [suggested-name]" >&2; exit 1; }
[ -z "$NAME" ] && NAME="unknown-$HOST"

# Validate the target pane exists — on the sot server (ADR 0038 keeper
# socket), same resolution comm-send uses for the paste itself. A bare
# `tmux` here checked the DEFAULT server, where daemon-created sessions
# never live (pre-existing gap, flagged in the PR #109 review round).
SOT_SOCK="$(sot_tmux_socket)" || { echo "ERROR: cannot resolve the sot tmux socket" >&2; exit 1; }
if ! tmux -S "$SOT_SOCK" list-panes -a -F '#{session_name}:#{window_index}.#{pane_index} #{pane_id}' 2>/dev/null \
     | grep -qE "(^| )${TGT}( |$)"; then
    echo "ERROR: tmux target '$TGT' not found among live panes on $SOT_SOCK" >&2
    exit 1
fi

BIN="$COMM_HOME/bin"
NUDGE="[sot-comm bootstrap from @$NAME] You have the sot-comm skill but are not joined. Please join and reply: run  $BIN/comm-join.sh${SUGG:+ --name $SUGG}  then  $BIN/comm-send.sh @$NAME \"joined as <yourname>\" . (Or just use the /sot-comm skill.) After this, we talk over sot-comm, not raw tmux."

"$SCRIPT_DIR/comm-send.sh" --force-target "$TGT" "$NUDGE"
echo "Bootstrap nudge sent to $TGT. Waiting for it to join — check with comm-list.sh / comm-poll.sh."
