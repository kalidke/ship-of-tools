# Configuration Files

Three TOML files, each read by a different piece and none of them by more
than one parser: `settings.toml` (layout + terminal, read by the frontend,
first file found wins), `hosts.toml` (the declared topology —
hub, daemon and frontend hosts, monitor targets — read ONLY by
`sot_protocol::topology`/`sotd topology`; the frontend reads no config
file for hosts at all, see below), and `keybindings.toml` (chords, read by
the frontend, same discovery). Keybindings have their own page —
[Keybindings](keybindings.md) — so this page covers `settings.toml` and
`hosts.toml`.

`settings.toml` and `keybindings.toml` share one discovery pattern: the
frontend reads the first file it finds among an env-var path, a `.sot/` file
found by walking up from the frontend's working directory, and a per-user
file, and never reads the others; built-in defaults fill anything the file
leaves out. Missing or
out-of-range values fall back to the default rather than crashing the
chrome. `hosts.toml` has its own, simpler search order — see below.

## `settings.toml`

Frontend settings.

### Discovery order

1. `$SOT_SETTINGS` — explicit path override.
2. `.sot/settings.toml`, found by walking upward from the frontend's working
   directory (not the session's project).
3. `$HOME/.config/sot/settings.toml` — per-user settings.
4. Built-in defaults.

Any value that is missing or out of range silently falls back to its default —
the chrome never crashes on a malformed settings file.

### [`[layout]`](@id layout)

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

### Other sections

| Section / key | Type | Default | Meaning |
|---------------|------|---------|---------|
| `[terminal] shell` | string | *(auto)* | Shell the Terminal drawer spawns. Unset resolves per platform: `$SHELL`, then `/bin/bash`, `/bin/sh` on Unix; `pwsh.exe`, `powershell.exe`, `cmd.exe` on Windows. |
| `[downloads] dir` | path | *(OS download dir)* | Local directory `d` (download) writes to. Empty means the OS download directory. |
| `[sessions] new_session_root` | path | *(fallback chain)* | Backend-host directory the new-session picker starts browsing from. Unset falls back to `$SOT_PROJECTS_ROOT`, `$SOT_REMOTE_HOME`, the daemon's default root, then `$HOME`. |
| `[drawer] attach_only` | bool | `false` | Windows only: the Terminal drawer attaches to a running capsule instead of spawning a local shell. Read when the drawer is created; takes a frontend restart. |

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

[gpu]
power_preference = "low"        # low (integrated, default) | high (discrete)

[display]
fullscreen_vsync_pin = false    # default false | true on a VRR/OLED panel
```

## `hosts.toml`

The declared topology: which host is the relay hub, which hosts run a
daemon, which run a frontend, and which hosts a `Ctrl+M` monitor drawer
samples. `sot_protocol::topology` (Rust) is the **one parser** for this
file — the daemon's `sotd topology plan|status|sync|relay-endpoint` CLI is
the one way anything reads it. Neither the frontend nor the PowerShell
launcher parses `hosts.toml` itself any more: the launcher runs
`sotd topology plan --self <host>` and renders its plain-line output into
SSH tunnels, `--dial <host>=<endpoint>` flags for the frontend, and
`SOT_RELAY_ENDPOINT`; the frontend reads no config file for hosts at all
(see `--dial` under [CLI flags](../start/setup.md), and
`rust/protocol/src/topology.rs`'s `plan` doc comment for the exact
line-oriented contract).

The format is deliberately simple — a section per host, scalar
`key = value` lines — so it needs no TOML library. Values pass through
**verbatim**: there is no TOML escape processing, so Windows pipe paths
use single backslashes.

### Discovery order

The one search order, used everywhere this file is read:

1. `$SOT_HOSTS` — explicit path override (tests, scratch daemons).
2. `<config dir>/hosts.toml` — `~/.config/sot/hosts.toml` on Linux/macOS,
   `%LOCALAPPDATA%\sot\config\hosts.toml` on Windows.

There is no repo-local `.sot/hosts.toml` layer any more: the hub's own copy
is canonical, and every other box's copy is a `sotd topology sync --hub
<alias>` fetch of it (see below).

### Top-level

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `hub` | string | *(required)* | The one host running the relay daemon every other box's comm handles register on and dial through. Exactly one `hub` key, naming a listed host. |

### `[host.<name>]`

One section per host. `<name>` is the plain host name (`[a-z0-9][a-z0-9._-]*`)
that box's own `host_name()` resolves to — it doubles as its SSH alias.
Every other box reaches a daemon host through SSH forwards to the hub, so only
the hub needs an `~/.ssh/config` entry for each daemon host, and every other
box needs one for the hub.

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `daemon` | bool | `false` | This host runs `sotd` and can be dialled. `sotd topology plan` emits a `dial` line for every daemon host, and on every box but the hub a `tunnel` line for each daemon host other than itself. |
| `frontend` | bool | `false` | This host runs a frontend and its launcher. |

A host can be `daemon = true`, `frontend = true`, neither (the section is
otherwise pointless), or both (a workstation running its own daemon *and*
driving a local frontend). The hub itself needs `daemon = true` too — it
is still a listed host, just the one every other daemon host's relay
handles register on.

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

The hosts sampled for the `Ctrl+M` server-monitor drawer. **Hub-scoped:**
declared once, but only the declared `hub` executes it — every non-hub daemon
samples just the host it runs on, regardless of what this table says, so the
roster's ssh aliases need only resolve **on the hub**. The drawer itself
subscribes to the hub, not to whichever host the frontend happens to be
navigating. Each line is `<display-name> = "<ssh-alias>"`.

| Form | Meaning |
|------|---------|
| `<name> = "<ssh-alias>"` | Sample this host in the monitor drawer. The host whose name (or alias) matches the hub's hostname is sampled **locally** (no SSH); the rest are sampled over `ssh <alias>` from the hub. |

`nvidia-smi` and `/proc` are world-readable, so no `sudo` or special privileges
are needed on any monitored host. Remove a line to stop monitoring that host.

### Example

```toml
hub = "myserver"

[host.myserver]
daemon = true

# A frontend box.
[host.laptop]
frontend = true

# A second daemon host; its SSH alias on the hub is `otherbox`.
[host.otherbox]
daemon = true

[monitor]
myserver = "myserver"
otherbox = "otherbox"
host-c = "host-c"
```

`sotd topology plan --self laptop` on the laptop above renders as (see
`rust/protocol/src/topology.rs`'s `plan` doc comment for the exact grammar):

```text
self laptop
hub myserver
relay-endpoint tcp:127.0.0.1:<base>
dial myserver tcp:127.0.0.1:<base>
dial otherbox tcp:127.0.0.1:<base+1>
tunnel myserver <base>
tunnel otherbox <base+1>
```

— which the launcher turns into two SSH forwards to the hub and
`--dial myserver=tcp:127.0.0.1:<base> --dial otherbox=tcp:127.0.0.1:<base+1>`
for the frontend. `<base>` is per OS user (`18743` plus a hash of the user
name modulo 100), so two users on one box do not collide; read the actual
ports from `sotd topology plan`, never hardcode them.

## See also

- [Keybindings](keybindings.md) — chords and grammar (still repo-local, under `.sot/`).
