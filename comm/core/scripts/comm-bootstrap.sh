#!/usr/bin/env bash
# comm-bootstrap.sh — first contact. Type a join+reply nudge into another
# session's workspace row so it enrolls itself in sot-comm. Use when the
# target has the skill installed but hasn't joined (so it isn't addressable
# by @name).
#
# Usage: comm-bootstrap.sh <workspace slug|label|id> [suggested-name]
#   suggested-name  optional handle to propose for the target
#
# Discover targets with `sot-fe workspaces` (or comm-list.sh for joined ones).
# Delivery is the daemon's `pty.input` on the row (Enter appended) — the
# same path comm-send.sh's live leg uses; the daemon refuses a row whose
# capsule is not ready.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/comm-lib.sh"
eval "$("$SCRIPT_DIR/comm-context.sh")"
ensure_home

TGT="${1:-}"; SUGG="${2:-}"
[ -z "$TGT" ] && { echo "usage: comm-bootstrap.sh <workspace slug|label|id> [suggested-name]" >&2; exit 1; }
# Identity refusal, via the ONE shared helper (comm-lib.sh) also used by
# comm-send.sh and comm-relay.sh: the nudge below embeds "@$NAME" as the
# reply-to address it hands the target session — a target that dutifully
# replies to an unroutable handle would have no working reply path at all.
sot_require_routable_identity || exit 1

ENDPOINT="$(sot_daemon_endpoint "${SOT_SPAWN_ENDPOINT:-}")" \
    || { echo "ERROR: no sotd daemon found; set SOT_SPAWN_ENDPOINT=unix:/path or tcp:HOST:PORT" >&2; exit 1; }
LIST="$(sot_oneshot_request '{"v":1,"id":1,"kind":"req","op":"workspace.list","payload":{}}' workspace.list || true)"
WSID="$(printf '%s' "$LIST" | jq -r --arg w "$TGT" \
    '.payload.workspaces[] | select(.slug==$w or .label==$w or .workspace_id==$w) | .workspace_id' 2>/dev/null | head -1)"
[ -n "$WSID" ] || { echo "ERROR: no workspace row matching '$TGT' (slug/label/id) on this daemon" >&2; exit 1; }

BIN="$COMM_HOME/bin"
NUDGE="[sot-comm bootstrap from @$NAME] You have the sot-comm skill but are not joined. Please join and reply: run  $BIN/comm-join.sh${SUGG:+ --name $SUGG}  then  $BIN/comm-send.sh @$NAME \"joined as <yourname>\" . (Or just use the /sot-comm skill.) After this, we talk over sot-comm."

RESP="$(sot_pty_input "$WSID" "$(printf '%s' "$NUDGE" | base64 | tr -d '\n')" || true)"
if printf '%s' "$RESP" | jq -e '.payload.ok == true' >/dev/null 2>&1; then
    echo "Bootstrap nudge typed into row $WSID ($TGT). Waiting for it to join — check with comm-list.sh / comm-poll.sh."
else
    echo "ERROR: pty.input into row $WSID failed: $(printf '%s' "$RESP" | jq -c '.payload' 2>/dev/null || echo 'no reply')" >&2
    exit 1
fi
