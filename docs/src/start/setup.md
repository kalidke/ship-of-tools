# Per-Machine Setup

Ship of Tools runs across machines and operating systems, and each one needs a small
amount of local state: the toolchains, a host registry, frontend settings, and a
launcher.

!!! note "Guided or manual"
    The guided flow ships as the **`/sot-setup` Claude Code skill** — available
    directly in any checkout (`.claude/skills/sot-setup/`) and installed
    user-level by `ShipTools.update_comm()`. There is no standalone `sot-setup`
    binary: in an agent session invoke `/sot-setup`; otherwise follow the
    checklist below manually. The Linux/macOS release installer
    (`scripts/install.sh`) automates much of this for packaged installs.

## Source Setup Checklist

The `sot-setup` flow is a one-shot, cross-OS onboarding for a Ship of Tools
machine (Windows, Linux, or macOS). Done manually, these are the steps:

1. **Install the toolchains.** Rust via [rustup](https://rustup.rs/) and Julia
   via [juliaup](https://github.com/JuliaLang/juliaup), where they are missing.
2. **Build the Rust workspace** (`rust/`) — the frontend and backend binaries.
3. **Ask a short Q&A** — your machine's role and, if it talks to a remote
   backend, that server's details (see below).
4. **Instantiate Julia environments**: the repo root, `core`, `julia/kernel`,
   `julia/repl`, and `julia/pluto`.
5. **Write `hosts.toml`** (the hub's canonical copy, or a `sotd topology
   sync --hub <alias>` fetch of it) **and `.sot/settings.toml`** from your
   answers.
6. **Install agent comm resources**:
   `julia --project=. -e 'using ShipTools; ShipTools.update_comm()'`.
7. **Create a launcher / shortcut** so you can start the app without typing the
   build paths.

On Windows frontend machines, run `scripts\install-shortcut.ps1` after
`hosts.toml` exists (`%LOCALAPPDATA%\sot\config\hosts.toml`, or `$SOT_HOSTS`
— see [Configuration Files](../ref/config.md)). Besides creating the desktop shortcut to
`scripts\launch-sot.ps1`, it sets the SoT icon (`logo.ico`) and stamps the
`ShipOfTools.Sot` AppUserModelID on the `.lnk`, so the running window merges
into the shortcut's taskbar button with the right icon (a hand-made shortcut
gets neither). Re-run it after editing host config or pinning to the taskbar
so the pin is repointed to the launcher.

## The cross-OS topology

The setup question that matters most is *which role this machine plays*. A
Linux frontend is first-class, same as Windows or a Mac; the split that
matters is backend vs. frontend, not one OS vs. another:

- **The backend runs on Linux.** The daemon and the Julia kernel run on a
  Linux server (for example `myserver`, `host-b`, or `host-c`). The daemon
  runs under the `systemd --user` unit `sotd.service`, and each session runs
  in its own capsule, so both survive SSH drops.
- **The frontend** — the native window that renders previews and owns the
  keyboard — runs on the machine in front of you: Linux, Windows or a Mac.
- **A per-user socket is SSH-forwarded** from the remote to the local
  machine; the frontend connects over that forward. Local and remote operation
  use the same protocol — only the transport differs.

## The machine-role question

The Q&A asks which of three roles the machine fills:

| Role | What runs here | Typical machine |
|------|----------------|-----------------|
| **frontend-local** | the frontend only; backend is on a remote | Windows laptop / workstation |
| **backend-remote** | the backend + kernel, reached over SSH | Linux server |
| **all-local** | frontend and backend on one machine | a single Linux box for offline work |

For **frontend-local**, the flow also records this machine as a `frontend`
host and the backend server as a `daemon` host in `hosts.toml` (the hub's
copy). The launcher derives everything else — the SSH forward's local
port, and the remote's socket path (`sotd session-socket-path sot`, always
queried, never configured) — from `sotd topology plan --self <host>` at
launch time; there is nothing else to fill in by hand.

## What gets written

### `hosts.toml` — the declared topology

One section per host, in a deliberately simple `key = value` format so it
needs no TOML library. `sot_protocol::topology` (Rust) is the one parser;
`sotd topology plan|status|sync|relay-endpoint` is the one way anything
reads it — see [Configuration Files](../ref/config.md) for the full
grammar. The in-app Hosts mode (hotkey `h`) lists every dialable host with
its live connected/unreachable status.

```toml
hub = "myserver"

[host.myserver]
daemon = true

[host.laptop]
frontend = true
```

Discovery order: `$SOT_HOSTS` → `<config dir>/hosts.toml`
(`~/.config/sot/hosts.toml` on Linux/macOS,
`%LOCALAPPDATA%\sot\config\hosts.toml` on Windows) — no repo-local `.sot/`
copy. The hub's copy is canonical; every other box's is a
`sotd topology sync --hub <alias>` fetch of it. Adding a new remote is one
`[host.<name>]` section on the hub, then `sync` on every box that dials it
— no launcher edit.

### `settings.toml` — frontend settings

The layout preset (see [`[layout]`](../ref/config.md#layout)). Any value missing or
out of range silently falls back to the built-in default; a malformed settings
file never crashes the chrome.

```toml
[layout]
preset = "auto"   # auto | ultrawide | laptop | portrait
```

The frontend reads the first of `$SOT_SETTINGS`, a `.sot/settings.toml`
found by walking up from the frontend's working directory, and
`$HOME/.config/sot/settings.toml`; built-in defaults fill the rest. Keybindings live in a
sibling `.sot/keybindings.toml` with the same discovery order.

The other sections are listed in [Configuration Files](../ref/config.md).

## After setup

Once the checklist is complete, the machine has a launcher and a valid host
configuration. Continue to [Going remote](remote.md) to start the app, or
take [Your first session](tour.md).
