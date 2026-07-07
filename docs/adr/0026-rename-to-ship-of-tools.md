# ADR 0026: Rename DevEnv.jl → "Ship of Tools"

**Status:** Accepted (2026-06-24)
**Date:** 2026-06-24

## Context

This project is a **polyglot application** — a Rust workspace (frontend + backend + protocol) that owns its own GPU renderer, plus a Julia kernel/ABI/plugin substrate for extensibility. It is **not** a registered Julia package and will never be loaded as `using ShipOfTools`. The `.jl` repo suffix is a convention reserved for registered Julia packages (`using X`), so it misrepresents the project. The product is renamed to **"Ship of Tools"**.

## Decision

| Surface | Old | New |
|---|---|---|
| Display name | DevEnv | **Ship of Tools** |
| GitHub repo / dir slug | `DevEnv.jl` | **`ship-of-tools`** (no `.jl`) |
| Launch binary | `devenv-frontend` | **`sot`** |
| Daemon binary | `devenv-backend` | **`sotd`** |
| Rust crates | `devenv-protocol/backend/frontend` | **`sot-protocol/backend/frontend`** |
| Julia internal pkgs | `DevEnv*` (Kernel, Repl, 7 plugins) | **`ShipTools*`** |
| Julia root helper | `DevEnv` | **`ShipTools`** |
| ABI package | `ConceptExplorerCore` | **unchanged** (names the ABI; stable identity) |

UUIDs are preserved across all Julia renames (only names change).

## Guardrails — DO NOT rename (shared `sot-comm` layer)

`sot-comm` is a **general cross-repo session-messaging layer** (other repos depend on it). Renaming it breaks them. Leave verbatim:

- `~/.sot-comm/`, the in-repo `comm/` directory, `comm-*.sh`, `sot-nav.sh`, `comm/PROTOCOL.md`.
- Comm/session env vars: `SOT_COMM_*`, `SOT_WORKSPACE`, `SOT_WORKSPACE_ROOT`, `SOT_SESSION`, `SOT_RELAY_ENDPOINT`, `SOT_SPAWN_ENDPOINT`, `SOT_NAV_DRY_RUN`.

## Seams discovered (4-agent inventory)

1. **App↔comm env contract** — Rust *writes* `SOT_WORKSPACE/_ROOT/SESSION/COMM_*` into the PTY env for the shared comm layer to *read* → those stay `SOT_*`.
2. **tmux prefix `sot-be-`** — the FE filters sessions on `starts_with("sot-be-")`; a BE↔FE contract → flip lockstep at cutover.
3. **comm-install seam** — the root package's `update_comm()` is invoked `using DevEnv` from the comm-adapter skills → rename them together.
4. **Rust↔Julia kernel-spawn** — the backend spawns the kernel/REPL by Julia module name → must track `ShipTools*` (done, commit 3963b87).

## Phased plan

- **Phase 0** — merge `feat/op-fe-command` → main. ✅
- **Phase 1** — Rust crates + binaries → `sot-*` / `sot` / `sotd`. ✅
- **Phase 2** — Julia `DevEnv*`→`ShipTools*` (uuids preserved) ✅ + kernel-spawn fix ✅ + root `DevEnv`→`ShipTools` & comm-install seam (pending) + prose → "Ship of Tools" (docs done; CLAUDE/STATUS/TODO + this ADR pending).
- **Phase 3 — cutover** (FE-co-reviewed, diff-first, FE-quiet): GitHub repo rename; local dir rename + FE re-clone; env/config/socket/tmux/`sot_ui` flips; hardcoded paths (`launch-devenv.sh:21`, `.devenv/hosts.toml:22`); `%LOCALAPPDATA%\devenv` (FE-owned); comm-handle change; converge `rename/ship-of-tools` + `docs/full-site`; final merge to main.

