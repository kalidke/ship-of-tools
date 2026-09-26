# Installation

There are three ways to install Ship of Tools. GitHub Releases ship prebuilt
artifacts for Linux x86_64, Windows x86_64 and macOS aarch64, so the default
path is [from release artifacts](@ref install-release).

- **Agent-driven** — start a Claude Code or Codex session on the target
  machine and tell it:

  ```text
  Install Ship of Tools: fetch https://raw.githubusercontent.com/kalidke/ship-of-tools/main/docs/INSTALL-AGENT.md and follow it.
  ```

  The agent checks the machine, confirms the release assets match the
  platform, falls back to the source path when they do not, and checks that
  the result answers before it reports success. The runbook it follows is
  `docs/INSTALL-AGENT.md` in this repo.

- **[From release artifacts](@ref install-release)** — prebuilt binaries via
  the `install.sh` installer. No Rust toolchain is needed.
- **[From source](@ref install-source)** — clone, build the Rust workspace,
  instantiate the Julia environments. This is the path for developing Ship of
  Tools; [Per-machine setup](setup.md) adds the config and launcher checklist.

## [From release artifacts (Linux/macOS)](@id install-release)

`scripts/install.sh` downloads a GitHub Release, verifies its checksums, and
sets everything up under `~/.local/share/sot` by default:

```bash
curl -fsSL https://raw.githubusercontent.com/kalidke/ship-of-tools/main/scripts/install.sh | bash -s -- --local
```

The script fetched from `main` only resolves the latest release and runs that
release's own `install.sh`, so what gets installed is always a tagged release.

With no role flag and an interactive terminal, the installer asks which role
this machine should play. In `curl | bash` or other non-interactive runs, pass
the role explicitly:

| Flag | Role |
|------|------|
| `--local` | all-in-one: frontend and backend on this machine |
| `--backend <ssh-alias>` | frontend here, backend on a remote host over SSH forwarding; the flag names where the *backend* lives |
| `--be-only` | headless backend only (servers) |

