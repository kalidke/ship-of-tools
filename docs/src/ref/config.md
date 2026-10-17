# Configuration Files

`settings.toml` holds the frontend's settings (layout, font, terminal and the rest); the frontend reads it when it
starts. `hosts.toml` declares the hosts and is read by `sotd`; the window also reads it for the hub's name and for
whether this box owns updates, but its connections come only from `--dial` flags. Chords live in a third file,
`keybindings.toml`, described on its own page: [Keybindings](keybindings.md). A missing or out-of-range value falls back
to its default rather than crashing the chrome.

## [Where the files are](@id config-file-locations)

The window and `sotd` do not look in the same places.

- **The window** reads `settings.toml` and `keybindings.toml` from the first of: `$SOT_SETTINGS` (or
  `$SOT_KEYBINDINGS`); `.sot/<file>` found by walking upward from the window's working directory; `$HOME/.config/sot/<file>`.
  It never consults `XDG_CONFIG_HOME`, and where `HOME` is unset (a normal Windows login) the last step finds nothing.
  On Windows the launcher sets `$SOT_SETTINGS` to the clone's `.sot\settings.toml`.
- **`sotd`** reads `hosts.toml` (unless `$SOT_HOSTS` names a file) and the `[trust]` table of `settings.toml` from its
  config directory: `$XDG_CONFIG_HOME/sot` when that is set, else `~/.config/sot`; on Windows
  `%LOCALAPPDATA%\sot\config`.

So on Linux and macOS with `XDG_CONFIG_HOME` set, the daemon's `settings.toml` is not the window's. On Windows they are
two files: set `[trust] root_prefix` in the daemon's, not in the clone-local file the window reads.

## `settings.toml`

Frontend settings.

### Discovery order