### Cutover checklist (Phase 3)
- [x] GitHub repo rename `kalidke/DevEnv.jl` → `kalidke/ship-of-tools` — **done** (`origin` is `ship-of-tools.git`).
- [x] Local dir rename on the dev backend + FE re-clone — **done 2026-06-26**: backend dir `sot` → `ship-of-tools`; FE clone already `ship-of-tools`. **Decision:** the *clone dir* = full repo name; `sot` stays the operational shorthand. Preserved by launching the daemon with `--label sot` (server.rs uses `opts.label`), so the home workspace keeps slug `sot` → `.SoT`, the `sot-be-sot` tmux, the `@sot-myhost` handle, `SOT_*`, and `sot-comm` — all unchanged. *(Durable follow-up: comm-context derives the handle from the dir basename, so pin it — prefer the daemon-stamped `SOT_WORKSPACE` slug over the basename — before this repo's sessions re-join.)*
- [x] App env vars → `SOT_*` in source (app subset only — NOT the comm-layer guardrail set), both Rust producers and script consumers in lockstep
- [ ] Config dirs `.devenv`→`.sot`, `~/.config/devenv`→`~/.config/sot` (+ read-fallback to old)
- [ ] Migrate the *live* runtime socket dir + the *running* `sot-be-` tmux sessions off their old names (source prefix is already `sot-be-`; FE filters on it) — lockstep with FE
- [x] Nav-envelope JSON key → `sot_ui` in source; runtime pickup still needs a version-gated migration / FE rebuild
- [ ] FE self-relaunch machinery + `%LOCALAPPDATA%\devenv` state dir — FE-owned, FE flips
- [x] `launch-devenv.sh:21` REMOTE_REPO default → `ship-of-tools` + `--label sot` (and `restart-backend.sh` launches with `--label sot`). [ ] `.devenv/hosts.toml:22` still to check.
- [ ] devenv-docs flips staged URL/path/binary refs in lockstep (ping at cutover)
- [ ] Orphaned old tmux sessions (`sot-be-devenv_jl`) cleaned up

## Consequences

- The `rename/ship-of-tools` branch was intentionally **mixed-state** mid-flight (e.g. crate `sot-*` while env vars and the nav-envelope key were still on their old `DEVENV_*` / `devenv_ui` spellings). That was fine — names and connection/wire contracts are independent, and the branch compiled + stayed internally consistent at each phase. The source has since converged on `sot`/`SOT_*` throughout.
- ADRs ≤ 0025 are **historical** and left under the old name; this ADR records the rename. `comm/PROTOCOL.md` is deliberately exempt (shared layer).

## Cutover scope decisions (greenlit 2026-06-24)

The initial cutover was **wire/env-compatible and minimal-disruption** — it renamed the repo, product, code, and binaries, but deliberately **kept the following operational names on their old `devenv`/`DEVENV` spellings for that first pass**, so running sessions, the FE, comm, and all managed workspaces were NOT disrupted mid-flight:

- **Local working-dir name `DevEnv.jl`** (on the dev backend) — renaming it stales this BE session's cwd and orphans the `sot-be-<dir-slug>` tmux session the BE Claude actually runs in.
- **tmux session prefix + home-base default** — flipping the prefix is GLOBAL across *every* managed workspace: it orphans them all at once and needs the FE's session filter to flip in lockstep.
- **App env vars** — flipping is producer↔consumer coupled (Rust writes / scripts read) and partly FE-machinery coupled.
- **Nav-envelope wire key** — keeping it left the running frontend ⇄ daemon wire-compatible, so a backend restart needed **no forced FE rebuild** (the FE auto-reconnects).

**Source flip subsequently completed.** The full `sot` source flip has since landed: the tmux prefix literal is now `sot-be-` (`paths.rs`) with home-base `sot-llm`, the app env vars are `SOT_*`, and the nav-envelope wire key is `sot_ui` — all in the Rust/Julia source. What remains is the **runtime/deploy migration**, which is intentionally separate and still pending (each its own one-time op): the local-dir rename `DevEnv.jl`→`ship-of-tools`, the GitHub repo rename, migrating the *live* tmux sessions + socket dir off their old names, and a coordinated FE rebuild/version-gate so the running FE picks up the `sot_ui` key. These are not bundled into the source rename.

## Post-cutover fixes & learnings (2026-06-24)

The live cutover surfaced two operational couplings the branch work didn't predict:

1. **The backend binary name is comm-coupled.** The shared comm scripts auto-detect the relay daemon via `pgrep -af devenv-backend` to extract its `--tcp` endpoint (`comm-relay.sh`, `comm-listen.sh`, `comm-spawn.sh`, `comm-despawn.sh`, `devenv-fe`). Renaming the daemon to `sotd` broke detection for any session NOT setting `SOT_RELAY_ENDPOINT` (the FE sets it explicitly, so it was fine; auto-detect sessions broke). Fixed by matching `pgrep -af 'sotd|devenv-backend'` (commit `221b0b6`). **Learning:** the backend binary name belonged in the keep-for-now/compat bucket alongside the tmux prefix and wire key. For the eventual full-`sot` flip, update comm detection in lockstep — or better, decouple comm from the binary name (pidfile or a config endpoint).

2. **The Julia kernel/repl envs must be re-instantiated after the package rename.** The gitignored `julia/kernel/Manifest.toml` still pinned the old `DevEnv*` names at the same UUIDs, so `Pkg.develop` of the renamed `ShipTools*` packages errored: `Refusing to add ShipToolsMarkdown [f621c3d9] — DevEnvMarkdown=f621c3d9 already exists in the manifest`. Fix: delete the stale kernel/repl Manifests, then `Pkg.develop` the path-deps + `Pkg.resolve()`/`instantiate()`. **Add to setup:** any fresh clone — and the eventual full-`sot` flip — needs this kernel-env re-instantiate step.

3. **A dead kernel wedges sotd's event-broadcast loop (latent sotd bug, exposed by #2).** While the kernel env was broken (#2), the kernel subprocess crashed on load — and sotd then served the initial snapshot to FEs but stopped **pushing ongoing events** (`pty.evt`, `agent.message`) to *all* clients (FE app-channel AND the comm relay). Result: frozen FE terminals + comm delivery silently dead (peers could `relayed -> FE` but nothing reached the FE's inbox). Restarting sotd *after* the kernel env was fixed (so the kernel stays alive) cleared it. **Learning:** sotd's broadcast path is coupled to kernel health — a blocking kernel call in the broadcast loop can wedge ALL outbound push. Two follow-ups: (a) operationally, instantiate the kernel env *before* the bounce so the kernel never dies; (b) code-level, decouple the broadcast loop from kernel calls (make them non-blocking / off the broadcast task) so a dead kernel degrades previews to bytes-level (already handled) without freezing event push.

