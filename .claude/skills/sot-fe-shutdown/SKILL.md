---
name: sot-fe-shutdown
description: Deterministically clean up and shut down the LOCAL Ship of Tools frontend — kill the supervisor, then the FE, wait for every remote daemon to detach the client cleanly, then tear every SSH tunnel (one per configured remote), then stop the local sotd (order matters — see below), leaving every REMOTE sotd + workspaces running by design. Use when the user says "clean up and shut down", "shut down the FE", "tear it all down", "close everything", or "/sot-fe-shutdown". NOT for a relaunch (that's the ADR-0017 sentinel) — this is a real quit with no respawn.
---

# sot-fe-shutdown

Deterministic teardown of the **local** frontend, its transport, and (ADR
0042 L1c) the **local** `sotd`. The REMOTE `sotd` and all backend state
(workspaces, tmux sessions, kernel + REPL) are **left running on purpose** —
the persistent-backend model (ADR 0010/0013) is what lets `claude --continue`
resume later. The LOCAL `sotd` is different: it's a launcher-managed daemon
`-Local` started on this same machine (ADR 0042 L1c), so this skill stops it too, last —
its capsule workspace supervisors (`sot-capsule.exe`) are NOT stopped by
this, on either host: they are separate detached processes and the daemon
re-adopts them via `--resume` on its next start, which is exactly what a
shutdown-and-later-relaunch is.

## Why a skill (and why a detached script)

Two problems make an ad-hoc "just kill it" unreliable:

1. **Ordering** (confirmed against the daemon code):
   a `Stop-Process` on a **live** FE makes the OS send FIN over every
   **still-open** tunnel → each remote daemon reads EOF → drops that client
   (`connections=N-1`) **immediately**. But if a **tunnel dies first**, its
   FIN can't propagate and that client is stranded as a **ghost** until the
   ADR-0027 keepalive reaper fires (~50 s). That ghost is the "FE not detaching
   on close" bug. So the order must be: **supervisor → FE → wait → tunnels →
   local sotd**. Killing the supervisor first stops it respawning the FE or
   racing us to tear a tunnel; the local `sotd` comes LAST because the FE
   (across every host it was connected to, local included — ADR 0042 L2b) is
   its only client, and stopping a daemon while its own client is still
   attached is the same class of bug as tearing a tunnel too early.

2. **Self-suicide**: this session runs *inside* the FE's Terminal drawer, so
   killing the FE kills this session mid-procedure. The teardown must therefore
   run **detached** and write its result to a log we (or the user) read after.

`scripts/shutdown-sot.ps1` encodes the ordering + verification. Keep the logic
in that script; keep orchestration here.

## Steps

1. **Confirm intent.** This is a real shutdown, not a relaunch — the FE will
   NOT come back on its own (relaunch is the `relaunch.request` sentinel). If
   the user actually wanted a rebuild-relaunch, stop and do that instead.

2. **Launch the teardown DETACHED** so it survives this session dying when the
   FE is killed:

   ```bash
   powershell.exe -NoProfile -Command "Start-Process powershell.exe -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','C:\\Users\\<you>\\...\\ship-of-tools\\scripts\\shutdown-sot.ps1' -WindowStyle Hidden"
   ```

   Pass `-TcpPort`/`-SshAlias` if this machine isn't on the default port
   `18743`, or if `$env:SOT_HOST` isn't set to the right backend host. Pass
   `-SkipDaemonVerify` when offline (skips the journal round-trip; the reaper
   still bounds any ghost at ~50 s).

   The script: kills the supervisor(s) (`launch-{sot,devenv}.ps1`) → kills the
   FE(s) (`sot.exe`) → waits ~2 s for the daemon to deregister → (best-effort)
   confirms `frontend disconnected … connections=N-1` in the `-SshAlias`
   backend's journal → kills every tunnel `ssh` it started (one per
   configured remote, `.sot/hosts.toml` — ADR 0042 L2b) → stops the LOCAL
   `sotd` on its per-user pipe (via `sot-local-daemon.ps1 -Stop`) → verifies
   nothing local remains. It leaves every REMOTE `sotd` alone.

3. **This session ends here** the moment the FE dies. There is nothing more to
   do on this turn — do not try to verify inline (the shell is gone).

4. **Verify afterward** (next session, or the user re-launches and asks): read
   `%LOCALAPPDATA%\sot\logs\shutdown.log`. Expect a final `CLEAN — local
   frontend and local daemon fully torn down` line and, in the REMOTE
   journal, the client's `connections` dropping. A `WARNING — residue
   remains` line means a stray supervisor/FE/tunnel/local-daemon survived —
   inspect and kill by hand. The local daemon's own log,
   `%LOCALAPPDATA%\sot\logs\sotd-local.log`, has the detail if the stop step
   itself needs diagnosing.

## Notes

- **Never** kill the REMOTE `sotd`, tmux sessions, or workspaces here — that
  breaks resume and is not what "shut down the frontend" means. If the user
  explicitly wants the *remote backend* down too, that's a separate,
  deliberate step on the backend host. The LOCAL `sotd` (ADR 0042 L1c) IS
  stopped here, by design — it is a launcher-managed process on this same
  machine, not a persistent shared backend.
- **Never** stop a capsule workspace's supervisor (`sot-capsule.exe`) here,
  on either host — it is the one authority over that workspace's live state,
  survives its daemon by design, and is re-adopted on the daemon's next
  `--resume`.
- Each tunnel is matched by its own `-L <port>:...` forward (one per
  configured remote — ADR 0042 L2b design E), so unrelated `ssh` sessions on
  the box are never touched. The local daemon is
  matched by process name `sotd.exe` AND its own per-user pipe name in the
  command line, so an unrelated `sotd.exe` (a different label/pipe) is never
  touched either.
- There is currently no daemon "goodbye"/force-drop op; clean socket close IS
  the detach, and the reaper is the safety net. If ghosts persist even with
  correct ordering, consider adding a `clients.list` op for positive
  verification, and/or a detach frame.
