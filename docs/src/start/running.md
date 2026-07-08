# Running & Relaunch

This page covers starting Ship of Tools, the Terminal drawer that hosts your dev
session, reconnecting after a drop, and the self-relaunch loop that lets the
frontend rebuild and restart itself.

## Launching

Start the app through the launcher created by [Per-Machine Setup](setup.md) (a
desktop shortcut on Windows, or `scripts/launch-sot.ps1` directly). The
launcher is the **supervisor**: it owns the SSH tunnel to the backend host,
starts the frontend, and watches for relaunch requests.

The default launch connects to the remote backend over an SSH local-forwarded
socket — the canonical "Windows local · Linux remote-in-tmux" workflow. The
backend is started once on the remote and survives across launches, so a second
launch is fast; the SSH forward is fresh each time and torn down when the
frontend exits. Pass `-Local` to fall back to a backend spawned on the same
machine over a named pipe (offline / debugging).

Which remote you connect to comes from the persisted host choice (Hosts mode,
hotkey `h`) resolved against `.sot/hosts.toml`; environment overrides
(`SOT_HOST`, `SOT_REMOTE_REPO`, `SOT_TCP_PORT`, `SOT_REMOTE_SOCKET`) win over both. See
[Per-Machine Setup](setup.md).

## Windows Taskbar Launch Looks Dead

The Windows shortcut runs the launcher hidden, so an early failure can look like
"the taskbar click did nothing." Check these first:

- `%LOCALAPPDATA%\sot\logs\launch-status.txt` — last launcher phase or fatal
  status.
- `%LOCALAPPDATA%\sot\logs\supervisor.log` — detailed launcher, rebuild,
  tunnel, and frontend respawn log.
- `%APPDATA%\Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar` —
  the pinned `.lnk`; stale pins can still point at an old bare `sot.exe` or an
  old checkout.

After writing or changing `.sot\hosts.toml`, rerun:

```powershell
pwsh -File scripts\install-shortcut.ps1
```

That refreshes the desktop shortcut and repoints any existing Ship of Tools
taskbar pin to `scripts\launch-sot.ps1`. If `.sot\hosts.toml` is missing, the
shortcut can still be created, but launch will fail with "no backend host
configured."

## The Terminal drawer

The frontend hosts a local OS shell in a bottom drawer, toggled with `Ctrl+T`.
This is a **local** shell on the frontend machine — its canonical use is SSHing
outward to backend hosts — and it works even when the backend is unreachable; it
is not proxied through the daemon.

The drawer is a single slot shared with the REPL (`Ctrl+J`): each key toggles its
own pane, and pressing the other key swaps the content.

| Key | Drawer closed | Showing this pane | Showing the other pane |
|-----|---------------|-------------------|------------------------|
| `Ctrl+J` | → REPL | → closed | → REPL |
| `Ctrl+T` | → Terminal | → closed | → Terminal |

When Ship of Tools is developed on itself, the dev `claude` session runs **inside this
Terminal drawer**.

## Reconnecting

The backend is a long-lived daemon; the connection can drop (laptop wake, wifi
flap, SSH timeout) without losing session state. Press **`F5`**
(`transport.reconnect`) to reconnect. Every connect carries the session id and
the last revision the client saw, so the daemon replays missed events from a
bounded ring or sends a fresh snapshot — reconnect feels like reattaching a tmux
session. The supervisor keeps the SSH tunnel alive across these reconnects; only
a real quit tears it down.

## Browser-Backed Previews

`W` opens HTML/static sites through backend HTTP ports that must be forwarded to
the frontend machine: `1236` for normal static pages, plus `1237`-`1240` for
sites with root-relative assets. Video and Pluto use the same pattern on `1235`
and `1234`.

If you manually reconnect with only the backend control tunnel, the frontend can
still talk to `sotd` but the local browser will show `127.0.0.1 refused to
connect` for `W`/video/Pluto. Open an aux tunnel as well:

```bash
ssh -N \
  -L 1234:127.0.0.1:1234 \
  -L 1235:127.0.0.1:1235 \
  -L 1236:127.0.0.1:1236 \
  -L 1237:127.0.0.1:1237 \
  -L 1238:127.0.0.1:1238 \
  -L 1239:127.0.0.1:1239 \
  -L 1240:127.0.0.1:1240 \
  <backend-ssh-alias>
```

## Existing tmux sessions are missing

By default `sotd` uses a private per-user tmux socket for workspace panes. If you
already have long-lived `sot-be-*` sessions on tmux's default server, the
frontend may show workspace rows but attach to fresh empty panes after a backend
restart. Point the backend at the existing tmux server for that migration:

```bash
export SOT_TMUX_SOCK="${TMUX%%,*}"   # from inside the old tmux server
sotd tmux-socket-path
```

For a user systemd service, add the same environment variable to the service
environment and restart `sotd`. The backend still checks the socket parent
directory before spawning tmux; use this only for a tmux socket owned by the
same Unix account.

## Self-relaunch: rebuild without dropping your session

Ship of Tools can rebuild and restart its own frontend — so you can edit the frontend,
recompile, and relaunch into the new binary without leaving the app. The moving
parts:

- **Staged-copy supervisor.** The launcher copies the built
  `sot` into a staging directory (`%LOCALAPPDATA%\sot\bin\`) and
  runs the app from that copy inside a respawn loop. Because the running file is
  the staged copy, `cargo build --release` can overwrite `rust/target/release/`
  freely — no running-exe file lock — and you see build output live.
- **Exit-75 sentinel.** The frontend requests a relaunch by exiting with code
  **75**; any other code is a real quit. A background watcher polls for a
  relaunch-request sentinel file; on seeing it, the frontend exits 75 and the
  supervisor re-stages the (freshly built) binary and respawns with
  `--relaunched`.
- **`claude --continue` resume.** On `--relaunched`, the frontend opens straight
  into the Terminal drawer and runs the configured `[terminal] resume_command`
  as the shell's first command. The default resumes the dev session without
  prompts:

  ```toml
  [terminal]
  resume_command = "claude --dangerously-skip-permissions --continue /sot-fe-session-start"
  ```

  Session continuity is decoupled from process survival: the terminal session
  does not need to live through the restart because `claude --continue` resumes
  it from its own store.

The one-command driver is `scripts/relaunch-sot.ps1`: it runs
`cargo build --release` and drops the relaunch sentinel **only on a green
build** — a failed build leaves the running app untouched.

## Do not kill the frontend to restart it

!!! warning "Never kill the frontend process"
    When Ship of Tools is being developed on itself, the dev `claude` runs **inside the
    frontend's Terminal drawer**. Killing the frontend process therefore kills
    your own session along with it. To restart, always use the relaunch loop —
    `scripts/relaunch-sot.ps1` (build → sentinel → exit-75 → re-stage →
    respawn), never a process kill.

Note that changes to the *supervisor script itself* (`launch-sot.ps1`) are not
picked up by the exit-75 in-place loop — those require a full restart of the
launcher. The exit-75 path only re-stages the frontend binary.

## Next steps

- [A Guided Tour](tour.md) — walk a first session, mode by mode and drawer by
  drawer.
