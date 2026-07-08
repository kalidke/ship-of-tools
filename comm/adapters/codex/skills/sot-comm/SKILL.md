---
name: sot-comm
description: Use Ship of Tools comms from Codex: send/poll messages, coordinate with Claude Code or Codex peers, use socket-only daemon relay, report work-state, and show results through the frontend. Use when asked to message agents, check backlog, repair comms, use @handles, or drive the FE from a Codex session.
---

# sot-comm

Use the installed tools in `~/.sot-comm/bin/`; do not hand-roll registry, tmux,
or daemon protocol logic.

## Core Commands

```bash
~/.sot-comm/bin/comm-send.sh @<handle> "message"       # durable, registry-based
~/.sot-comm/bin/comm-send.sh --broadcast "message"
~/.sot-comm/bin/comm-poll.sh                           # read queued inbox
~/.sot-comm/bin/comm-list.sh                           # registered sessions
~/.sot-comm/bin/comm-status.sh waiting "watching X"    # sticky purple
~/.sot-comm/bin/comm-status.sh blocked "need Y"        # red
~/.sot-comm/bin/comm-relay.sh send @win-fe "message"   # daemon relay to attached FEs
~/.sot-comm/bin/sot-fe preview <workspace> <path>      # badge/show result in FE
```

Local text is not visible to peers. If you receive `[relay] from ...` or an
`@handle` request, answer with `comm-send.sh` or `comm-relay.sh`, not only in
assistant text.

## Socket-Only Backend

The normal backend listens on a private Unix socket, not remote TCP
`127.0.0.1:18743`.

Endpoint resolution for `comm-relay.sh`, `comm-spawn.sh`, `comm-despawn.sh`, and
`sot-fe` is:

1. explicit endpoint env or `--endpoint`
2. `$SOT_SOCKET`
3. old dev daemon args `--tcp` / `--socket`
4. `sotd session-socket-path ${SOT_BACKEND_LABEL:-sot}`

Override only when needed:

```bash
export SOT_RELAY_ENDPOINT=unix:/path/to/sot.sock        # backend host
export SOT_RELAY_ENDPOINT=tcp:127.0.0.1:<local-port>    # frontend host tunnel
```

On Windows or another frontend-local host, `127.0.0.1:<local-port>` is local to
that host and must SSH-forward to the remote Unix socket. It is not a remote
backend listener.

## Work-State

Hooks handle turn start/end and permission prompts. Self-report what hooks cannot
see:

```bash
~/.sot-comm/bin/comm-status.sh blocked "question for the user"
~/.sot-comm/bin/comm-status.sh waiting "background job/subagent still running"
~/.sot-comm/bin/comm-status.sh working "resuming"
```

Clear waiting when the job lands. A stale purple row lies to the user.

## Results

If work produces a visual or browsable artifact, show it before final response:

```bash
show-result <path>
```

For files inside a workspace, `sot-fe preview <workspace> <path>` badges the FE
without force-switching the user's current view.
