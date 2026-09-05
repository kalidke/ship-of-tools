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
| `resume_command` | string | `claude --permission-mode auto --continue /sot-fe-session-start` | Command auto-run in the Terminal drawer when the supervisor respawns the frontend after a self-relaunch (`--relaunched`). It resumes the session without permission prompts. |

The `resume_command` is spelled out in full (not a personal shell shortcut) so it
is portable to any machine with `claude` on `PATH`. The trailing positional
`/sot-fe-session-start` is submitted as the resumed session's first
interactive turn, which re-arms the fast-comm inbox monitor and catches the
relaunch deaf-window gap — a resumed `--continue` is reactive and cannot self-arm
a monitor, so the frontend bootstraps it via this prompt. Iterate on the
bootstrap steps in that skill, not in this command.

### `[gpu]`

| Key | Type | Values | Default | Meaning |
|-----|------|--------|---------|---------|
| `power_preference` | string | `low` · `high` | `low` | Which adapter to request from wgpu. `low` prefers the integrated GPU; `high` prefers the discrete one. wgpu's own spellings (`low_power`, `high_performance`) and `integrated`/`discrete` are accepted; case is ignored. |

The frontend renders glyph quads and image blits — a 2D workload an integrated
GPU handles comfortably — so it asks for the **low-power adapter by default**. On
a hybrid-graphics laptop, requesting the discrete GPU keeps it awake for the
entire session (measured ~11 W on an otherwise idle RTX 4070): an active surface
prevents the dGPU from power-gating. Set `high` if you are on a desktop with a
real GPU, or if the integrated adapter renders incorrectly.

On single-adapter machines the key is a **no-op** — with only an integrated GPU
present, `high` already resolves to it.

> **Takes effect on the next frontend start.** The preference binds once, when
> the adapter and surface are created at startup, so editing this key mid-session
> changes nothing until the frontend restarts.

### `[display]`

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `fullscreen_vsync_pin` | bool | `false` | While fullscreen, keep requesting a redraw every vsync instead of falling back to the on-demand idle tick. |

In borderless fullscreen, DWM composition disengages and the panel's refresh
follows the frontend's present cadence directly. The efficient on-demand idle
path presents ~1 frame/sec, which drives a VRR/adaptive-sync OLED panel into
the 1–10 Hz band where low-framerate compensation makes brightness pump
visibly. With the pin on, fullscreen instead requests a redraw every vsync so
the panel stays pinned at its native refresh; this costs continuous GPU while
fullscreen.

There is no VRR/adaptive-sync detection API worth trusting, so this is a
setting rather than a heuristic, and it defaults to `false`: most panels are
fixed-refresh and the pin only burns power for no visible benefit — measured
25.5 points of one core and 8.5 points of iGPU 3D continuously at idle in
fullscreen (measured on one laptop). Set `true` on a VRR/adaptive-sync OLED
panel that pumps brightness in borderless fullscreen.

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
resume_command = "claude --permission-mode auto --continue /sot-fe-session-start"

[gpu]
power_preference = "low"        # low (integrated, default) | high (discrete)

[display]
fullscreen_vsync_pin = false    # default false | true on a VRR/OLED panel
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
| `default_host` | string | *(none)* | The one remote host the CLI `--socket`/`--tcp` override targets, and whose `tcp_port` may be omitted (falls back to `SOT_TCP_PORT`/18743). The launcher resolves it as: env vars (`SOT_HOST` etc.) → `default_host` → error ("no backend host configured") if neither resolves — but it is no longer the only tunnel the launcher opens (ADR 0042 L2b design E, below). (Pre-ADR-0042-L2a this also fell back to a persisted `last_host` the launcher read from frontend state; that step is deleted — see the note below.) |

### `[host.<name>]`

One section per host. The frontend opens one connection per section that
has a reachable endpoint (a `socket` or a `tcp_port`), local-first then
file order — every configured host is live at once, not a single picked
target. The Hosts mode lists every entry with its live
connected/unreachable status; Enter moves the Sessions-mode cursor to
that host's node (it doesn't pick a target for the launcher — see ADR
0042 slice L2a; ADR 0015's `last_host`-based single-host picker is
superseded).

