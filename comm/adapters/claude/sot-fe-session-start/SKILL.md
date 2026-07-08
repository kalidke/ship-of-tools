---
name: sot-fe-session-start
description: Bootstrap a Ship of Tools frontend-side Claude session after the native frontend opens or resumes its Terminal drawer. Sets the FE handle, points relay sends at the local SSH tunnel to the backend Unix socket, arms the frontend-local fe-inbox monitor, and optionally pings a backend session. Runnable manually or as a `claude --continue` resume turn. Activates for "fe session start", "frontend session start", "rearm fe comm", "fe bootstrap session".
---

# sot-fe-session-start

Run this inside the **frontend machine's Terminal drawer**. This is not the same
as `/sot-be-session-start`: the frontend machine usually does not share the
backend's `~/.sot-comm` registry, and it cannot open the backend's Unix socket
directly. The native frontend receives daemon relay messages and appends them to
its local `fe-inbox.jsonl`; this session tails that file for wakes and sends
outbound messages through the local SSH tunnel.

## Step 1 - set the FE handle

The handle must mirror the Rust frontend's `self_comm_handle()`:
`win-fe-<lowercase HOSTNAME or COMPUTERNAME>`.

PowerShell:

```powershell
$hostName = if ($env:HOSTNAME) { $env:HOSTNAME } elseif ($env:COMPUTERNAME) { $env:COMPUTERNAME } else { "unknown" }
$env:SOT_COMM_NAME = "win-fe-$($hostName.ToLowerInvariant())"
```

Git Bash / bash:

```bash
export SOT_COMM_NAME="win-fe-$( (hostname -s 2>/dev/null || hostname || printf unknown) | tr '[:upper:]' '[:lower:]' )"
```

If `~/.sot-comm/bin` is installed on this machine, join locally so
`comm-relay.sh` sends with the right `from:` name:

```bash
~/.sot-comm/bin/comm-join.sh --name "$SOT_COMM_NAME" --expertise "Ship of Tools frontend terminal"
```

This local join is for the FE machine's tools. Do not expect it to create a row
in the backend host's shared-home registry.

## Step 2 - point sends at the local tunnel

Socket-only backend mode means the remote backend normally listens on a private
Unix socket, discovered on the backend host with:

```bash
sotd session-socket-path ${SOT_BACKEND_LABEL:-sot}
```

The frontend launcher opens a **local** TCP port that forwards to that remote
Unix socket. Set relay sends to the local tunnel:

PowerShell:

```powershell
$port = if ($env:SOT_PORT) { $env:SOT_PORT } else { "18743" }
$env:SOT_RELAY_ENDPOINT = "tcp:127.0.0.1:$port"
```

Git Bash / bash:

```bash
export SOT_RELAY_ENDPOINT="tcp:127.0.0.1:${SOT_PORT:-18743}"
```

Do **not** try to connect to `127.0.0.1:18743` on the remote backend. In
socket-only mode that port belongs only on the frontend machine, and only while
the SSH tunnel/launcher is running. Browser helper ports `1234`-`1240` must also
be forwarded by the launcher for Pluto, docs, and static pages.

## Step 3 - arm the FE inbox monitor

The native frontend writes daemon `agent.message` events here:

- Windows: `%LOCALAPPDATA%\sot\fe-inbox.jsonl`
- Linux/macOS frontend: `${XDG_STATE_HOME:-$HOME/.local/state}/sot/fe-inbox.jsonl`

Arm a persistent Monitor on that local file. Local frontend storage is not NFS,
so `tail -F` / `Get-Content -Wait` is appropriate here.

PowerShell Monitor command:

```powershell
$dir = Join-Path $env:LOCALAPPDATA "sot"
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$inbox = Join-Path $dir "fe-inbox.jsonl"
if (!(Test-Path $inbox)) { New-Item -ItemType File -Path $inbox | Out-Null }
Get-Content -Path $inbox -Wait -Tail 0 | ForEach-Object {
  try {
    $m = $_ | ConvertFrom-Json
    if ($m.from -ne $env:SOT_COMM_NAME -and (($null -eq $m.to) -or ($m.to -eq "") -or ($m.to -eq $env:SOT_COMM_NAME))) {
      "[relay] from $($m.from): $($m.text)"
    }
  } catch {}
}
```

Git Bash / bash Monitor command:

```bash
state="${XDG_STATE_HOME:-$HOME/.local/state}/sot"
[ -n "${LOCALAPPDATA:-}" ] && state="$LOCALAPPDATA/sot"
mkdir -p "$state"
touch "$state/fe-inbox.jsonl"
tail -n0 -F "$state/fe-inbox.jsonl" | while IFS= read -r line; do
  printf '%s\n' "$line" | jq -r --arg me "$SOT_COMM_NAME" \
    'select((.from // "") != $me and (((.to // "") == "") or (.to == $me))) | "[relay] from \(.from): \(.text)"'
done
```

Without this Monitor, the FE receives messages but this Claude session will not
wake on them.

## Step 4 - announce to a backend session

If you know the backend session handle, send a directed ping:

```bash
~/.sot-comm/bin/comm-relay.sh send @<be-handle> "[$SOT_COMM_NAME] FE receive path armed; please ack."
```

If you do not know the handle, ask the human or use a broadcast as a low-priority
announcement. Broadcasts may file silently on backend sessions, so they are not a
round-trip proof.

## Troubleshooting

- `connection refused` on `127.0.0.1:18743`: the local SSH tunnel is not running
  or is bound to a different local port. Restart the frontend launcher/tunnel.
- Backend says no remote TCP listener: expected in socket-only mode. Query and
  forward the remote Unix socket instead.
- No messages wake the FE Claude session: confirm the native frontend is running,
  `fe-inbox.jsonl` is growing, and the Monitor above is still active.
