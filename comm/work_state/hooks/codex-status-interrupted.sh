#!/usr/bin/env bash
[ "${SOT_COMM_HOOKS:-}" != off ] && [ "${SOT_AGENT_KIND:-}" = codex ] || exit 0
bin="${SOT_COMM_HOME:-$HOME/.sot-comm}/bin"
[ -r "$bin/codex-turn.sh" ] || exit 0
. "$bin/codex-turn.sh"
case ${SOT_LOCK_WAIT_SECS:-} in 0|1|2) ;; *) export SOT_LOCK_WAIT_SECS=3 ;; esac
[ -z "$SOT_CODEX_TURN" ] || "$bin/comm-status.sh" interrupt >/dev/null 2>&1
exit 0
