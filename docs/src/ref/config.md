# Configuration Files

Five TOML files, each with one reader but `settings.toml`, which has
two: `settings.toml` (layout + terminal, read by the frontend, first
file found wins; the daemon reads its `[trust]` table too, see
[Install](../start/install.md)), `hosts.toml` (the declared topology —
hub, daemon and frontend hosts, monitor targets — read ONLY by
`sot_topology`/`sotd topology`; the frontend reads no config
file for hosts at all, see below), `keybindings.toml` (chords, read by
the frontend, same discovery), and the project chat's `chat.toml` and
`chat-secrets.toml` (read by the hub's daemon). Keybindings have their own
page — [Keybindings](keybindings.md) — so this page covers the other four.

`settings.toml` and `keybindings.toml` share one discovery pattern: the
frontend reads the first file it finds among an env-var path, a `.sot/` file
found by walking up from the frontend's working directory, and a per-user
file, and never reads the others; built-in defaults fill anything the file
leaves out. Missing or
out-of-range values fall back to the default rather than crashing the
chrome. `hosts.toml` has its own, simpler search order — see below.

## `settings.toml`

Frontend settings.

### [Discovery order](@id settings-discovery-order)

1. `$SOT_SETTINGS` — explicit path override.
2. `.sot/settings.toml`, found by walking upward from the frontend's working
   directory (not the session's project).
3. `settings.toml` in the per-user config folder: `$XDG_CONFIG_HOME/sot`, else `$HOME/.config/sot` on Linux and
   macOS, `%LOCALAPPDATA%\sot\config` on Windows (the folder the daemon reads too; the window ignores its
   `[trust]` table). This is the place for a person's own settings.
4. Built-in defaults.

`keybindings.toml` is found the same way, by its own variable `$SOT_KEYBINDINGS`.

A file kept in an old place is no longer read, with no message: one under `$HOME/.config/sot` while
`XDG_CONFIG_HOME` is set, one in a clone's `.sot/` on a Windows computer that starts the window outside that clone,
and one under `~/.config/sot` on a Windows window that had `HOME` set. Move it to the config folder, or set
`$SOT_SETTINGS` (`$SOT_KEYBINDINGS`) to its path.

The daemon reads the same file with a strict TOML parser, which refuses a value the window accepts, such as an
unquoted `[chat] name`; the daemon then records no folder trust (open item `SETTINGS-PARSERS-DISAGREE` in
`docs/defects.md`). Write every value as valid TOML.

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
| `widths` | comma-list of fractions | `0.167,0.333,0.5` | Fractional column widths, one per column; renormalized to sum to 1.0 on parse. A count that differs from `columns` warns and gives the columns an equal split. |
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
| `[chat] name` | string | *(unset)* | The author a chat post carries beside the app's name. Unset or blank, the Chat drawer does not post. |
| `[sessions] new_session_root` | path | *(fallback chain)* | Backend-host directory the new-session picker starts browsing from. Unset falls back to `$SOT_PROJECTS_ROOT`, `$SOT_REMOTE_HOME`, the daemon's default root, then `$HOME`. |

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

[chat]
name = "Ada"                    # the author a chat post carries; quote it
```

## `hosts.toml`

The declared topology: which host is the relay hub, which hosts run a
daemon, which run a frontend, and which hosts a `Ctrl+M` monitor drawer
samples. `sot_topology` (Rust) is the **one parser** for this
file — the daemon's `sotd topology plan|status|sync|relay-endpoint` CLI is
the one way anything reads it. Neither the frontend nor the PowerShell
launcher parses `hosts.toml` itself any more: the launcher runs
`sotd topology plan --self <host>` and renders its plain-line output into
`--dial <host>=<endpoint>` flags for the frontend (each a link the
frontend starts itself over one ssh login, or a local socket/pipe);
the frontend reads no config file for hosts at all
(see `--dial` under [CLI flags](../start/setup.md), and
`rust/topology/src/lib.rs`'s `plan` doc comment for the exact
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
Every other box reaches a daemon host through one ssh login into the hub, so only
the hub needs an `~/.ssh/config` entry for each daemon host, and every other
box needs one for the hub.

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `daemon` | bool | `false` | This host runs `sotd` and can be dialled. `sotd topology plan` emits a `dial` line for every daemon host: this box's own socket for itself, the hub's own relay socket for a remote host (on the hub only), and `ssh:<hub>` / `ssh:<hub>/<host>` everywhere else. |
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
host-c = "host-c"
```

`sotd topology plan --self laptop` on the laptop above renders as (see
`rust/topology/src/lib.rs`'s `plan` doc comment for the exact grammar):

```text
self laptop
hub myserver
relay-endpoint ssh:myserver
dial myserver ssh:myserver
dial otherbox ssh:myserver/otherbox
```

— which the launcher turns into
`--dial myserver=ssh:myserver --dial otherbox=ssh:myserver/otherbox` for
the frontend: one ssh login into the hub per host, no argument for the hub's own
daemon and `--host otherbox` to reach the hub's relay socket for the
other one. No port, no forward — read the actual endpoints from
`sotd topology plan`, never hardcode one.

## `chat.toml` and `chat-secrets.toml`

The project chat's configuration, two TOML files in the config folder (`~/.config/sot` on Linux and macOS,
`%LOCALAPPDATA%\sot\config` on Windows).

*Status: the daemon reads both files and connects to Slack with the two tokens. The Chat drawer shows the channel's held messages (see
[The project chat](../guide/chat.md)), and a person's post goes out under `[chat] name` of `settings.toml`; mail is not built.*

Only the hub's daemon reads them: the chat runs on the computer that `hosts.toml` (above) parses and names, by
`SOT_SELF_HOST` or its host name, as its `hub`. A computer with no `hosts.toml`, or another computer, runs no chat and
opens neither file. Both files are read once, when the daemon starts, and a mapped folder must have a row by then. After
editing either file, or adding a row for a mapped folder, restart the daemon (on Linux `systemctl --user restart sotd`;
rows keep running). A missing or refused `chat.toml`, or a missing or refused `chat-secrets.toml`, leaves the chat off.
One refused mapping, such as a folder with no row at start, turns the chat off for every project. The reason is in the
daemon's log, on the "project chat" line, as a fixed explanation with the mapping's number or the line, never a key's
value; `sotd log-path` prints the log's path. A start that Slack refuses or cannot reach (a rejected token, no network)
is logged as "project chat did not start" and stays failed until the daemon restarts.

Each file is a regular file of at most 64 KiB, and only the keys below are accepted.

`chat.toml`:

| Key | Form | Required | Meaning |
|-----|------|----------|---------|
| `workspace` | Slack id (a capital letter, then capitals and digits, 32 at most), not a name | yes | The one Slack workspace; it must equal `chat-secrets.toml`'s. A list or table naming a second one is refused. |
| `channels` | list of Slack ids | yes | The channels the chat may use. A repeated channel is refused. |
| `[[mapping]]` | table | one per project | `folder`, an absolute path of a folder a row had when the daemon started (not the window's default row, which runs no agent), and `channel`, one of `channels`. On Windows write the path in single quotes (`'C:\work\proj'`). A table may also repeat `workspace`, which must match. Two entries for one folder, or one channel for two folders, are refused. |
| `mail_after` | Slack timestamp (`<seconds>.<six digits>`), a quoted string | no | Read and checked; nothing uses it yet. *Mail delivery is unbuilt.* |

`chat-secrets.toml` holds `workspace`, `bot_token` (it starts `xoxb-`) and `app_token` (it starts `xapp-`), each token
letters, digits and hyphens up to 512 bytes. Keep the file private to your account; the
daemon never prints a token and masks one in its log, but it does not check the file's permissions.

## See also

- [Keybindings](keybindings.md) — chords and grammar (still repo-local, under `.sot/`).
