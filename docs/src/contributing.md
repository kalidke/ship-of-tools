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

## See also

- [Requirements](design/requirements.md) — the source of truth for scope.
- [Roadmap](design/roadmap.md) — phase plan and milestones.
