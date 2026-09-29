---
name: sot-comm
description: "Use Ship of Tools comms from Codex: send/poll messages, coordinate with peers over the daemon relay, report work-state, show results in the frontend. Use when asked to message agents, check backlog, use @handles, or drive the FE from a Codex session."
---

# sot-comm

Use the installed tools in `~/.sot-comm/bin/`; do not hand-roll registry or
daemon protocol logic.

## Core Commands

```bash
~/.sot-comm/bin/comm-send.sh @<handle> "message"       # durable, registry-based
~/.sot-comm/bin/comm-send.sh --broadcast "message"
~/.sot-comm/bin/comm-poll.sh                           # read queued inbox
~/.sot-comm/bin/comm-list.sh                           # registered sessions
~/.sot-comm/bin/comm-status.sh waiting "watching X"    # purple until you report
~/.sot-comm/bin/comm-status.sh blocked "need Y"        # red
~/.sot-comm/bin/sot-fe notify "message"                # toast on the attached FE(s)
~/.sot-comm/bin/sot-fe preview <workspace> <path>      # badge/show result in FE
```

Local text is not visible to peers. If you receive `[relay] from ...` or an
`@handle` request, answer with `comm-send.sh` or `comm-relay.sh`, not only in
assistant text.

## Socket-Only Backend

The normal backend listens on a private Unix socket. It has had no remote TCP
listener since 0.4.0.

Endpoint resolution for `comm-relay.sh`, `comm-spawn.sh`, `comm-despawn.sh`, and
`sot-fe` is one gate at the point a value is returned: an explicit endpoint,
else `$SOT_SOCKET`, else what `sotd` answers (its own endpoint, or the
declared plan's), else an old dev daemon's `--socket` arg — every one of
them through that gate, which dials only `unix:`, `pipe:` and `ssh:`.

Override only when needed:

```bash
export SOT_RELAY_ENDPOINT=unix:/path/to/sot.sock   # backend host
export SOT_RELAY_ENDPOINT=ssh:<hub>                # frontend host reaching the hub
```

On Windows or another frontend-local host, `ssh:<hub>` spawns its own `ssh`
child to the hub and speaks the protocol over its stdio (C3) — never a
forwarded port, and never this box's own local daemon substituted silently.

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
without force-switching the user's current view. Full verb/flag list:
`sot-fe --help` (`goto`, `mode`, `notify`, `open-url`, `repl run|eval|
interrupt|status`, `type`, `screen`, ...).

## Typing into / reading a sibling row

`sot-fe type <workspace> [<text>] [--stdin] [--enter]` sends literal bytes
into another session's pane (`pty.input`); `sot-fe screen <workspace>` prints
its current screen, no scrollback (`pty.screen`). **Read before you write:**
never send `type` — not even a bare Enter — to a pane you haven't just read
with `screen`; a blind keystroke can end a session instead of advancing it.
