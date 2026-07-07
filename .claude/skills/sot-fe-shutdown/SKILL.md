---
name: sot-fe-shutdown
description: Deterministically clean up and shut down the LOCAL Ship of Tools frontend — kill the supervisor, then the FE, wait for the daemon to detach the client cleanly, then tear the SSH tunnel (order matters — see below), leaving the remote sotd + workspaces running by design. Use when the user says "clean up and shut down", "shut down the FE", "tear it all down", "close everything", or "/sot-fe-shutdown". NOT for a relaunch (that's the ADR-0017 sentinel) — this is a real quit with no respawn.
---

# sot-fe-shutdown

Deterministic teardown of the **local** frontend + its transport. The remote
`sotd` and all backend state (workspaces, tmux sessions, kernel + REPL) are
**left running on purpose** — the persistent-backend model (ADR 0010/0013) is
what lets `claude --continue` resume later. This skill only closes the local FE
and its SSH tunnel.

## Why a skill (and why a detached script)

Two problems make an ad-hoc "just kill it" unreliable:

1. **Ordering** (confirmed against the daemon code):
   a `Stop-Process` on a **live** FE makes the OS send FIN over the
   **still-open** tunnel → the daemon reads EOF → drops the client
   (`connections=N-1`) **immediately**. But if the **tunnel dies first**, the
   FIN can't propagate and the client is stranded as a **ghost** until the
   ADR-0027 keepalive reaper fires (~50 s). That ghost is the "FE not detaching
   on close" bug. So the order must be: **supervisor → FE → wait → tunnel**.
   Killing the supervisor first stops it respawning the FE or racing us to tear
   the tunnel.

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
   confirms `frontend disconnected … connections=N-1` in the backend's journal →
   kills only the tunnel `ssh` forwarding this port → verifies nothing local
   remains. It leaves the remote `sotd` alone.

3. **This session ends here** the moment the FE dies. There is nothing more to
   do on this turn — do not try to verify inline (the shell is gone).

4. **Verify afterward** (next session, or the user re-launches and asks): read
   `%LOCALAPPDATA%\sot\logs\shutdown.log`. Expect a final `CLEAN — local
   frontend fully torn down` line and, in the journal, the client's
   `connections` dropping. A `WARNING — residue remains` line means a stray
   supervisor/FE/tunnel survived — inspect and kill by hand.

## Notes

- **Never** kill remote `sotd`, tmux sessions, or workspaces here — that breaks
  resume and is not what "shut down the frontend" means. If the user explicitly
  wants the *backend* down too, that's a separate, deliberate step on the
  backend host.
- The tunnel is matched by its `-L <port>:127.0.0.1:<port>` forward, so
  unrelated `ssh` sessions on the box are never touched.
- There is currently no daemon "goodbye"/force-drop op; clean socket close IS
  the detach, and the reaper is the safety net. If ghosts persist even with
  correct ordering, consider adding a `clients.list` op for positive
  verification, and/or a detach frame.
