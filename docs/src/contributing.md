# Contributing

Ship of Tools has a small number of working conventions and one firm rule about where
decisions are recorded. This page covers both, plus how to build and test.

Before adding a feature, read [Requirements](design/requirements.md) — it defines
scope. This page and the design pages define structure.

## Working conventions

- **Julia is the canonical language** for plugin code, the ABI, and any
  Julia-aware logic. Use it expressively — lean on multiple dispatch and the type
  system. See [The Dispatch ABI](extend/abi.md).
- **Rust is for plumbing** — rendering, IPC, file watching, process supervision.
  Keep it boring and predictable.
- **Plotting is CairoMakie** when generating plots in Julia.
- **Eat dogfood.** Core handlers ship as plugins to themselves; the standard
  file types are methods on the same abstract type a third-party plugin
  extends (the core modes are native Rust views, since `Mode` is not built). If core seems to want privileged access, the rule is to *fix the ABI*,
  not to special-case core. This keeps the extension surface honest.
- **Boundaries are serialization seams.** Rust never learns about new entity
  kinds: the window dispatches on a preview's MIME type. Adding a `FileType`
  needs no frontend change unless its output must be bounded, which also names
  its extensions in the daemon's preview gates. See
  [the serialization seam](@ref abi-serialization-seam) and
  [Line Protocol](design/protocol.md).
- **Reactive over eager** for staleness, refresh, and indexing. Visible drift
  (e.g., a stale-annotation badge) is a feature, not a bug.
- **Defer until forced.** If a feature can wait, it should. A diagnosed defect is never deferred.
- **One owner per concept, and the checks.** Before adding code, find the concept's owner (`docs/ownership.md`) and
  change it there. CI enforces the limits: a file holds at most 800 code lines, a folder at most 12 source files and
  3,000 lines, a function at most 100 lines (`scripts/tests/check-layout.sh`). A manual page changes in the commit
  that changes the code it describes. The root `CLAUDE.md` ("Rules" and "Limits") has the full list.

## The decision process

Cross-cutting decisions get documented — **never only in a code
comment or a commit message.** Decisions are recorded as ADRs in `docs/adr/`.
Parts of the design are unbuilt (the orchestrator chat, the Types, Math and Outputs modes,
plugin discovery); the pages that describe them say so. See the
[Phase 1 plan (historical)](design/roadmap.md) for the original milestone structure.

## Build and test

The repository is a Julia umbrella package with a Rust workspace inside it.

Julia — the umbrella suite runs from the root, and each package runs from its own environment, as CI does:

```julia
using Pkg
Pkg.test()                          # umbrella package, from the repository root
```

```bash
julia --project=core -e 'using Pkg; Pkg.instantiate(); Pkg.test()'    # likewise julia/kernel, julia/repl,
                                                                      # julia/plugins/pdf-file,
                                                                      # julia/plugins/video-file, julia/sotlog
```

Rust — the frontend, backend, and protocol crates live under `rust/`. Build the workspace first (the backend's
integration tests need `sot-capsule` built), then test it; the MathJax tests need `npm ci` in
`rust/backend/sidecars/mathjax`:

```bash
cargo build --workspace --locked --manifest-path rust/Cargo.toml
cargo test --workspace --locked --manifest-path rust/Cargo.toml
```

`.github/workflows/rust.yml` and `CI.yml` hold the exact steps, including the system tools the preview-plugin tests
use (`pdftoppm`, `ffmpeg`).

## Developing the frontend on Windows: rebuild without dropping your session

On Windows, Ship of Tools can rebuild and restart its own frontend — so you can edit the frontend,
recompile, and relaunch into the new binary without leaving the app. The moving
parts:

- **Staged-copy supervisor.** The launcher copies the built
  `sot` into a staging directory (`%LOCALAPPDATA%\sot\bin\`) and
  runs the app from that copy inside a respawn loop. Because the running file is
  the staged copy, `cargo build --release` can overwrite `rust/target/release/`
  freely — no running-exe file lock — and you see build output live.
- **Exit-75 sentinel.** The frontend requests a relaunch by exiting with code
  **75**; **76** ("converge") also relaunches, after the launcher updates and rebuilds; any other code is a real quit. A background watcher polls for a
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
    binary, and the swap costs one reconnect per host — the frontend's
    backoff respawns each `ssh` child.

On Linux and macOS the installed `sot-launch` respawns the window on exit 75 or 76, after it applies any pending
update, and a window started by
`scripts/launch-sot.sh` is not respawned: restart it by hand. ADR 0017 says how the window restarts.

Note that changes to the *supervisor script itself* (`launch-sot.ps1`) are not
picked up by the exit-75 in-place loop — those require a full restart of the
launcher. The exit-75 path only re-stages the frontend binary.

## See also

- [Requirements](design/requirements.md) — the source of truth for scope.
- [Phase 1 plan (historical)](design/roadmap.md) — the original phase-1 plan, with a table of what is built.
- [Design decisions](https://github.com/kalidke/ship-of-tools/tree/main/docs/adr) —
  the ADRs in `docs/adr/`.

## Releases

Releases are tag-driven: `scripts/release.sh` stamps the versions and tags,
and CI builds, smoke-tests and creates a draft release with the artifacts,
which the release skill publishes. Every tag, candidate or final, is cut from
main; the stamp after the tag moves main to the next version with `-dev`
([ADR 0030](https://github.com/kalidke/ship-of-tools/blob/main/docs/adr/0030-versioning-release-and-auto-update.md)).
