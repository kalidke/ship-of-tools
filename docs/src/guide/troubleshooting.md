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

Keys typed into a session pane before it shows attached (while it connects,
reconnects, or its host is down) are never sent later; so are keys refused
because 64 KiB of input is already waiting to be sent. The pane's top line says
how many were discarded. Wait for the pane to show it is attached before
typing. An agent pane only one row tall shows the reason it is not live but not
the discarded count.

## The agent pane says `frontend out of date`

The daemon speaks a newer protocol than this frontend, so the two cannot talk.
Update the frontend (see [Updating](updating.md)). `daemon out of date` is the
reverse skew: the daemon is the side that is behind. The preview pane shows both
versions.

When a launch finds the daemon on an older version while a window is open, it leaves that daemon running and the new
window's startup notice says `daemon out of date, restart when ready`. Restart the daemon after closing your other
windows, or launch again once none is open; a launch that replaces a daemon says its rows keep running and their REPLs
restart.

## Text is cut off at the bottom of the preview

The preview stops each block of text and each table at its last whole line, so no line is drawn
cut in half; figure captions and the scale-bar label are not covered. While you edit an annotation
you can scroll (mouse wheel) to the end of what you are editing.
Known limits: a pane shorter than one line of text still shows that one partial line, and a
table's last whole row can sit in the narrow bottom margin where body text stops.

## A pane stays at `connecting…`

The status line under the pane names the reason the capsule gave. `capsule busy
grounding a checkpoint` clears itself within seconds. `capsule watcher slots all
taken` does not: a capsule accepts a bounded number of watchers, and connections
left behind by a frontend that went away without closing them keep their slots.
Restart the backend daemon (`systemctl --user restart sotd`) — that closes the
connections it was holding and frees the slots, and the sessions themselves keep
running, because each capsule supervisor lives in its own transient systemd
scope rather than the daemon's. The exception is a supervisor the daemon log
marked `no transient user scope available`: that one does share the daemon's
kill domain.

## A browser page says `127.0.0.1 refused to connect`

`Shift+W` (built docs), `o` (video playback, Pluto) and `wglshow` pages are
relayed through the frontend↔daemon connection, over the same `ssh` child that
connection already dialled for that host — not a separate port forward. A
failed dial shows up the same way any dial failure does: that child's last
stderr line, in the host row's status text. See
[Transport](../concepts/transport.md). Reconnect with `F5`, then reopen the
page.

## The REPL says it has no display of its own

The REPL's first run on a Linux backend prints, on stderr, `sot: this REPL has no display of its own (...), and GLMakie
needs one. CairoMakie and WGLMakie (wglshow) work.` GLMakie needs an X display, and the daemon gives the REPL one by
starting `Xvfb`. The reason in the parentheses says why it could not: `Xvfb is not installed: none on the daemon's PATH`
means the program is missing, so install it (`sudo apt-get install xvfb` on Debian and Ubuntu). The REPL that printed
the notice keeps running without a display; start a new one with `r` on a `.jl` file in Files mode, or
`sot-fe repl run <ws> <path> --fresh`, and it gets its display. Any other reason ends with the last lines Xvfb wrote. CairoMakie and WGLMakie need no
display and keep working. See [Prerequisites](../start/install.md#install-prerequisites).

## The REPL ended because its display ended

The REPL's display, its `Xvfb`, ended while the REPL ran, so the daemon ended the REPL with it. An eval in flight
answers `the REPL's own display (Xvfb) ended, so the REPL was ended with it before this eval ended`. Run again: the next
run starts a new REPL with a new display and prints `sot: the REPL before this one was ended because its own display
(Xvfb) ended; this is a new REPL with a new display, and nothing the old one defined or loaded is here` first. Redefine
what you need; nothing of the old REPL survives. Pluto's server is ended the same way when its display ends.

## A browser page opened in a different window than expected

A browser open goes to exactly one window, the one in use, and `--fe <fe>` names another. The rule is in
[A second frontend](@ref browser-window).

## The Windows taskbar launch looks dead

The Windows shortcut runs the launcher hidden, so an early failure can look like
"the taskbar click did nothing." Check these first:

- `%LOCALAPPDATA%\sot\logs\launch-status.txt` — last launcher phase or fatal
  status.
- `%LOCALAPPDATA%\sot\logs\supervisor.log` — detailed launcher, rebuild,
  and frontend respawn log.
- `%APPDATA%\Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar` —
  the pinned `.lnk`; stale pins can still point at an old binary or an old
  checkout.

After changing the topology on the hub (a new `[host.<name>]`, a flag flip),
`sotd topology sync` on every other box picks it up on its next launch — no
shortcut re-run needed for that. Re-run the shortcut installer only after
moving the repo or the launcher script:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\install-shortcut.ps1
```

That refreshes the desktop shortcut and repoints any existing Ship of Tools
taskbar pin to `scripts\launch-sot.ps1`.

## The daemon does not start on this host

If `journalctl --user -u sotd.service` says the start was skipped because a
condition check failed, the unit's host pin, `sotd.service.d/topology.conf`,
does not name this host: the installer pins the unit to the hosts
`~/.config/sot/hosts.toml` declares `daemon` or `frontend`, matched by kernel
hostname. Declare the host there (on the hub), then run
`sotd topology pin --dir ~/.config/systemd/user` and
`systemctl --user daemon-reload && systemctl --user start sotd` on it. With no
`hosts.toml` the pin names the one host whose install wrote it; run the
installer on the host that should run the daemon, or declare the hosts.

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
