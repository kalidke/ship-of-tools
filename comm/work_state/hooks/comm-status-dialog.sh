#!/usr/bin/env bash
# comm-status-dialog.sh — Claude Code `PermissionRequest` hook: a permission
# dialog is open and waits on a person, so the row reads red (ADR 0044
# amendment, `dialog`). Sends the event `dialog "<text>"`; the row clears it
# when the call returns (comm-status-heartbeat.sh) or the turn's boundary comes.
#
# It prints no decision, so the dialog shows as it always did. PermissionRequest
# fires when the dialog opens, so a quick approval reads red briefly too, which
# is true: a person is being asked. The same script can be wired to Notification
# `permission_prompt`, which waits about six seconds first: then the envelope
# carries `message`, used as the text. A Notification of any other type (an
# `idle_prompt`) writes nothing, whatever matcher wired it.
#
# Self-gating: comm-status.sh no-ops in a pane without a registry row. Always
# exits 0 — a hook must never wedge the permission flow.
#
# Source of truth: comm/work_state/hooks/comm-status-dialog.sh in
# Ship of Tools, deployed to ~/.sot-comm/bin by ShipTools.update_comm().
set -uo pipefail
[ "${SOT_COMM_HOOKS:-}" = off ] && exit 0
COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
STATUS="$COMM_HOME/bin/comm-status.sh"
[ -x "$STATUS" ] || exit 0
fields="$(jq -r '[.notification_type // "", .message // "", .tool_name // ""] | join("\u001f")' 2>/dev/null || true)"   # consumes hook stdin
type="${fields%%$'\x1f'*}"; rest="${fields#*$'\x1f'}"
message="${rest%%$'\x1f'*}"; tool="${rest#*$'\x1f'}"
[ -z "$type" ] || [ "$type" = permission_prompt ] || exit 0
text="$message"
[ -n "$text" ] || text="${tool:+Claude needs your permission to use $tool}"
"$STATUS" dialog "${text:-permission prompt}" >/dev/null 2>&1 || true
exit 0
