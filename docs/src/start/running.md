# Running & Relaunch

This page covers starting Ship of Tools, the Terminal drawer, reconnecting
after a drop, and the self-relaunch loop that lets the frontend rebuild and
restart itself.

## Launching

Start the app through your launcher. A **release install** ([Install](install.md))
created `sot-launch` (on `PATH` in `~/.local/bin`) plus a desktop entry on
Linux or an app bundle on macOS — run either. A **source setup**
([Per-Machine Setup](setup.md)) uses the repo launcher: a desktop shortcut on
Windows, or `scripts/launch-sot.ps1` directly. Either way the launcher is the
**supervisor**: it owns the SSH tunnel to the backend host, starts the
frontend, and watches for relaunch requests.

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

A **comm-tooling session** (a Claude Code session driving `sot-fe`/
`comm-relay.sh` from a shell on the Windows box, not the frontend app itself)
sees the same two daemons and must pick between them explicitly. With no
`--endpoint`, those scripts prefer the box's OWN local daemon — reached over
its named pipe (`\\.\pipe\sot-<user>-local`), never a loopback port — so an
unqualified `sot-fe screen <workspace>` reaches that box's own rows. To reach
the **backend** instead (a different daemon, with its own, separate set of
rows), pass `--endpoint tcp:127.0.0.1:$SOT_TCP_PORT` explicitly: that loopback
port is the SSH tunnel the launcher already opened, not a second local
listener. There is no auto-routing by workspace name between the two — only
the (unbuilt) remote-attach bridge could add that.

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

When Ship of Tools is developed on itself, the dev `claude` session that
drives the frontend runs as a **first-class local capsule session** (create
one from the Sessions view with agent `claude`), not inside this drawer —
the drawer itself just runs a plain shell.

## Reconnecting

The backend is a long-lived daemon; the connection can drop (laptop wake, wifi
flap, SSH timeout) without losing session state. Press **`F5`**
(`transport.reconnect`) to reconnect. Every connect carries the session id and
the last revision the client saw, so the daemon replays missed events from a
bounded ring or sends a fresh snapshot — reconnect feels like reattaching a tmux
session. The supervisor keeps the SSH tunnel alive across these reconnects; only
a real quit tears it down.

## Browser-Backed Previews

`W` opens HTML/static sites, video and Pluto pages, and interactive WGLMakie
figures (`wglshow`) in your OS browser. Against a v0.5.0+ backend these pages
ride the **control tunnel itself**: the frontend binds local loopback
listeners on demand and relays them through the daemon's TCP proxy (ADR
0035), whose allowlist covers only ports *your* daemon actually bound. **No
extra SSH forwards are needed** — one control forward is the whole tunnel.

If the browser shows `127.0.0.1 refused to connect` for `W`/video/Pluto,
suspect the frontend↔daemon connection (see the reconnect notes above), not a
missing port forward.

Only when talking to a **pre-v0.5.0 backend** do the fixed helper ports
(`1234` Pluto, `1235` video, `1236`-`1240` docs, `1241` WGLMakie) still need
old-style forwarding — set `SOT_LEGACY_FORWARDS=1` before launching and the
launcher opens them. Avoid this on shared hosts: a fixed port you didn't bind
may belong to another user's server, and the browser will render their
content without any error.

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
- **The drawer reopens plain.** On `--relaunched`, the frontend opens straight
  into the Terminal drawer with a plain shell and runs nothing automatically —
  the old `[terminal] resume_command` setting that used to prime it is
  retired. A session that needs to survive frontend relaunches (the dev
  driver above is one) is a first-class **local capsule session** instead: it
  is adopted by its own supervisor independent of the frontend process, so it
  rides through the relaunch untouched and needs no priming command.

The one-command driver is `scripts/relaunch-sot.ps1`: it runs
`cargo build --release` and drops the relaunch sentinel **only on a green
build** — a failed build leaves the running app untouched.

## Prefer the relaunch loop over killing the frontend

!!! warning "Use the relaunch loop, not a process kill"
    The dev `claude` session that drives frontend development is a local
    capsule session, not a passenger of the frontend process, so it survives
    either way. Still restart through the relaunch loop —
    `scripts/relaunch-sot.ps1` (build → sentinel → exit-75 → re-stage →
    respawn) — rather than a process kill: it re-stages the freshly built
    binary and keeps the supervisor's SSH tunnel alive across the swap.

Note that changes to the *supervisor script itself* (`launch-sot.ps1`) are not
picked up by the exit-75 in-place loop — those require a full restart of the
launcher. The exit-75 path only re-stages the frontend binary.

## Next steps

- [A Guided Tour](tour.md) — walk a first session, mode by mode and drawer by
  drawer.
