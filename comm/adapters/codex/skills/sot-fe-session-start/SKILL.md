---
name: sot-fe-session-start
description: Bootstrap a frontend-side Codex session for Ship of Tools. Use in a local FE terminal/Codex session to set the win-fe handle, send through the local SSH tunnel to the backend Unix socket, read the frontend-local fe-inbox, and coordinate with backend sessions without relying on old remote TCP.
---

# sot-fe-session-start

Run this on the frontend machine. The FE machine usually has its own filesystem
and registry, so do not expect a local `comm-join.sh` to create a backend-host
registry row.

## Handle

Match the native frontend handle:

PowerShell:

```powershell
$hostName = if ($env:HOSTNAME) { $env:HOSTNAME } elseif ($env:COMPUTERNAME) { $env:COMPUTERNAME } else { "unknown" }
$env:SOT_COMM_NAME = "win-fe-$($hostName.ToLowerInvariant())"
```

Bash:

```bash
export SOT_COMM_NAME="win-fe-$( (hostname -s 2>/dev/null || hostname || printf unknown) | tr '[:upper:]' '[:lower:]' )"
```

If `~/.sot-comm/bin` is installed locally, join with that name so outbound relay
messages have the right `from:`:

```bash
~/.sot-comm/bin/comm-join.sh --name "$SOT_COMM_NAME"
```

## Endpoint

The backend is socket-only by default. The frontend sends through a local TCP
tunnel that forwards to the remote Unix socket:

PowerShell:

```powershell
$port = if ($env:SOT_PORT) { $env:SOT_PORT } else { "18743" }
$env:SOT_RELAY_ENDPOINT = "tcp:127.0.0.1:$port"
```

Bash:

```bash
export SOT_RELAY_ENDPOINT="tcp:127.0.0.1:${SOT_PORT:-18743}"
```

Do not probe remote `127.0.0.1:18743`. Query the remote socket with
`sotd session-socket-path sot` and forward local `18743` to that socket.

## Inbox

The native frontend appends daemon `agent.message` events to:

- Windows: `%LOCALAPPDATA%\sot\fe-inbox.jsonl`
- Linux/macOS FE: `${XDG_STATE_HOME:-$HOME/.local/state}/sot/fe-inbox.jsonl`

Codex FE sessions do not automatically get a tmux `codex-watch.sh` wake unless
they are running inside tmux. At turn start, or when told there is FE backlog,
read the local FE inbox and answer via `comm-relay.sh`.

PowerShell backlog check:

```powershell
$inbox = Join-Path $env:LOCALAPPDATA "sot\fe-inbox.jsonl"
if (Test-Path $inbox) { Get-Content $inbox -Tail 40 }
```

Bash backlog check:

```bash
state="${XDG_STATE_HOME:-$HOME/.local/state}/sot"
[ -n "${LOCALAPPDATA:-}" ] && state="$LOCALAPPDATA/sot"
[ -f "$state/fe-inbox.jsonl" ] && tail -40 "$state/fe-inbox.jsonl"
```

## Ping BE

If the backend handle is known:

```bash
~/.sot-comm/bin/comm-relay.sh send @<be-handle> "[$SOT_COMM_NAME] FE relay path armed; please ack."
```

If no direct route exists yet, use the human-provided temporary dropbox only for
that live incident; never commit private machine paths or secrets to the repo.
