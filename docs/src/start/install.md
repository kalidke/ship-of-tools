# Installation

There are three ways to install Ship of Tools. Published GitHub Releases with
prebuilt artifacts exist (Linux x86_64, Windows x86_64, macOS aarch64), so the
default path is [from release artifacts](@ref install-release).

- **Agent-driven (recommended)** — start a coding-agent session (Claude Code,
  Codex, …) on the target machine and tell it:

  ```text
  Install Ship of Tools: fetch https://raw.githubusercontent.com/kalidke/ship-of-tools/main/docs/INSTALL-AGENT.md and follow it.
  ```

  The agent preflights, verifies release assets match the platform, falls back
  to the source path when they do not, and proves the result answers before
  declaring success. The runbook it follows is `docs/INSTALL-AGENT.md` in this
  repo.

- **[From release artifacts](@ref install-release)** — prebuilt binaries via
  the `install.sh` installer. No Rust toolchain is needed on this path.
- **[From source](@ref install-source)** — clone, build the Rust workspace,
  instantiate the Julia environments. This is the path for developing it, and
  [Per-Machine Setup](setup.md) adds the current manual config and launcher
  checklist on top of it.

## [From release artifacts (Linux/macOS)](@id install-release)

`scripts/install.sh` downloads the latest GitHub Release, verifies checksums,
and sets everything up under `~/.local/share/sot` by default:

```bash
curl -fsSL https://raw.githubusercontent.com/kalidke/ship-of-tools/main/scripts/install.sh | bash -s -- --local
```

With no role flag and an interactive TTY, the installer asks which role this
machine should play. In `curl | bash` or other non-interactive runs, pass the
role explicitly:

| Flag | Role |
|------|------|
| `--local` | all-in-one: frontend + backend on this box |
| `--backend <ssh-alias>` | frontend here, backend on a remote host over SSH forwarding — the flag names where the *backend* lives |
| `--be-only` | headless backend only (servers, canary boxes) |

Plus `--version vX.Y.Z` to pin a release (default: latest), `--prefix <dir>` to
relocate the install, `--port <n>` for the daemon port, and `--no-service` on
backend roles when a shared-home deployment should not get a persistent user
systemd unit.

What the installer lays out under the prefix:

| Path | Contents |
|------|----------|
| `$PREFIX/bin/sot` | prebuilt frontend binary |
| `$PREFIX/bin/sotd` | prebuilt backend daemon |
| `$PREFIX/updates` | staging area used by the release/update path |
| `$PREFIX/repo/current` | blobless partial clone of the repo at the selected release tag |

The checkout is part of the installed product. `install.sh` runs
`git clone --filter=blob:none --branch vX.Y.Z`: full history is present for
blame, but only the selected tag's tree is downloaded. Per the ADR 0030
amendment, this checkout is the resource tree and the manual. The backend
exports `SOT_MANUAL` pointing at it, and runtime resources resolve through
`resource_dir` to `$PREFIX/repo/current` before older layouts. A legacy
`$PREFIX/julia/current` symlink is still created only so older pre-clone
binaries that look for the retired bundle layout can find the checkout.

