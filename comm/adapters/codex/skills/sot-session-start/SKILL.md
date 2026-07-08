---
name: sot-session-start
description: Bootstrap or repair a backend Codex session on the Ship of Tools comm network. Use after a Codex restart, manual tmux attach, lost comms, or when a session needs to join, start its relay listener, start codex-watch pane injection, poll backlog, and self-test socket-mode relay.
---

# sot-session-start

`ccx` normally runs this bootstrap before Codex starts. Run it manually only when
the session was started without `ccx`, was resumed in an existing pane, or comms
need repair.

## Steps

Set a default handle if the launcher did not:

```bash
if [ -z "${SOT_COMM_NAME:-}" ]; then
  repo="$(basename "$(git rev-parse --show-toplevel 2>/dev/null || pwd)")"
  host="$(hostname -s 2>/dev/null || hostname)"
  export SOT_COMM_NAME="${repo}-cx-${host}"
fi
```

Join, listen, and start the Codex wake helper:

```bash
~/.sot-comm/bin/comm-join.sh --name "$SOT_COMM_NAME"
~/.sot-comm/bin/comm-listen.sh
if [ -n "${TMUX_PANE:-}" ]; then
  nohup ~/.sot-comm/bin/codex-watch.sh "$SOT_COMM_NAME" "$TMUX_PANE" >/dev/null 2>&1 &
fi
~/.sot-comm/bin/comm-poll.sh
```

Then prove the relay receive path:

```bash
~/.sot-comm/bin/comm-listen.sh --selftest
```

Exit codes: `0` OK, `3` bridge still connecting on a cold start, `1` endpoint
missing or unreachable. For `3`, wait a few seconds and rerun once. The real
Codex wake proof is a typed `[relay] from __selftest__:` line from
`codex-watch.sh`.

## Socket-Only Endpoint

Do not expect remote TCP `127.0.0.1:18743`. Backend scripts auto-discover the
Unix socket with `sotd session-socket-path ${SOT_BACKEND_LABEL:-sot}`. Override
only for special cases:

```bash
export SOT_RELAY_ENDPOINT=unix:/path/to/sot.sock
```

Frontend hosts should use their local SSH-forwarded TCP endpoint instead.
