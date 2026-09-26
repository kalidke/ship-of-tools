# Contributing

Ship of Tools has a small number of working conventions and one firm rule about where
decisions are recorded. This page covers both, plus how to build and test.

Before adding a feature, read [Requirements](design/requirements.md) — it defines
scope. This page and the design pages define structure.

## Working conventions

- **Julia is the canonical language** for plugin code, the ABI, and any
  Julia-aware logic. Use it expressively — lean on multiple dispatch and the type
  system. See [The Dispatch ABI](extend/abi.md).
- **Rust is for plumbing** — the TUI, IPC, file watching, process supervision.
  Keep it boring and predictable.
- **Plotting is CairoMakie** when generating plots in Julia.
- **Eat dogfood.** Core handlers ship as plugins to themselves; the core modes and
  standard file types are methods on the same abstract types a third-party plugin
  extends. If core seems to want privileged access, the rule is to *fix the ABI*,
  not to special-case core. This keeps the extension surface honest.
- **Boundaries are serialization seams.** `TreeNode` and `PreviewPayload` carry
  opaque, kernel-defined payloads. Rust never learns about new entity kinds —
  adding a `FileType` (and, by design, a `Mode`) requires zero Rust changes. See
  [Line Protocol](design/protocol.md).
- **Reactive over eager** for staleness, refresh, and indexing. Visible drift
  (e.g., a stale-annotation badge) is a feature, not a bug.
- **Defer until forced.** If a feature can wait for a later phase, it should.

## The decision process

Cross-cutting decisions get documented — **never only in a code
comment or a commit message.** Some decisions are explicitly
deferred (multi-agent coordination, remote transport variants, MCP as the internal
protocol, an embedded editor, a user-preferences mechanism, automatic plot
capture); when you reach for one of these, check whether it is still deferred
before building it.

Plan amendments go through a pull request. The phase plan is a committed
document — argue changes in the diff, not in side channels. See the
[Roadmap](design/roadmap.md) for the milestone structure.

## Build and test

The repository is a Julia umbrella package with a Rust workspace inside it.

Julia — run the umbrella suite and the core package suite:

```julia
using Pkg
Pkg.test()                          # umbrella package
Pkg.test("ConceptExplorerCore")     # core, from its own environment
```

Rust — the frontend, backend, and protocol crates live under `rust/`:

```bash
cargo test --manifest-path rust/Cargo.toml
```

## Developing the frontend: rebuild without dropping your session

Ship of Tools can rebuild and restart its own frontend — so you can edit the frontend,
recompile, and relaunch into the new binary without leaving the app. The moving
parts:

- **Staged-copy supervisor.** The launcher copies the built
  `sot` into a staging directory (`%LOCALAPPDATA%\sot\bin\`) and
  runs the app from that copy inside a respawn loop. Because the running file is
  the staged copy, `cargo build --release` can overwrite `rust/target/release/`
  freely — no running-exe file lock — and you see build output live.
- **Exit-75 sentinel.** The frontend requests a relaunch by exiting with code
  **75**; any other code is a real quit. A background watcher polls for a
  relaunch-request sentinel file; on seeing it, the frontend exits 75 and the
  supervisor re-stages the (freshly built) binary and respawns with
  `--relaunched`.
- **The drawer reopens plain.** On `--relaunched`, the frontend opens into the
  Terminal drawer with a plain shell and runs nothing. A session that must
  survive frontend relaunches (the dev driver above is one) is a **local
  capsule session**, held by its own supervisor.

The one-command driver is `scripts/relaunch-sot.ps1`: it runs
`cargo build --release` and drops the relaunch sentinel **only on a green
build** — a failed build leaves the running app untouched.

### Prefer the relaunch loop over killing the frontend

!!! warning "Use the relaunch loop, not a process kill"
    The dev `claude` session that drives frontend development is a local
    capsule session, not a passenger of the frontend process, so it survives
    either way. Still restart through the relaunch loop —
    `scripts/relaunch-sot.ps1` (build → sentinel → exit-75 → re-stage →
    respawn) — rather than a process kill: it re-stages the freshly built
    binary and keeps the supervisor's SSH tunnel alive across the swap.

Note that changes to the *supervisor script itself* (`launch-sot.ps1`) are not
picked up by the exit-75 in-place loop — those require a full restart of the
launcher. The exit-75 path only re-stages the frontend binary.

## See also

- [Requirements](design/requirements.md) — the source of truth for scope.
- [Roadmap](design/roadmap.md) — phase plan and milestones.
- [Design decisions](https://github.com/kalidke/ship-of-tools/tree/main/docs/adr) —
  the ADRs in `docs/adr/`.

## Releases

Releases are tag-driven: `scripts/release.sh` stamps the versions and tags,
and CI builds, smoke-tests and publishes the artifacts
([ADR 0030](https://github.com/kalidke/ship-of-tools/blob/main/docs/adr/0030-versioning-release-and-auto-update.md)).
