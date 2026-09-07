---
name: sot-fe-session-start
description: Bootstrap a frontend-side Codex session for Ship of Tools by running comm-session-start.sh, which already derives the win-fe handle, points at the local tunnel, and reads fe-inbox.jsonl on Windows. Use in a local FE terminal/Codex session.
---

# sot-fe-session-start

Run this on the frontend machine, in two calls:

```bash
~/.sot-comm/bin/comm-session-start.sh              # phase 1: arm
~/.sot-comm/bin/comm-session-start.sh --catch-up   # phase 2: after arming
```

On Windows, when this session is recognized as the FE-driver ROLE (not
merely "running on Windows" — comm-session-skill.sh's own routing decides
this), phase 1 already: derives the `win-fe-<host>` handle (mirrors the
native frontend's own handle — do not override unless you need a specific
sibling identity) and defaults `$SOT_RELAY_ENDPOINT` to the local
SSH-forwarded tunnel (`tcp:127.0.0.1:${SOT_PORT:-18743}`) if not already
set; phase 2 reads/cursors `fe-inbox.jsonl` directly for the initial
backlog — never expect a bridge or a `~/.sot-comm` row on the backend host's
shared registry.

Phase 1 prints a `MONITOR:` command, but there is no tmux wake helper on a
non-tmux Codex FE — nothing re-arms it automatically. So beyond the
phase-2 catch-up, at every later turn start (or when told there is FE
backlog) read the local FE inbox directly and answer via `comm-relay.sh`:

- Windows: `%LOCALAPPDATA%\sot\fe-inbox.jsonl`
- Linux/macOS FE: `${XDG_STATE_HOME:-$HOME/.local/state}/sot/fe-inbox.jsonl`

Treat a line as addressed to you when `to` is your handle or the bare
`win-fe` family label; a sibling's handle or a true broadcast (`to:""`) is an
FYI only — anything broadcast that matters is also on the durable git bus
(`bus.sh sync`).

Read your OWN outbound too (`from:<your handle>`) — a relaunch keeps the
handle and discards the context, so this session is accountable for
messages it never wrote and has no memory of. Before contradicting a peer's
account of what "you" said, grep the record first; scope every denial to
the session ("no record in this session, I was relaunched at HH:MMZ"), never
"that never happened" — never re-assert a prior session's commitment as your
own.

If a backend handle is known, also send a directed announce:

```bash
~/.sot-comm/bin/comm-relay.sh send @<be-handle> "[$SOT_COMM_NAME] FE relay path armed; please ack."
```