Julia is no longer distributed as a curated bundle. The installer uses
[juliaup](https://github.com/JuliaLang/juliaup) if Julia is missing, then runs
`ShipTools.update_comm()` from the checked-out tag so `~/.sot-comm/bin` and the
Claude/Codex skills match the installed version. For roles that run a backend
on this machine (`--local` and `--be-only`), it also prepares the Julia
sidecars inside the checkout:

```bash
$PREFIX/repo/current/julia/kernel
$PREFIX/repo/current/julia/repl
$PREFIX/repo/current/julia/pluto  # instantiated, precompiled, and load-tested
```

`--backend <ssh-alias>` is a frontend-only install on the local machine, so it
only uses Julia locally for `ShipTools.update_comm()`; it does not instantiate
the kernel/repl/pluto sidecars. The remote backend machine should be installed
as a backend role.

Connection behavior is role-specific:

- `--local` runs frontend and backend on one box over the backend user's
  per-user socket. The generated `sot-launch` wrapper starts `sot` with
  `--socket <path>`; there is no SSH-to-localhost requirement.
- `--backend <ssh-alias>` creates SSH local forwards to the remote backend,
  forwarding the local frontend port to the remote user's per-user backend
  socket.
- `--be-only` installs the headless backend. By default it installs and starts
  the `systemd --user` `sotd.service`; `--no-service` skips that unit so shared
  `$HOME` machines can supervise `sotd` themselves.

Config is written under `~/.config/sot`:

| File | Behavior |
|------|----------|
| `hosts.toml` | created for the selected role if missing; role changes back up the existing file to `hosts.toml.bak` before rewriting |
| `settings.toml` | created only if missing |

## [Updating](@id updating)

After release artifacts exist, two mechanisms complement rather than compete:

| Mechanism | What it does | What you do |
|-----------|--------------|-------------|
| **Built-in update check** | The backend checks GitHub for new releases (daily and on demand), **notifies the frontend**, and downloads + verifies + stages the new *binaries*. A staged binary is applied on the next launch (with a `.prev` rollback copy). | Nothing — you'll see the notice; relaunch when convenient. |
| **Re-running the installer** | The **complete** update in one command: new binaries *and* the repo checkout moved to the new tag *and* Julia envs re-instantiated, atomically. | `curl -fsSL …/install.sh \| bash -s -- <your-role>` (same command as install; add `--version vX.Y.Z` to pin). |

**When in doubt: re-run the installer.** It is idempotent, safe, and moves
everything together. The built-in check keeps you *informed* and keeps the
binaries fresh; full checkout+env automation of the apply step is on the
roadmap.

On update the installer swaps binaries while keeping `.prev` copies, fetches
tags in `$PREFIX/repo/current`, checks out the requested tag, **refuses to
move a dirty checkout** (commit/stash/revert first — the checkout is
read-only by convention), and verifies `HEAD` equals the tag's recorded
commit, so a moved tag or half-checkout fails at update time, not at first
use.

## Requirements



- **linux-x86_64** or **macos-aarch64** release artifacts.
- **git** for the release-tag checkout.
- **curl** and **tar** for the installer. If using the `$GITHUB_TOKEN` path
  instead of `gh`, `jq` is also required.
- **Julia ≥ 1.12** for agent comm resource installation. The installer uses
  juliaup to install it when missing.
- **tmux** on any host that runs the backend (`--local`, `--be-only`) — the
  daemon hosts the LLM pane in a tmux session. The installer preflights it:
  **absent is fatal**. **tmux < 3.2 is a graceful degrade, not an error** — the
  daemon version-gates `new-session -e` (older tmux rejected it, which once drove
  a respawn storm), so the backend runs but the pane's in-session `SOT_*`
  awareness is best-effort; put a **tmux ≥ 3.2** earlier on the daemon's `PATH`
  (e.g. `~/.local/bin`) for full awareness. Frontend-only `--backend <alias>`
  hosts don't need tmux.
- Frontend roles need **glibc ≥ 2.35** (Ubuntu 22.04 or newer). The backend
  binary is static musl and runs on any distro — `--be-only` has no glibc
  floor.
- `--backend <ssh-alias>` needs key-based SSH to the remote backend host.
- No GitHub auth is required (the repo is public). An authenticated `gh` or
  a set `$GITHUB_TOKEN` is *honored* to avoid the unauthenticated API rate
  limit (60 requests/hour per IP) — relevant only if you install repeatedly.

On **Windows** there is no `install.ps1` and no shipped `sot-setup` command yet.
`scripts/install.sh` exits with a Windows-specific message, so install from
source via the manual checklist in [Per-Machine Setup](setup.md), or use a
Windows release zip only after one exists. On **macOS aarch64** the bash
installer is wired, but support is still experimental and there is no launchd
service wiring; local roles start `sotd` on demand.

Development machines don't use the release installer at all — they run from a
checkout via [Per-Machine Setup](setup.md).

## [From source](@id install-source)

Ship of Tools is a Rust + Julia project: the frontend and backend are Rust binaries,
the kernel and plugins are Julia. Installing it means building the Rust
workspace once and instantiating a handful of Julia environments.

This section is the manual build path. Machine-specific config and launchers are
covered by [Per-Machine Setup](setup.md); that page is currently a manual
source-checkout checklist, not a shipped `sot-setup` command.

### Prerequisites

| Tool | Version | Why |
|------|---------|-----|
| Rust toolchain | current stable | frontend, backend, protocol crates |
| Julia | ≥ 1.12 | kernel, `ConceptExplorerCore`, plugins |
| `git` | any | clone the repo |
| `tmux` | ≥ 3.2 (backend hosts) | the daemon hosts the LLM pane in a tmux session; < 3.2 runs but degrades in-pane `SOT_*` awareness. Frontend-only machines don't need it. |

Install Rust with [rustup](https://rustup.rs/) and Julia with
[juliaup](https://github.com/JuliaLang/juliaup). The `julia = "1.12"` compat
floor is set product-wide (`Project.toml`, `core/Project.toml`, the kernel and
plugin environments).

### Clone

```bash
git clone https://github.com/kalidke/ship-of-tools
cd ship-of-tools
```

### Build the Rust workspace

The Rust workspace lives under `rust/` and has three members — `protocol`
(shared line-protocol types), `backend` (the daemon), and `frontend` (the native
window). One command builds all of them:

```bash
cargo build --release --manifest-path rust/Cargo.toml
```

This produces the two binaries the launcher runs:

| Binary | Path | Role |
|--------|------|------|
| `sot` | `rust/target/release/` | native window, chrome, previews |
| `sotd` | `rust/target/release/` | project state, supervision, orchestrator |

The first release build pulls the full rendering stack (`winit`, `wgpu`,
`cosmic-text`, `glyphon`, `resvg`) and compiles for a while; subsequent builds
are incremental.

### Instantiate the Julia environments

The repo is a set of nested Julia environments, not one flat project. The
minimum source setup instantiates the umbrella environment, `core`, the kernel,
the REPL shim, and the Pluto sidecar. The umbrella environment at the repo root pins
project-level dependencies (e.g. `CairoMakie` for plotting):

```bash
julia --project=. -e 'using Pkg; Pkg.instantiate()'
```

Then instantiate the core library, kernel, REPL shim, and Pluto sidecar:

```bash
julia --project=core            -e 'using Pkg; Pkg.instantiate()'
julia --project=julia/kernel    -e 'using Pkg; Pkg.instantiate()'
julia --project=julia/repl      -e 'using Pkg; Pkg.instantiate()'
julia --project=julia/pluto     -e 'using Pkg; Pkg.instantiate()'
```

`core/` is `ConceptExplorerCore` — the abstract types and dispatch contract that
make up the [extension ABI](../extend/abi.md). The standard plugins live under
`julia/plugins/*`; instantiate any plugin environment you intend to load the
same way, e.g.:

```bash
julia --project=julia/plugins/julia-source -e 'using Pkg; Pkg.instantiate()'
```

See the *Repository layout* section of the project `CLAUDE.md` for the full tree
of where each environment lives.

### Verify the build

A quick check that the binaries built and the core library loads:

```bash
ls rust/target/release/sot rust/target/release/sotd
julia --project=core -e 'using ConceptExplorerCore; println("core OK")'
```

## Next steps

- [Per-Machine Setup](setup.md) — the current manual source-checkout checklist:
  toolchains, config files, comm skills, and a launcher.
- [Running & Relaunch](running.md) — launching the frontend, the Terminal
  drawer, and the self-relaunch loop.
- [A Guided Tour](tour.md) — a first session, mode by mode.
