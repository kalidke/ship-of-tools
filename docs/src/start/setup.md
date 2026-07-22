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
   On a machine that runs the **backend**, also install **tmux** (the daemon
   hosts the LLM pane in a tmux session) — **tmux ≥ 3.2** for full in-pane
   `SOT_*` awareness; older tmux runs but degrades that awareness.
2. **Build the Rust workspace** (`rust/`) — the frontend and backend binaries.
3. **Ask a short Q&A** — your machine's role and, if it talks to a remote
   backend, that server's details (see below).
4. **Instantiate Julia environments**: the repo root, `core`, `julia/kernel`,
   `julia/repl`, and `julia/pluto`.
5. **Write `.sot/hosts.toml` and `.sot/settings.toml`** from your answers.
6. **Install agent comm resources**:
   `julia --project=. -e 'using ShipTools; ShipTools.update_comm()'`.
7. **Create a launcher / shortcut** so you can start the app without typing the
   build paths.

On Windows frontend machines, run `scripts\install-shortcut.ps1` after
`.sot\hosts.toml` exists. Besides creating the desktop shortcut to
`scripts\launch-sot.ps1`, it sets the SoT icon (`logo.ico`) and stamps the
`ShipOfTools.Sot` AppUserModelID on the `.lnk`, so the running window merges
into the shortcut's taskbar button with the right icon (a hand-made shortcut
gets neither). Re-run it after editing host config or pinning to the taskbar
so the pin is repointed to the launcher.

## The cross-OS topology

The setup question that matters most is *which role this machine plays*, because
The Ship of Tools deployment is split across operating systems by design:

- **Windows is the display surface.** The frontend — the native window that
  renders previews and owns the keyboard — runs on the Windows machine.
- **Linux remotes run the backend.** The daemon and the Julia kernel run on a
  Linux server (for example `myserver`, `host-b`, or `host-c`), supervised
  inside a `tmux` session so it survives SSH drops.
- **A per-session socket is SSH-forwarded** from the remote to the local
  machine; the frontend connects over that forward. Local and remote operation
  use the same protocol — only the transport differs.

This is the canonical "Windows local · Linux remote-in-tmux" topology.

## The machine-role question

The Q&A asks which of three roles the machine fills:

| Role | What runs here | Typical machine |
|------|----------------|-----------------|
| **frontend-local** | the frontend only; backend is on a remote | Windows laptop / workstation |
| **backend-remote** | the backend + kernel, reached over SSH | Linux server in `tmux` |
| **all-local** | frontend and backend on one machine | a single Linux or macOS box for offline work |

For **frontend-local**, the flow also collects the backend server's connection
details (its SSH alias, the repo path on the remote, and the local forwarded
port) and writes them as a host entry. The remote side of that forward is the
backend user's per-user Unix socket, discovered from `sotd session-socket-path
sot` unless `remote_socket` is set explicitly.

## What gets written

The answers land in two files under `.sot/`, both with layered discovery so a
machine-specific or environment override can take precedence:

### `hosts.toml` — the host registry

A section per backend host, in a deliberately simple `key = value` format so both
the Rust frontend and the PowerShell launcher can parse it without a TOML
library. The in-app Hosts mode (hotkey `h`) lists every `[host.<name>]` and lets
you pick the target; the choice persists, and the launcher and reconnect both
route to it.

```toml
default_host = "myserver"

[host.myserver]
ssh_alias   = "myserver"
remote_repo = "/home/me/projects/ship-of-tools"
tcp_port    = 18743  # local side of the SSH forward
# remote_socket = "/run/user/<uid>/sot/sessions/sot.sock"
```

Discovery order: `$SOT_HOSTS` → `<repo-root>/.sot/hosts.toml` →
`$XDG_CONFIG_HOME/sot/hosts.toml` (or `%APPDATA%\sot\hosts.toml`). Adding a
new remote is one entry here — no launcher edit.

### `settings.toml` — frontend settings

The layout preset and the Terminal drawer's resume command (used by the
self-relaunch loop — see [Running & Relaunch](running.md)). Any value missing or
out of range silently falls back to the built-in default; a malformed settings
file never crashes the chrome.

```toml
[layout]
preset = "auto"   # auto | ultrawide | laptop | portrait

[terminal]
resume_command = "claude --dangerously-skip-permissions --continue /sot-fe-session-start"
```

Discovery order: `$SOT_SETTINGS` → `<repo-root>/.sot/settings.toml` →
`$HOME/.config/sot/settings.toml` → built-in defaults. Keybindings live in a
sibling `.sot/keybindings.toml` with the same layered pattern.

## After setup

Once the checklist is complete, the machine has a launcher and a valid host
configuration. Continue to [Running & Relaunch](running.md) to start the app, or
take [A Guided Tour](tour.md) of a first session.
