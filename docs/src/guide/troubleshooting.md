# Troubleshooting

First move for any "how do I" or "why is it doing that" question: ask the agent
in your session. Every workspace exports `$SOT_MANUAL`, pointing at the release
checkout — this manual, the ADRs and the requirements — so the agent answers
from the same docs you are reading, matched to the version you installed.

## [The window does not open](@id window-does-not-open)

Run `sot-launch` in a terminal on the frontend machine: the window runs in the
foreground there, so its error is printed. On Linux the window needs a desktop
session (X11 or Wayland) and a working Vulkan driver.

- **`no compatible wgpu adapter found`**: no usable Vulkan driver. Install the
  driver for your GPU (Mesa for Intel and AMD, the NVIDIA driver for NVIDIA);
  `vulkaninfo --summary`, from the `vulkan-tools` package, should then list
  the GPU.
- **An error about opening a display**: the shell has no graphical desktop,
  for example an SSH login. Run the window on the machine in front of you and
  keep the backend on the server; see [Going remote](../start/remote.md).

## The connection dropped

Press **`F5`** to reconnect. The daemon replays what you missed or sends a
fresh snapshot; agent sessions and the REPL keep running on the backend while
you are away. See [Going remote](../start/remote.md#Reconnecting).

## A browser page says `127.0.0.1 refused to connect`

`Shift+W` (built docs), `o` (video playback, Pluto) and `wglshow` pages are
relayed through the frontend↔daemon connection, not through separate port
forwards. Suspect that
connection first: reconnect with `F5`, then reopen the page.

## The Windows taskbar launch looks dead

The Windows shortcut runs the launcher hidden, so an early failure can look like
"the taskbar click did nothing." Check these first:

- `%LOCALAPPDATA%\sot\logs\launch-status.txt` — last launcher phase or fatal
  status.
- `%LOCALAPPDATA%\sot\logs\supervisor.log` — detailed launcher, rebuild,
  tunnel, and frontend respawn log.
- `%APPDATA%\Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar` —
  the pinned `.lnk`; stale pins can still point at an old binary or an old
  checkout.

After changing the topology on the hub (a new `[host.<name>]`, a flag flip),
`sotd topology sync` on every other box picks it up on its next launch — no
shortcut re-run needed for that. Re-run the shortcut installer only after
moving the repo or the launcher script:

```powershell
pwsh -File scripts\install-shortcut.ps1
```

That refreshes the desktop shortcut and repoints any existing Ship of Tools
taskbar pin to `scripts\launch-sot.ps1`.

## A session will not start on a shared home

The daemon keeps capsule session records under
`${XDG_STATE_HOME:-~/.local/state}/sot` and refuses to start a capsule row on a
remote filesystem (NFS, SMB/CIFS, 9p, an unqualified FUSE mount) or a volatile
one (tmpfs, ramfs), answering `state_root_unqualified` and naming the
filesystem. On a home shared over NFS, point each host's state root at its
own local disk with a drop-in at
`~/.config/systemd/user/sotd.service.d/state.conf`:

```ini
[Service]
Environment=XDG_STATE_HOME=/path/on/local/disk/state
```

then `systemctl --user daemon-reload && systemctl --user restart sotd`.
Relocating the root does not migrate existing rows — end that host's capsule
runs first. See [Sessions and persistence](../concepts/sessions.md).

## Windows: never launch the daemon from an agent's shell

Never start the frontend or the daemon from a shell running under an agent (a
Claude pane, a capsule row's shell). The agent process sits in a Windows job
object that forbids breakaway, and a daemon started there can never free the
supervisors it spawns: its sessions die with it. Launch from the launcher or a
plain shell. The daemon log says `this daemon's own job forbids breakaway`
when this has happened.

## Enrolment failures

A host that should be reachable from every frontend but is not: see
[Reading a failure](enrolling.md#Reading-a-failure) in the enrolment checklist.
