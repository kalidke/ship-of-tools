# Ship of Tools

An agentic development environment for Julia. `requirements.md` is the scope; `docs/package.md`, loaded below, is the
map: what runs, the subsystems and the guarantee each gives the others, how they depend on each other, the decisions
taken and the order of the passes now reorganizing the tree. A subsystem's charter (the CLAUDE.md in the folder the
package page names) is its contract until its pass rewrites it. `docs/src/` is the user manual. `docs/history/` holds
the design records and the old folder pages; read one only when a question needs it.

Every session, in a row or outside any, reads `docs/SESSION.md` at its start and after each compaction. A subagent
does not read it and uses no mail; it reports to the session that launched it.

@docs/package.md

## Rules
- **Elegance first: simple and elegant leads to performance and security; as simple as possible, but no simpler.** Every
  design and review round asks both "what is missing?" and "what can be deleted?". A field, type, file or knob names the
  invariant it serves, or it is a deletion candidate. Stripping past an invariant (durability, identity, the honesty of
  the record) is false elegance.
- One owner per concept. Before adding code, find the subsystem whose contract owns the concept (the package page, then
  `docs/ownership.md` until the contracts replace it) and change it there.
- Julia for plugin code and Julia-aware logic (the kernel parses with JuliaSyntax); Rust for plumbing (rendering, IPC,
  watching, supervision), kept boring. The Rust-Julia boundary is a serialization seam: the daemon speaks to the kernel
  and the REPL in JSON frames over their stdio, and Rust builds every tree node.
- A new file type is a `FileType` plugin written against `core/`'s public methods, as the built-in ones are. The kernel
  loads plugins only from its own project: the built-ins are `using`d in `julia/kernel/src/ShipToolsKernel.jl`, each a
  `[sources]` dependency in `julia/kernel/Project.toml`; HDF5Preview, the one heavy plugin, loads on its first preview
  through `LAZY_PLUGIN_FOR_EXT` in `julia/kernel/src/preview.jl`. Nothing discovers a user's packages (ADR 0006's
  declarative mechanism is unbuilt), so a new plugin is a kernel dependency plus a `using` line or a table row. A plugin
  whose output must be bounded also needs its extensions in `is_bounded_output_plugin`
  (`rust/backend/src/files/preview/mod.rs`). `Mode`, `ConceptEntity`, `AnnotationKind`, `Tool` and `Capture` are
  declared in `core/` but unbuilt; scope any extensibility claim to `FileType`.
- Reactive over eager. A concept annotation shows stale when the file's hash, which the kernel computes on
  `file.parse`, no longer matches its `synced_against`; nothing sweeps in the background.
- Defer features until forced. A defect is known when it has been seen in use, by a person or by a session operating
  the product as a user; a thought from reading code is a note. Either goes to the owning subsystem's queue and is
  handled at that subsystem's pass; a defect that blocks use is fixed on the release branch.
- Work happens on main, one writer at a time; a second pass in parallel works in a worktree on a disjoint subsystem.
  Before a push, the touched crates' tests and the workspace build run locally. Hosted runs happen once at the end of
  a pass and once at the release gate.
- Read `requirements.md` before adding a feature. Change the contract or manual page in the commit that changes the
  code it describes. Plots in Julia use CairoMakie.
- The repo is public: no private host names, user names, LAN details or host-suffixed handles in commits, PRs, code
  comments or docs. A push is swept for private names before it leaves this machine.
- The repo is canonical, not any machine's memory: the user works across computers, and per-machine session memory is
  never a prerequisite. Session memory holds operating state only; a fact about the code belongs in its subsystem's
  contract.
- Run a command on a second host with `scripts/tests/on-host.sh HOST DIR -- CMD`, never a hand-built `env -u` list
  inside a quoted ssh command: its newlines can run a bare `env` there and print that host's environment.
- Never kill the window's process: ADR 0017 (`docs/history/adr/`) says how it restarts.
