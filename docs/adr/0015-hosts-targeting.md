# ADR 0015: In-app host targeting via `hosts.toml` + `Mode::Hosts`

**Status:** Accepted
**Date:** 2026-05-19

## Context

Until now the launcher hard-coded the target host (myhost) and the cross-machine endpoint (TCP port 18743). Switching to a different remote (host-b, host-c, a future workstation) required editing `launch-devenv.ps1` or setting env vars by hand. The user wants a host picker visible inside the running app, with the chosen target persisted so subsequent launches and post-disconnect reconnects target the same host without re-asking.

Two constraints shaped the design:

- **The user does not need active in-session host switching.** Switching hosts means cold-starting a new daemon's session state; that's heavyweight and expected to be rare ("we don't really need to switch hosts actively"). Picking a host once and having it persist is enough.
- **Reconnection (tunnel-supervisor restarts after laptop wake, wifi flap, etc.) must target the *same* host the user is on**, not silently bounce to a different one.

## Decision

Pick the target host once, persist it, and have every mechanism that re-opens the connection — launcher startup, tunnel supervisor restart, frontend transport reconnect — route to the same persisted host. No live in-session swap; switching hosts is "pick + Ctrl+Q + relaunch."

### Three artefacts

1. **`.devenv/hosts.toml`** — section-per-host registry, deliberately simple format so both the Rust frontend *and* the PowerShell launcher can parse it without a TOML library.

   ```toml
   default_host = "myhost"

   [host.myhost]
   ssh_alias   = "myhost"
   remote_repo = "/home/user/projects/MyPackage.jl"
   tcp_port    = 18743

   [host.host-b]
   ssh_alias   = "host-b"
   remote_repo = "/home/user/projects/MyPackage.jl"
   tcp_port    = 18744
   ```

   Layered discovery (Rust): `$SOT_HOSTS` → `<cwd>/.devenv/hosts.toml` → `$XDG_CONFIG_HOME/devenv/hosts.toml` → `$HOME/.config/devenv/hosts.toml` → `%APPDATA%/devenv/hosts.toml`. Launcher reads `<repo>/.devenv/hosts.toml` (single fixed path; layering on the PowerShell side isn't needed for the v1).

   Values pass through verbatim — no TOML escape processing — so Windows pipe paths use single backslashes (`\\.\pipe\sot-local`). Documented in-file.

2. **`Mode::Hosts` in the frontend (hotkey `h`)** — populates the nav tree from `hosts_config` at startup (cached at `State::new` from `hosts::load()`). Each row shows `<name> · <endpoint>` with `[current]` and `[default]` badges. Enter on a row writes `last_host` to `state-<hostname>.toml` and surfaces `host = <name> · Ctrl+Q + relaunch to apply` in the status line. No backend round-trip — `populate_hosts_tree` rebuilds entirely from the cached config.

3. **Launcher consumes `last_host`** — reads `state-<hostname>.toml` for `last_host`, looks up the matching entry in `hosts.toml`, sets the existing `SOT_HOST` / `SOT_REMOTE_REPO` / `SOT_TCP_PORT` env vars (which already drive the tunnel + remote spawn). Env-set values still win, then state-toml's `last_host`, then `hosts.toml::default_host`, then the hardcoded `myhost` fallback. The tunnel supervisor and the frontend's transport reconnect both target whatever endpoint the launcher chose at startup.

### What's persisted across host switches

The frontend's per-workspace `WorkspaceUiSnapshot` + `WorkspaceReplSnapshot` are keyed by workspace slug. Different hosts' workspaces are different slugs (per daemon's registry), so the snapshots coexist cleanly. Session-id resume from `state.toml` is per-daemon, so the *first* connect to a new host is a cold start; subsequent reconnects to the same host replay missed events as usual.

### What's not done

- **Live in-session swap** — would require the transport task to drop its current connection and reconfigure against a new endpoint, plus a launcher IPC protocol so the tunnel target swaps without a frontend restart. Both feasible (sentinel file, named-pipe message, etc.) but not justified by current usage. If the "Ctrl+Q + relaunch" friction becomes felt, revisit.
- **Multi-host parallelism** — only one host is active at a time. A second-host probe (e.g. for a status indicator) would need a separate transport task. Out of scope.
- **`hosts.toml` schema validation** — the parser is tolerant (unknown keys ignored, malformed numbers → None). Bad entries render as `(no endpoint)` in the picker and are no-ops on Enter. No error reporting beyond that.

### Frontend API surface

```rust
// hosts.rs
pub struct HostEntry { name, ssh_alias, remote_repo, tcp_port, socket }
pub struct HostsConfig { default_host, hosts }
pub fn load() -> HostsConfig;
pub fn parse(text: &str) -> HostsConfig;

// state_persistence.rs additions
struct GlobalState { …, last_host: Option<String> }

// gpu.rs additions
enum Mode { Files, Modules, Sessions, Hosts }
struct State { …, hosts_config: HostsConfig, selected_host: Option<String> }
fn populate_hosts_tree(&mut self);
fn pick_host_under_cursor(&mut self);
```

### Launcher API surface (PowerShell)

```powershell
function Read-DevenvLastHost { …reads state-<COMPUTERNAME>.toml… }
function Read-DevenvHosts { …reads .devenv/hosts.toml… }
# Pre-flight: set SOT_HOST / SOT_REMOTE_REPO / SOT_TCP_PORT
# from $lastHost ?? hosts.default_host ?? hardcoded fallback, unless
# user already set them in the env.
```

## Consequences

- **Single source of truth for host config.** Adding a new remote = one entry in `hosts.toml`, no launcher edit.
- **Reconnection respects last choice.** Tunnel supervisor restart and transport reconnect both target whatever `last_host` resolved to at launch — never bounces between hosts.
- **Picker is a 1-keystroke discoverability win** (`h` → see all configured hosts + which one is current + which is default).
- **Cost of switching = Ctrl+Q + click shortcut.** Two clicks/keystrokes; acceptable given switching is rare.
- **Parser simplicity is load-bearing** — both Rust and PowerShell parse the same file without a real TOML library. Trade: no escape sequences, no nested tables, no inline arrays. If the format needs to grow we'd switch both to a real parser.
- **Backward compatibility**: env vars (`SOT_HOST` etc.) still win over `last_host`, so existing CI / launcher overrides keep working.