## Runtime/deploy migration finalized (2026-07-01)

The runtime/deploy migration flagged as "still pending" above is now complete —
the source carries **no `devenv` spelling anywhere** (`git grep -i devenv` outside
this `docs/adr/` record, `.claude-bus/`, and `.claude-memory/` is empty; 274 rust
tests green). What landed:

- **Legacy read-fallbacks removed → single-path `sot`.** The "prefer `sot`, fall
  back to legacy `devenv`" discovery in the FE (`hosts.rs`, `settings.rs`,
  `keybindings.rs`, `state.rs`, `state_persistence.rs`, `gpu.rs::sot_state_dir`)
  and the BE capture dir (`handlers.rs`/`watcher.rs` → `.sot/captures/`) is gone.
  The config/state dirs were already migrated (`~/.config/devenv` removed), so the
  fallback was dead weight. A box still holding un-migrated `.devenv` /
  `%LOCALAPPDATA%\devenv` state should **cold-relaunch** — it loses only ephemeral
  UI state, never source.
- **`DEVENV_REPO_DIR` env fallback dropped** (`gpu.rs`): the supervisor now exports
  only `SOT_REPO_DIR`, so each box needs a **cold** relaunch (not a warm exit-75)
  to re-export under the new name.
- **Runtime paths flipped:** session socket dir `/run/user/<uid>/devenv` →
  `…/sot` (`paths.rs`); Windows FE runtime dir `%LOCALAPPDATA%\devenv` → `…\sot`;
  backend log `/tmp/devenv-backend.log` → `/tmp/sotd.log`.
- **Launcher/asset files renamed:** `launch-devenv.{ps1,sh}` → `launch-sot.{ps1,sh}`,
  `relaunch-devenv.ps1` → `relaunch-sot.ps1`, `devenv.svg` → `sot.svg`; desktop
  shortcut `DevEnv.lnk` → `Ship of Tools.lnk` (re-run `scripts/install-shortcut.ps1`).
- **Comm deprecation list emptied** (`src/comm.jl`): the transitional
  `devenv-fe`/`devenv-nav.sh` prune entries are retired (active machines already
  pruned); the mechanism stays for future renames. The comm-detection matcher
  (fix #1 above) is now `sotd`-only.

**Deploy:** rebuild `sotd` (done on the dev backend) + each Windows FE (`relaunch-sot.ps1`),
cold-relaunch, repoint shortcuts.