Plus `--version vX.Y.Z` to pin a release (default: latest), `--prefix <dir>` to
relocate the install, `--port <n>` for the frontend's local TCP port of the SSH
forward (remote role only), `--no-service` on backend roles when a
shared-home deployment should not get a persistent user systemd unit,
`--hub <ssh-alias>` to copy the hub's `~/.config/sot/hosts.toml` to this
machine first (for a machine that does not share the hub's home directory),
and `--force-role-change` (see the next section).

If `~/.config/sot/hosts.toml` already names this host, that entry decides
which parts are installed and enabled here, and a role flag on the command
line is ignored.

The checkout under the prefix is part of the installed product: a
`git clone --filter=blob:none` of the selected tag, so history is available but
only that tag's files are downloaded. The runtime resources and the in-app
manual resolve from it. For roles that run a backend on this machine (`--local`
and `--be-only`), the installer instantiates the Julia environments inside the
checkout (`julia/kernel`, `julia/repl`, `julia/pluto`). `--backend <ssh-alias>`
is a frontend-only install and uses Julia locally only to install the agent
skills; install the remote machine as a backend role. Julia runs the
installer's comm step on every role, which is why it is needed here too; the
skills and hooks it installs are inert unless a session runs on this
machine.

Connection behaviour is role-specific:

- `--local` runs frontend and backend on one machine over the backend user's
  per-user socket. There is no SSH-to-localhost requirement.
- `--backend <ssh-alias>` creates SSH local forwards to the remote user's
  per-user backend socket.
- `--be-only` installs the headless backend and, unless `--no-service`, runs it
  as the `systemd --user` unit `sotd.service`.

### Re-running on a machine that already has an install

A re-run is an upgrade. The installer refuses to overwrite files that belong
to another install: the `sotd.service` unit, the `sot-launch` wrapper, the
desktop entry and the macOS app. If one of these already exists under a
different prefix (a source checkout, another `--prefix`, an earlier trial
install), the installer stops before writing anything and names the file and
its owner. `--force-role-change` overrides that. A file whose owner cannot be
determined (an unrecognized `sot-launch` shape) is refused with no override;
move it aside and re-run.

## [What the installer changes](@id install-footprint)

Everything below is written under your home directory, with one exception:
on Linux backend roles the installer runs `loginctl enable-linger` for your
user, a systemd setting kept outside your home (it continues without it if
that call is refused). Nothing needs root. The default prefix is
`~/.local/share/sot` (`--prefix` moves it). There is no uninstall script;
[Uninstall](@ref install-uninstall) lists the commands to remove each item by hand.

Roles: *all* is every install; *backend* is `--local` and `--be-only`;
*frontend* is `--local` and `--backend`.

| Path | What |
|------|------|
| `~/.local/share/sot/bin/` | *all* · `sot` (frontend), `sotd` (backend daemon), `sot-capsule` (session supervisor), `sot-apply` (update applier) |
| `~/.local/share/sot/repo/` | *all* · the release checkout, one directory per version under `repo/versions/` (the current and previous one are kept); `repo/current` links to the installed one |
| `~/.local/share/sot/julia/current` | *all* · a link to the same checkout |
| `~/.local/share/sot/updates/` | *all* · staging area for downloaded updates |
| `~/.local/share/sot/install.json` | *all* · install manifest: prefix, version, and which parts (daemon, frontend) were installed |
| `~/.config/sot/settings.toml` | *all* · a one-line stub, written only if missing |
| `~/.config/sot/hosts.toml` | *all* · written only with `--hub`, as a copy fetched from the hub; otherwise only read |
| `~/.juliaup/` | *all* · Julia 1.12, installed with the juliaup installer, only if no Julia 1.12 or newer is on `PATH` (an existing juliaup gets the 1.12 channel added instead) |
| the first depot in `DEPOT_PATH` (usually `~/.julia`) | *backend* · packages for the kernel, REPL and Pluto environments |
| `~/.config/systemd/user/sotd.service` | *backend, Linux, unless `--no-service`* · the backend service, enabled and started; `loginctl enable-linger` is also run so it survives logout. The service sources `~/.bashrc` before it starts `sotd`, so the backend and its sessions inherit your shell profile's exports. |
| `~/.local/bin/sot-launch` | *frontend* · the launcher wrapper |
| `~/.local/share/applications/ship-of-tools.desktop` | *frontend, Linux* · the desktop entry |
| `~/.local/share/icons/hicolor/` | *frontend, Linux* · the icon, `256x256/apps/ship-of-tools.png` |
| `~/Applications/Ship of Tools.app` | *frontend, macOS* · app bundle |

The installer then runs `ShipTools.update_comm()` from the checkout, which
installs the agent integration:

| Path | What |
|------|------|
| `~/.sot-comm/` | the session messaging scripts (`bin/`) and their runtime state |
| `~/.local/bin/` | `ccb` (Claude Code launcher), `ccx` (Codex launcher), `sot-fe`, `show-result`, `sot-gh-auth` |
| `~/.claude/skills/` | the Claude Code skills: `julia-repl`, `show-result`, `sot-comm`, `sot-session-start`, `sot-status`, `sitrep`, `worktree`, `project-log`, `sot-install`, `sot-setup`, `sot-statusline-setup`, `sot-gh-auth` |
| `~/.claude/settings.json` | hook entries for `UserPromptSubmit`, `PreToolUse` (on `AskUserQuestion`), `PostToolUse`, `Stop` and `SessionStart` (on `compact` and `clear`), each calling a `comm-*.sh` script in `~/.sot-comm/bin`; merged in without removing existing hooks |
| `~/.codex/skills/` | the Codex skills `sot-comm` and `sot-session-start` |
| `~/.codex/AGENTS.md` | installed only if no `AGENTS.md` is there |
| `~/.agents/plugins/sot-comm/`, `~/.agents/plugins/marketplace.json` | the Codex work-state hook plugin; written whether or not Codex is installed, and registered with `codex plugin add sot-comm@sot-local` when it is |

`~/.claude` and `~/.codex` follow `CLAUDE_CONFIG_DIR` and `CODEX_HOME` when
those are set; `~/.sot-comm` follows `SOT_COMM_HOME`. The other paths do not
move: `~/.local/bin`, `~/.agents/plugins` and everything in the first table
are always written in your home directory. Setting those three variables is
therefore not an isolated try-out; for that, install under a separate user
account or in a virtual machine.

The hooks do nothing in an agent session that has not joined the Ship of
Tools session registry. Each hook first looks the session up in the session registry
(`~/.sot-comm/registry.json`) and exits without writing anything when it is not
there, which is the case for a `claude` or `codex` you start yourself in a
terminal. Setting `SOT_COMM_HOOKS=off` turns the Claude Code work-state hooks
off entirely.

The skills are likewise available in a plain terminal `claude`, but their
commands need a Ship of Tools session: `julia-repl` and `show-result` reach
`sot-fe`, which has nothing to talk to outside one.

A skill directory with the same name as one of the skills above is
overwritten: each shipped file replaces the file of the same name, and any
other file in that directory is deleted. No backup is made. Every re-run of
the installer, including each update, repeats this.
The five skills without an `sot-` prefix are the likeliest to collide with
one of your own: `julia-repl`, `show-result`, `sitrep`, `worktree` and
`project-log`.

The installer itself edits no shell profile. If no Julia 1.12 or newer is
found and juliaup is not installed, it runs the juliaup installer, which adds
its `PATH` block to your shell startup files; `juliaup self uninstall` removes
it.

### How agent sessions are launched

Sessions you create in the window start their agent with these permission
settings:

- **Claude Code**: `claude --permission-mode auto`.
- **Codex**: `codex --dangerously-bypass-approvals-and-sandbox
  --dangerously-bypass-hook-trust`, through the `ccx` launcher, which also
  marks the session's directory as trusted in `~/.codex/config.toml`.

Claude Code's auto mode is not a bypass. It runs without routine permission
prompts, but a separate classifier model reviews each action before it runs and
blocks anything that goes beyond your request, targets infrastructure it does
not recognise, or looks driven by hostile content; your own `ask` rules still
prompt. See Claude Code's
[permission modes](https://code.claude.com/docs/en/permission-modes).
Codex has no equivalent reviewed mode here: its flags turn approvals and the
sandbox off.

These flags are fixed: no setting or environment variable changes them. The
Claude Code flags are built into the daemon, and the Codex flags are in the
`ccx` script, which every update overwrites. Ship of Tools adds no permission
layer of its own on top. A `claude` or `codex` you start yourself, outside
Ship of Tools, uses whatever flags you give it. See
[Agent sessions](../guide/orchestrator.md).

## [Uninstall](@id install-uninstall)

There is no uninstall script. To remove an install made with the default
prefix, stop the service and delete what the installer wrote:

```bash
systemctl --user disable --now sotd.service
rm -f ~/.config/systemd/user/sotd.service
systemctl --user daemon-reload
loginctl disable-linger "$USER"   # skip if something else needs linger
rm -rf ~/.local/share/sot ~/.config/sot ~/.sot-comm
rm -f ~/.local/bin/sot-launch ~/.local/bin/ccb ~/.local/bin/ccx ~/.local/bin/sot-fe \
      ~/.local/bin/show-result ~/.local/bin/sot-gh-auth
rm -f ~/.local/share/applications/ship-of-tools.desktop \
      ~/.local/share/icons/hicolor/256x256/apps/ship-of-tools.png
rm -rf ~/.claude/skills/{julia-repl,show-result,sot-comm,sot-session-start,sot-status,\
sitrep,worktree,project-log,sot-install,sot-setup,sot-statusline-setup,sot-gh-auth}
rm -rf ~/.codex/skills/{sot-comm,sot-session-start}
rm -rf ~/.agents/plugins/sot-comm
```

These commands use the default locations. If `CLAUDE_CONFIG_DIR`,
`CODEX_HOME` or `SOT_COMM_HOME` is set, use those directories instead of
`~/.claude`, `~/.codex` and `~/.sot-comm`. Deleting `~/.config/sot` also
deletes your `hosts.toml` and any settings you edited; keep a copy if you want
them.

Then, by hand:

- remove the hook entries that call `~/.sot-comm/bin/comm-*.sh` from
  `~/.claude/settings.json`;
- if Codex is installed, remove the `sot-comm@sot-local` plugin (it is listed
  by `codex plugin list`);
- remove the `sot-local` entry from `~/.agents/plugins/marketplace.json`, or
  delete the file if nothing else is listed in it;
- remove the `[projects."<dir>"]` blocks with `trust_level = "trusted"` that
  `ccx` appended to `~/.codex/config.toml`, one per directory a Codex session
  ran in;
- delete `~/.codex/AGENTS.md` if the installer created it;
- on macOS, delete `~/Applications/Ship of Tools.app`;
- `~/.juliaup` and the Julia depot are ordinary Julia installs; keep them or
  remove them with juliaup (`juliaup self uninstall` also removes the `PATH`
  block it added to your shell startup files).

Sessions also keep runtime state under `~/.local/state` (or
`$XDG_STATE_HOME`); see [Sessions and persistence](../concepts/sessions.md).

## Updating

Re-running the install command is also the updater. The built-in update
check, pinning a version, and rolling back are in
[Updating and rollback](../guide/updating.md).

## Requirements

### [Platforms](@id platforms)

- **Linux x86_64**: all roles.
- **Windows**: frontend only, against a Linux backend.
- **macOS (Apple Silicon)**: experimental; the tested use is a frontend
  against a Linux backend (`--backend`) — the other roles install but agent
  sessions on a Mac backend are not supported yet.

- **linux-x86_64** or **macos-aarch64** release artifacts (a
  **windows-x86_64** zip also ships for the Windows frontend path below).
- **git**, **curl** and **tar**.
- A **coding agent** (Claude Code or Codex), installed and logged in on every
  machine that runs sessions. The installer does not install one.
- **Julia 1.12** or newer. The installer installs it with juliaup when
  missing; an existing juliaup only gets the 1.12 channel added, not made the
  default, so if your default channel is older, run `juliaup default 1.12`
  yourself (or set `SOT_JULIA_BIN`).
- **node/npm**, optional, on the backend machine: typeset math in markdown
  previews renders in a long-lived node process, whose dependencies the
  installer fetches with `npm ci`. Without it the installer warns and
  continues, and math renders as raw LaTeX. To add math later, install node
  and re-run the installer (or `npm ci` in
  `<checkout>/rust/backend/sidecars/mathjax`).
- **poppler-utils** (`pdftoppm`, `pdfinfo`) and **ffmpeg**, optional, on the
  backend machine: PDF pages and video poster frames in previews. The
  installer does not check for them; without them the preview shows a note
  naming the missing tool.
- Frontend roles need **glibc 2.35** or newer (Ubuntu 22.04 or newer) and a
  desktop session (X11 or Wayland): the frontend is a native window. It
  renders with `wgpu` through Vulkan on Linux, Metal on macOS and DirectX 12
  or Vulkan on Windows, so a Linux frontend needs a working Vulkan driver. The backend binary is
  static musl and runs on any distribution; `--be-only` has no glibc floor.
- `--backend <ssh-alias>` needs key-based SSH to the remote backend host.
- No GitHub authentication: the repo is public and releases download from
  fixed URLs.
- A home directory on NFS (or another remote filesystem) needs a systemd
  drop-in pointing the daemon's state root at local disk before sessions
  will start; see
  [A session will not start on a shared home](../guide/troubleshooting.md#A-session-will-not-start-on-a-shared-home).

On **Windows** there is no `install.ps1` yet (`scripts/install.sh` exits with
a Windows-specific message), but no Rust toolchain is needed: the release
ships `sot-<ver>-windows-x86_64.zip`. Extract `sot.exe` into
`%LOCALAPPDATA%\sot\bin`, clone the repo for the launcher scripts and config,
and wire the shortcut with `scripts\install-shortcut.ps1`, which points the
desktop shortcut (and any taskbar pin) at `scripts\launch-sot.ps1`. Do not
point a shortcut at bare `sot.exe`, which skips the launcher's update and
relaunch handling. The step-by-step walkthrough is
[INSTALL-AGENT.md §2b](https://github.com/kalidke/ship-of-tools/blob/main/docs/INSTALL-AGENT.md)
(written for a coding agent, equally followable by hand).

On **macOS aarch64** the bash installer works, but there is no launchd
service; local roles start `sotd` on demand. See [Platforms](@ref platforms)
for what is and is not supported on a Mac.

## [From source](@id install-source)

Ship of Tools is a Rust and Julia project: the frontend and backend are Rust
binaries, the kernel and plugins are Julia. Installing from source means
building the Rust workspace once and instantiating a handful of Julia
environments. Machine-specific config and launchers are covered by
[Per-machine setup](setup.md); run that page's checklist by hand or let the
`/sot-setup` Claude Code skill drive it.

### Prerequisites

| Tool | Version | Why |
|------|---------|-----|
| Rust toolchain | current stable | frontend, backend, protocol crates |
| Julia | 1.12 or newer | kernel, `ConceptExplorerCore`, plugins |
| `git` | any | clone the repo |

Install Rust with [rustup](https://rustup.rs/) and Julia with
[juliaup](https://github.com/JuliaLang/juliaup).

### Clone

```bash
git clone https://github.com/kalidke/ship-of-tools
cd ship-of-tools
```

### Build the Rust workspace

The Rust workspace lives under `rust/` and has six members: `protocol`
(shared line-protocol types), `backend` (the daemon), `frontend` (the native
window), `updater`, `log` and `vt100` (a terminal-emulator fork). One command
builds all of them:

```bash
cargo build --release --manifest-path rust/Cargo.toml
```

This produces the binaries the launcher runs:

| Binary | Path | Role |
|--------|------|------|
| `sot` | `rust/target/release/` | native window, chrome, previews |
| `sotd` | `rust/target/release/` | project state, supervision, agent sessions |
| `sot-capsule` | `rust/target/release/` | `sotd`'s capsule-runtime pair, one per session |

The first release build compiles the full rendering stack (`winit`, `wgpu`,
`cosmic-text`, `glyphon`, `resvg`) and takes a while; later builds are
incremental.

### Instantiate the Julia environments

The repo is a set of nested Julia environments. The umbrella environment at
the repo root pins project-level dependencies such as `CairoMakie`:

```bash
julia --project=. -e 'using Pkg; Pkg.instantiate()'
```

Then the core library, kernel, REPL shim, and Pluto sidecar:

```bash
julia --project=core            -e 'using Pkg; Pkg.instantiate()'
julia --project=julia/kernel    -e 'using Pkg; Pkg.instantiate()'
julia --project=julia/repl      -e 'using Pkg; Pkg.instantiate()'
julia --project=julia/pluto     -e 'using Pkg; Pkg.instantiate()'
```

`core/` is `ConceptExplorerCore`, the abstract types and dispatch contract that
make up the [extension ABI](../extend/abi.md). The standard plugins live under
`julia/plugins/*`; instantiate any plugin environment you intend to load the
same way, e.g.:

```bash
julia --project=julia/plugins/julia-source -e 'using Pkg; Pkg.instantiate()'
```

### Verify the build

```bash
ls rust/target/release/sot rust/target/release/sotd rust/target/release/sot-capsule
julia --project=core -e 'using ConceptExplorerCore; println("core OK")'
```

## Next steps

- [Quickstart](quickstart.md) — launch and start a first agent session.
- [Going remote](remote.md) — a backend on a server, the window on your
  laptop.
- [Per-machine setup](setup.md) — the source-checkout checklist: toolchains,
  config files, comm skills, and a launcher.
