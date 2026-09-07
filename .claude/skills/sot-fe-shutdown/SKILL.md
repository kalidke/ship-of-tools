---
name: sot-fe-shutdown
description: Deterministically shut down the LOCAL Ship of Tools frontend and local daemon; every REMOTE sotd + workspaces keep running by design. Use for "shut down the FE", "tear it all down", "close everything". NOT a relaunch — a real quit, no respawn.
---

# sot-fe-shutdown

Deterministic teardown of the **local** frontend, its transport, and (ADR
0042 L1c) the **local** `sotd` — order: **supervisor → FE → wait → tunnels →
local sotd**. The REMOTE `sotd` and all backend state (workspaces, tmux
sessions, kernel + REPL) are **left running on purpose** (ADR 0010/0013
persistent-backend model). Capsule workspace supervisors (`sot-capsule.exe`)
are NOT stopped here on either host — they are separate detached processes
the daemon re-adopts via `--resume` on its next start.

Full ordering rationale + the self-suicide problem (this session runs
*inside* the FE's Terminal drawer, so the teardown must run detached):
`scripts/shutdown-sot.ps1` header.

## Steps

1. **Confirm intent** — this is a real shutdown, not a relaunch (the FE will
   NOT come back on its own). If the user wanted a rebuild-relaunch, do that
   instead (ADR 0017's `relaunch.request` sentinel).

2. **Launch the teardown DETACHED** so it survives this session dying when
   the FE is killed:

   ```bash
   powershell.exe -NoProfile -Command "Start-Process powershell.exe -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','C:\\Users\\<you>\\...\\ship-of-tools\\scripts\\shutdown-sot.ps1' -WindowStyle Hidden"
   ```

   Pass `-TcpPort`/`-SshAlias` if this machine isn't on the default port or
   `$env:SOT_HOST` isn't the right backend host; `-SkipDaemonVerify` when
   offline (the reaper still bounds any ghost at ~50s).

3. **This session ends here** the moment the FE dies — nothing more to do
   this turn, don't try to verify inline.

4. **Verify afterward** (next session, or when the user relaunches): read
   `%LOCALAPPDATA%\sot\logs\shutdown.log`, expect `CLEAN — local frontend and
   local daemon fully torn down` and, in the REMOTE journal, the client's
   `connections` dropping. A `WARNING — residue remains` line means a stray
   supervisor/FE/tunnel/local-daemon survived — inspect and kill by hand.
   `%LOCALAPPDATA%\sot\logs\sotd-local.log` has detail on the stop step
   itself.

## Never

- Kill the REMOTE `sotd`, tmux sessions, or workspaces — that breaks resume.
  A deliberate remote-backend-down is a separate, explicit step there.
- Stop a capsule workspace's supervisor (`sot-capsule.exe`) on either host —
  it outlives its daemon by design and is re-adopted on the next `--resume`.
- Assume this touches an unrelated `ssh`/`sotd.exe` — each tunnel is matched
  by its own `-L <port>` forward, and the local daemon by process name AND
  its per-user pipe name in the command line.