1. `$SOT_SETTINGS` — explicit path override.
2. `.sot/settings.toml`, found by walking upward from the frontend's working
   directory (not the session's project).
3. `$HOME/.config/sot/settings.toml` — per-user settings.
4. Built-in defaults.

A value that is missing or does not parse keeps its default, and the reason is written to the window's log. A preset's
`widths` has one entry per entry in its `columns`.

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
| `widths` | comma-list of fractions | `0.167,0.333,0.5` | Fractional column widths, one per column; renormalized to sum to 1.0 on parse. A count that differs from `columns` warns and gives the columns an equal split. |
| `drawer` | slot name or `none` | `repl` | Slot rendered in the shared bottom drawer when toggled open. |
| `drawer_height` | fraction `[0.10, 0.80]` | `0.35` | Drawer height as a fraction of window height when open; clamped to range. |

Laptop defaults to `0.18,0.32,0.50` widths with a `0.40` drawer; portrait drops
the `llm` column (`nav,preview` at `0.30,0.70`, `0.40` drawer). Unknown keys are ignored with
a logged warning, and a value that does not parse keeps the default.

### Other sections

| Section / key | Type | Default | Meaning |
|---------------|------|---------|---------|
| `[terminal] shell` | string | *(auto)* | Shell the Terminal drawer spawns. Unset resolves per platform: `$SHELL`, then `/bin/bash`, `/bin/sh` on Unix; `pwsh.exe`, `powershell.exe`, `cmd.exe` on Windows. |
| `[repl] auto_open_drawer_on_run` | bool | `true` | Running a `.jl` file from the navigation tree with `r`/`R` opens the REPL drawer if it is closed, so the run's output is visible. Navigation keeps focus. |
| `[font] scale` | number `[0.5, 3.0]` | *(by monitor width)* | Text-size multiplier at startup. A zoom you saved with the font-scale keys wins over this key, and `--font-scale` on the command line overrides both. Unset, wide monitors start with larger text. |
| `[nav] spill_ms` | integer, ms | `2000` | While the navigation cursor is moving, a row too long for the column floats its full text over the preview pane's left edge, and the text vanishes this many milliseconds after the last move. `0` turns it off. |
| `[trust] root_prefix` | absolute path | *(none)* | Read by the daemon from its own `settings.toml` (see [Where the files are](@ref config-file-locations)); the frontend ignores it. When it starts an agent in a project folder under this prefix, it marks Claude Code's trust dialog for that folder as answered. Set it with `sotd trust declare <absolute-prefix>`. |
| `[downloads] dir` | path | *(OS download dir)* | Local directory `d` (download) writes to. Empty means the OS download directory. |
| `[sessions] new_session_root` | path | *(fallback chain)* | Backend-host directory the new-session picker starts browsing from. Unset falls back to `$SOT_PROJECTS_ROOT`. For a remote host the picker then tries `$SOT_REMOTE_HOME`, the daemon's default root, then the window's `$HOME`. For the local host the configured root counts only if it exists on this disk; otherwise the picker starts at the daemon's default root, then `$HOME`. |
| `[drawer] attach_only` | bool | `false` | Windows only: the Terminal drawer attaches to a running capsule instead of spawning a local shell. Dormant: nothing starts a drawer capsule to attach to, so leave it `false`. Read when the drawer is created; takes a frontend restart. |

### `[gpu]`

| Key | Type | Values | Default | Meaning |
|-----|------|--------|---------|---------|
| `power_preference` | string | `low` · `high` | `low` | Which adapter to request from wgpu. `low` prefers the integrated GPU; `high` prefers the discrete one. wgpu's own spellings (`low_power`, `high_performance`) and `integrated`/`discrete` are accepted; case is ignored. |

The frontend renders glyph quads and image blits — a 2D workload an integrated
GPU handles comfortably — so it asks for the **low-power adapter by default**. On
a hybrid-graphics laptop, requesting the discrete GPU keeps it awake for the
entire session: an active surface
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
fixed-refresh and the pin only burns power for no visible benefit. Set `true` on a VRR/adaptive-sync OLED
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
daemon, which run a frontend, and which hosts the Monitor drawer
(`drawer.monitor`) samples. `sot_protocol::topology` (Rust) is the **one parser** for this
file — the daemon's `sotd topology plan|status|sync|relay-endpoint` CLI is
the one way anything reads it. Neither the frontend nor the PowerShell
launcher parses `hosts.toml` itself: the launcher runs
`sotd topology plan --self <host>` and renders its plain-line output into
`--dial <host>=<endpoint>` flags for the frontend (each an ssh child the
frontend spawns itself, or a local socket/pipe). `sot --dial` takes one
connection per flag and may repeat; `sot --help` lists it. The window does load `hosts.toml` through the same parser, but only for the hub's name (the Monitor drawer's
subscription) and to decide whether this box owns updates; which daemons it connects to comes only from `--dial`
(`rust/protocol/src/topology/mod.rs`'s `plan` doc comment has the exact line-oriented contract).

The format is deliberately simple — a section per host, scalar
`key = value` lines — so it needs no TOML library.

### Discovery order

The one search order, used everywhere this file is read:

1. `$SOT_HOSTS` — explicit path override (tests, scratch daemons).
2. `<config dir>/hosts.toml` — `$XDG_CONFIG_HOME/sot/hosts.toml` when `XDG_CONFIG_HOME` is set, else
   `~/.config/sot/hosts.toml` on Linux/macOS; `%LOCALAPPDATA%\sot\config\hosts.toml` on Windows.

There is no repo-local `.sot/hosts.toml` layer: the hub's own copy is canonical, and every other box's copy is a
`sotd topology sync --hub <alias>` fetch of it.

### Top-level

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `hub` | string | *(required)* | The one host running the relay daemon every other box's comm handles register on and dial through. Exactly one `hub` key, naming a listed host. |

### `[host.<name>]`

One section per host. `<name>` is the plain host name (`[a-z0-9][a-z0-9._-]*`)
that box's own `host_name()` resolves to — it doubles as its SSH alias.
Every other box reaches a daemon host through an ssh child into the hub, so only
the hub needs an `~/.ssh/config` entry for each daemon host, and every other
box needs one for the hub.

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `daemon` | bool | `false` | This host runs `sotd` and can be dialled. `sotd topology plan` emits a `dial` line for every daemon host: this box's own socket for itself, the hub's own relay socket for a remote host (on the hub only), and `ssh:<hub>` / `ssh:<hub>/<host>` everywhere else. |
| `frontend` | bool | `false` | This host runs a frontend and its launcher, and so its own private `sotd`. |

A host can be `daemon = true`, `frontend = true`, or both (a workstation running its own daemon *and*
driving a local frontend). A host declared with neither runs no `sotd`: `sotd.service` is pinned only to hosts that declare
`daemon` or `frontend`, and the updater arms nothing there. The hub itself needs `daemon = true` too — it
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

The hosts sampled for the Monitor drawer (`drawer.monitor`). **Hub-scoped:**
declared once, but only the declared `hub` executes it — every non-hub daemon
samples just the host it runs on, regardless of what this table says, so the
roster's ssh aliases need only resolve **on the hub**. The drawer itself
subscribes to the hub, not to whichever host the frontend happens to be
navigating. Each line is `<display-name> = "<ssh-alias>"`. A monitor target is not a `[host.*]`: it can be any box you can
reach with `ssh`, and it needs no section above.

| Form | Meaning |
|------|---------|
| `<name> = "<ssh-alias>"` | Sample this host in the monitor drawer. The host whose name (or alias) matches the hub's hostname is sampled **locally** (no SSH); the rest are sampled over `ssh <alias>` from the hub. |

`nvidia-smi` and `/proc` are world-readable, so no `sudo` or special privileges
are needed on any monitored host. Remove a line to stop monitoring that host.
The sampler reads `/proc`, so a daemon samples its own host only on Linux; on
macOS and Windows the local host is left out of the drawer.

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
host-c = "host-c"    # sampled over ssh; not a [host.*] above
```

`sotd topology plan --self laptop` on the laptop above renders as (see
`rust/protocol/src/topology/mod.rs`'s `plan` doc comment for the exact grammar):

```text
self laptop
hub myserver
relay-endpoint ssh:myserver
dial myserver ssh:myserver
dial otherbox ssh:myserver/otherbox
```

— which the launcher turns into
`--dial myserver=ssh:myserver --dial otherbox=ssh:myserver/otherbox` for
the frontend: an ssh child into the hub, no argument for the hub's own
daemon and `--host otherbox` to reach the hub's relay socket for the
other one. No port, no forward — read the actual endpoints from
`sotd topology plan`, never hardcode one.

## See also

- [Keybindings](keybindings.md) — chords and grammar; the file is found as `settings.toml` is: `$SOT_KEYBINDINGS`, then `.sot/keybindings.toml`
  up from the working directory, then `~/.config/sot/keybindings.toml`.
