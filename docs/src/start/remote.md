# Going remote

The **backend daemon** runs where your GPUs and data are, the **frontend**
window runs on the machine in front of you, and the two talk over SSH:
the launcher forwards the backend's socket, and browser pages ride that same
connection. If the frontend closes, the agents and the REPL keep
running on the server.

```@raw html
<DemoLoop name="sessions" caption="Session rows and their work-state colours." />
```

## 1. Install the backend on the server

On the server (Linux), install the headless backend role:

```bash
curl -fsSL https://raw.githubusercontent.com/kalidke/ship-of-tools/main/scripts/install.sh | bash -s -- --be-only
```

By default this installs and starts the `systemd --user` unit `sotd.service`,
so the daemon outlives your SSH login. (`--no-service` skips the unit when a
shared-home deployment supervises `sotd` itself; see
[Install details](install.md).)

Every role needs Julia 1.12 or newer (installed with juliaup if missing)
and gets the skills and hooks; see
[What the installer changes](@ref install-footprint).

If the server's home directory is itself on NFS, point the daemon's state
root at local disk first — see
[A session will not start on a shared home](../guide/troubleshooting.md#A-session-will-not-start-on-a-shared-home).

## 2. Install the frontend on your machine

On the laptop or desktop, install the frontend role and name the server by its
SSH alias (the one you use for `ssh myserver`):

```bash
curl -fsSL https://raw.githubusercontent.com/kalidke/ship-of-tools/main/scripts/install.sh | bash -s -- --backend myserver
```

On Windows, use the release zip and `scripts\install-shortcut.ps1`, as
described under [Install](install.md).

## 3. Launch

Run `sot-launch` (or the desktop entry). The launcher opens the SSH forwards
to the backend, applies any staged update, and starts the frontend. The daemon
on the server is started once and survives across launches. On Linux and
macOS the SSH forwards run in the background and outlive the window; a later
launch reuses them.

With more than one server, the frontend connects to every backend host listed
in `hosts.toml`. One machine, the *hub*, holds the canonical copy, and
`sotd topology sync` fetches it on every launch; see
[Hosts mode](../guide/modes.md#hosts-mode) and
[Configuration files](../ref/config.md).

## Reconnecting

The connection can drop (laptop sleep, network change, SSH timeout) without
losing anything on the server. The frontend reconnects on its own; **`F5`**
retries at once. Agent sessions do not depend on the connection: each runs
under its own supervisor on the server (see
[Sessions and persistence](../concepts/sessions.md)).

## Browser pages

`o` opens HTML, video and Pluto pages in your own browser, `Shift+W` opens a
file's built documentation site, and `wglshow` opens an interactive WGLMakie
figure. These work the same with a remote backend and need no extra SSH
setup. How the traffic is carried is described under
[Interactive figures in the browser](../guide/repl.md#Interactive-figures-in-the-browser).

## Next steps

- [A second frontend](../guide/second-frontend.md) — attach a desktop and a
  laptop to the same backend.
- [Enrolling a host](../guide/enrolling.md) — make a server reachable from
  every frontend.
- [Troubleshooting](../guide/troubleshooting.md) — when a launch or a browser
  page does not come up.