**`local` needs no section at all.** The frontend always holds a
connection to this machine's own daemon, and every launch mode ensures
that daemon is running (ADR 0042 L2b designs B and D) — no `hosts.toml`
entry required. A `[host.local]` section is only for overriding its
derived `socket` (see that key below); every other key on it is ignored,
since local is never SSH-tunneled.

| Key | Type | Meaning |
|-----|------|---------|
| `ssh_alias` | string | SSH alias for the remote host (an entry in your `~/.ssh/config`). Presence of this key is what makes a section a tunnel target (ADR 0042 L2b design E) — the launcher opens one SSH forward per section that has it, not just `default_host`'s. |
| `remote_repo` | string | Absolute path to the project repo on the remote host. |
| `tcp_port` | integer | Local TCP port for the SSH-forwarded backend connection. The remote side should terminate at the per-user backend socket. **Required** for every remote except `default_host`, which may omit it (falls back to `SOT_TCP_PORT`/18743 for compatibility); a remote missing it gets no tunnel — the launcher logs one line naming the host and moves on, it does not fail the whole launch. |
| `remote_socket` | string | Optional remote Unix socket path for the backend control channel. If omitted, launchers query `sotd session-socket-path sot` on the remote host. |
| `remote_home` | string | Absolute home directory on the remote host. |
| `socket` | string | **Local-host form** — a named-pipe / socket path instead of SSH (no remote). Only meaningful on `[host.local]`, to override the derived pipe path (`sot_protocol::session_socket_path("local")`, ADR 0042 L2b design A) — every other host resolves its endpoint from `tcp_port` instead. On Windows this uses single backslashes, e.g. `\\.\pipe\sot-local`, because values are not escape-processed. |

A remote host sets `ssh_alias` / `remote_repo` / `tcp_port` (and usually
`remote_home`); `remote_socket` is optional and normally discovered. `local`
needs nothing at all unless overriding its derived `socket`.

### Backend tmux socket

`sotd` normally puts workspace tmux sessions on its private per-user tmux
socket. For a one-time migration to existing `sot-be-*` sessions on another
same-user tmux server, set `SOT_TMUX_SOCK` in the backend environment. `sotd
tmux-socket-path` prints the effective path, including this override.

`sotd` also needs **tmux ≥ 3.2** to stamp the pane's `SOT_*` awareness env via
`new-session -e`. On older tmux it degrades gracefully — omitting `-e` and
falling back to a best-effort `set-environment` — rather than failing, so the
backend still runs; put a tmux ≥ 3.2 earlier on the daemon's `PATH` for full
in-pane awareness.

### File-watcher budget

`sotd` watches each workspace's tree to refresh previews on disk changes,
registering one (non-recursive) inotify watch per kept directory. It skips
build/VCS directories and never crosses a filesystem boundary (so a project root
over a mounted data share doesn't pull the share in), and it caps the watches
per workspace at `min(8192, ¼ of fs.inotify.max_user_watches)` so it can't
exhaust the OS watch table. Override the cap with `SOT_WATCH_BUDGET=<n>` in the
backend environment. Past the cap, deeper subtrees stop auto-refreshing;
navigation still refreshes previews reactively.

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
# remote_socket = "/run/user/<uid>/sot/sessions/sot.sock"
remote_home = "/home/me"

# A second remote -- its OWN tunnel, opened alongside myserver's, not
# instead of it (ADR 0042 L2b design E). tcp_port is required here (this
# isn't default_host).
[host.otherbox]
ssh_alias = "otherbox"
remote_repo = "/home/me/ship-of-tools"
tcp_port = 18744

# "local" needs no section -- see above. Only to override its derived pipe:
# [host.local]
# socket = "\\.\pipe\sot-local"

[monitor]
myserver = "myserver"
host-b = "host-b"
host-c = "host-c"
```

## See also

- [Keybindings](keybindings.md) — the third `.sot/` file, chords and grammar.
