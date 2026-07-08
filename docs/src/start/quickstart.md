# Quickstart

The shortest path from nothing to a first useful session. For the full
picture behind any step here — requirements, roles, the from-source path,
per-machine onboarding, reconnect internals — see [Install Details](install.md)
and [First Session Tour](tour.md).

## 1. Install

The recommended path is the one the product is built on — an agent. Start a
Claude Code (or other) coding-agent session on the target machine and say:

```text
Install Ship of Tools: fetch https://raw.githubusercontent.com/kalidke/ship-of-tools/main/docs/INSTALL-AGENT.md and follow it.
```

It preflights the machine, asks you where things should run, drives the
installer, and verifies the result. Prefer to do it yourself? On Linux, one
command:

```bash
curl -fsSL https://raw.githubusercontent.com/kalidke/ship-of-tools/main/scripts/install.sh | bash -s -- --local
```

`--local` runs frontend and backend on this one box — the fastest way to a
working install. (Splitting frontend and backend across machines, Windows, and
building from source are all covered in [Install Details](install.md).)

## 2. Launch

The installer creates a `sot-launch` wrapper and a desktop entry. Run either
one. It starts the backend daemon if it isn't already running, opens the
frontend window, and connects them.

## 3. Attach / connect

The frontend and backend talk over a socket. A `--local` install runs both on
one box and connects to the backend's per-user socket through the generated
`sot-launch` wrapper — no SSH involved. Remote (`--backend`) installs forward a
local TCP port to that remote per-user socket over SSH, so the remote endpoint is
still owned by the selected Unix account. Either way, the default host and
config live at `~/.config/sot/hosts.toml`, and the frontend connects
automatically on launch. If the connection ever drops (laptop sleep, network
blip), press
**`F5`** to reconnect without losing session state.

## 4. Open a project

Press **`s`** for **Sessions mode**, then press **Enter** on the
**`[+ create new]`** row to open the directory picker. Choose a directory,
then press **Enter** to start a comm-aware agent session there (or
**Shift+Enter** for a bare shell/REPL with no agent). That directory is now
your active project.

## 5. Do something useful

A handful of keys get you moving immediately:

| Key | What it does |
|-----|---------------|
| `f` | **Files mode** — browse the project, preview renders as you move |
| `m` | **Modules mode** — structural view of the code (modules → functions → methods) |
| `Ctrl+J` | toggle the **REPL** drawer — a persistent Julia session |
| `Ctrl+T` | toggle the **Terminal** drawer — a local shell |
| `Ctrl+Arrow` | move focus between the four panes |
| `?` | the in-app help overlay — the live, authoritative keymap |

That's enough to look around, run code, and get help from inside the app.
For the rest of the panes, drawers, and modes, walk through
[First Session Tour](tour.md).

## 6. Updating

Re-run the install command from step 1 — it is also the updater (binaries,
repo checkout, and Julia environments move together). The app will also
notify you when a new release exists and stage the binaries itself; details
in [Updating](@ref updating).
