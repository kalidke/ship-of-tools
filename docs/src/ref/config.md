# Configuration Files

The frontend reads three TOML files from `.sot/`, each with its own layered
discovery: `settings.toml` (layout + terminal), `hosts.toml` (the host registry),
and `keybindings.toml` (chords). Keybindings have their own page —
[Keybindings](keybindings.md) — so this page covers `settings.toml` and
`hosts.toml`.

All three share the same single-responsibility, layered-discovery pattern: a
project file in `.sot/`, overridable by an env var and a per-user file, with
built-in defaults underneath. Missing or out-of-range values fall back to the
default rather than crashing the chrome.

## `settings.toml`

Frontend layout and terminal settings.

### Discovery order

1. `$SOT_SETTINGS` — explicit path override.
2. `<repo-root>/.sot/settings.toml` — the project's settings.
3. `$HOME/.config/sot/settings.toml` — per-user settings.
4. Built-in defaults.

Any value that is missing or out of range silently falls back to its default —
the chrome never crashes on a malformed settings file.

### `[layout]`

Layout is **preset-based**, keyed by the primary monitor's aspect ratio — there
is no in-session reflow. The top-level `[layout]` table selects the active
preset; three sub-tables define the presets.

| Key | Type | Values | Default | Meaning |
|-----|------|--------|---------|---------|
| `preset` | string | `auto` · `ultrawide` · `laptop` · `portrait` | `auto` | Which preset to use. `auto` resolves by the primary monitor's aspect ratio at startup (`> 1.9` → ultrawide, `1.5–1.9` → laptop, `< 1.5` → portrait); the other three lock to that preset regardless of aspect. |

#### `[layout.ultrawide]` / `[layout.laptop]` / `[layout.portrait]`

One sub-table per aspect bucket, each defining its columns, their widths, and the
shared bottom drawer.

| Key | Type | Default (ultrawide) | Meaning |
|-----|------|---------------------|---------|
| `columns` | comma-list of slot names (`nav` · `preview` · `llm` · `repl`) | `nav,preview,llm` | Named column slots, left to right. |
| `widths` | comma-list of fractions | `0.167,0.333,0.5` | Fractional column widths; same length as `columns`, renormalised to sum to 1.0 on parse. |
| `drawer` | slot name or `none` | `repl` | Slot rendered in the shared bottom drawer when toggled open. |
| `drawer_height` | fraction `[0.10, 0.80]` | `0.35` | Drawer height as a fraction of window height when open; clamped to range. |

Laptop defaults to `0.18,0.32,0.50` widths with a `0.40` drawer; portrait drops
the `llm` column (`nav,preview` at `0.30,0.70`, `0.40` drawer). Unknown keys and
out-of-range values warn and fall back to the default.

### `[terminal]`

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `resume_command` | string | `claude --dangerously-skip-permissions --continue /sot-fe-session-start` | Command auto-run in the Terminal drawer when the supervisor respawns the frontend after a self-relaunch (`--relaunched`). It resumes the session without permission prompts. |

The `resume_command` is spelled out in full (not a personal shell shortcut) so it
is portable to any machine with `claude` on `PATH`. The trailing positional
`/sot-fe-session-start` is submitted as the resumed session's first
interactive turn, which re-arms the fast-comm inbox monitor and catches the
relaunch deaf-window gap — a resumed `--continue` is reactive and cannot self-arm
a monitor, so the frontend bootstraps it via this prompt. Iterate on the
bootstrap steps in that skill, not in this command.

### Example

```toml
[layout]
preset = "auto"   # auto | ultrawide | laptop | portrait

[layout.ultrawide]              # primary monitor aspect > 1.9
columns       = "nav,preview,llm"
widths        = "0.167,0.333,0.5"
drawer        = "repl"
drawer_height = "0.35"

[terminal]
resume_command = "claude --dangerously-skip-permissions --continue /sot-fe-session-start"
```

## `hosts.toml`

The host registry the in-app Hosts mode (hotkey `h`) lists and the PowerShell
launcher consumes.

The format is deliberately simple — a section per host, scalar `key = value`
lines — so the PowerShell launcher can parse it with a regex without pulling in a
TOML library. Values pass through **verbatim**: there is no TOML escape
processing, so Windows pipe paths use single backslashes.

### Discovery order

1. `$SOT_HOSTS` — explicit path override.
2. `<repo-root>/.sot/hosts.toml` — the project's host registry.
3. `$XDG_CONFIG_HOME/sot/hosts.toml` or `%APPDATA%\sot\hosts.toml` —
   per-user registry.

The launcher reads the single fixed path `<repo>/.sot/hosts.toml` (the
PowerShell side does not layer).

### Top-level

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `default_host` | string | *(none)* | Host used when no `last_host` has been picked yet. The launcher resolves the target as: env vars (`SOT_HOST` etc.) → persisted `last_host` → `default_host` → error ("no backend host configured") if none of those resolve. |

### `[host.<name>]`

One section per host. The frontend's Hosts mode lists every `[host.<name>]`
section; picking one writes `last_host` to `state-<hostname>.toml`, and the next
launch resolves that name back to its entry here.

| Key | Type | Meaning |
|-----|------|---------|
| `ssh_alias` | string | SSH alias for the remote host (an entry in your `~/.ssh/config`). |
| `remote_repo` | string | Absolute path to the project repo on the remote host. |
| `tcp_port` | integer | TCP port for the SSH-forwarded backend connection (each host gets a distinct port). |
| `remote_home` | string | Absolute home directory on the remote host. |
| `socket` | string | **Local-host form** — a named-pipe / socket path instead of SSH (no remote). On Windows this uses single backslashes, e.g. `\\.\pipe\sot-local`, because values are not escape-processed. |

A remote host sets `ssh_alias` / `remote_repo` / `tcp_port` (and usually
`remote_home`); a local backend on the same machine sets `socket` instead.

### `[monitor]`

The hosts sampled for the `Ctrl+M` server-monitor drawer. Each line is
`<display-name> = "<ssh-alias>"`.

| Form | Meaning |
|------|---------|
| `<name> = "<ssh-alias>"` | Sample this host in the monitor drawer. The host whose name (or alias) matches this machine's hostname is sampled **locally** (no SSH); the rest are sampled over `ssh <alias>`. |

`nvidia-smi` and `/proc` are world-readable, so no `sudo` or special privileges
are needed on any monitored host. Remove a line to stop monitoring that host.

### Example

```toml
default_host = "myserver"

[host.myserver]
ssh_alias = "myserver"
remote_repo = "/home/me/ship-of-tools"
tcp_port = 18743
remote_home = "/home/me"

# A local backend on the same machine (no SSH):
# [host.local]
# socket = "\\.\pipe\sot-local"

[monitor]
myserver = "myserver"
host-b = "host-b"
host-c = "host-c"
```

## See also

- [Keybindings](keybindings.md) — the third `.sot/` file, chords and grammar.
