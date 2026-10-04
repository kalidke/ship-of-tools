# Ship of Tools

An agentic development environment for Julia. Agent programs (Claude Code, Codex) run in rows, one project directory
each; a native window lets the developer steer, watch and review them, browse the project, preview files and run a
Julia REPL. `requirements.md` is the scope; `docs/adr/` records why; `docs/src/` is the user manual.

## How it runs
- `sot`, the window (Rust, winit + wgpu). ratatui computes the chrome's cells only; one wgpu pass draws every pixel.
  It is not a terminal program. It holds one connection to each daemon it dials.
- `sotd`, the daemon, one per OS account per computer. It owns that computer's rows, serves files, previews and pages,
  supervises the Julia children, and answers on one private socket (a named pipe on Windows).
- `sot-capsule`, one supervisor per row and one leg under it running the agent on a real terminal, recorded into an
  append-only voyage. Rows outlive the window and the daemon by design; the last window's lease decides whether a
  computer's rows end when it closes.
- Julia children of the daemon: a kernel per row (Julia-aware previews and project scans; it never runs user code and
  never loads the user's environment), a REPL per row (runs user code in the row's environment), one Pluto server.
- Other computers: `hosts.toml` declares them and names the hub; the window reaches a far daemon over ssh into
  `sotd stdio-bridge`. Sessions message each other through the comm folder `~/.sot-comm`.

## Subsystems
Each line names the subsystem and the folder of its charter page (idea, owns, promises, connections).
- **wire**: every byte two programs exchange; one integer for compatibility. `rust/protocol/`
- **topology**: hosts.toml, endpoints, ssh logins, relay units. `rust/protocol/src/topology/`
- **server**: sotd's one listener; admission and routing. `rust/backend/src/server/`
- **lifecycle**: leases, the close, child ownership, bounded exits. `rust/backend/src/lifecycle/`
- **rows**: the row registry and the daemon side of each row's capsule. `rust/backend/src/rows/`
- **agents**: the agent launch recipe, accounts, launchers, skills, daemon-client CLIs. `agents/`
- **capsule**: supervisor, leg, voyage record, lanes, attach client. `rust/log/`
- **platform**: dirs, host name, durable writes, locks, peer identity. `rust/log/src/host/`
- **messaging**: inboxes, the registry, work-state, the wake. `comm/`
- **files**: a workspace's files, previews and `.concept/` annotations. `rust/backend/src/files/`
- **sidecars**: kernel, REPL, Pluto, MathJax, monitor; the plugin ABI. `rust/backend/src/sidecars/`
- **pages**: loopback HTTP pages and the window's page proxy. `rust/backend/src/pages/`
- **fe-ui**: the window's one UI thread, drawing and input. `rust/frontend/src/ui/`
- **fe-net**: the window's connections to daemons. `rust/frontend/src/net/`
- **distribution**: release, CI, install, update, apply, launch. `scripts/`
- **records**: ADRs, the manual, this file. `docs/`

Who owns what: `docs/ownership.md`. How they connect: `docs/integration.md`.

## Finding your way
- Pages come in three tiers: this root map; one charter per subsystem in its charter folder; and a module
  page in every other source folder. No page repeats its parent.
- Search finds the path. The first Read of a file in a folder loads that folder's page and every page above it; Grep and
  shell searches load none. So read the file, not only grep it.
- Folders with no page of their own are listed, with their reason, in `scripts/tests/exempt.txt`.
- Designed but unbuilt parts (the Project, Types, Math, Outputs and Agents modes; the concept layer's refresh and
  reference checks; the plugin contract beyond `FileType`) are described, marked unbuilt, in `docs/src/guide/modes.md`,
  `docs/src/guide/concept-layer.md`, `docs/src/guide/color-coding.md`, `docs/src/extend/abi.md` and
  `docs/src/extend/mode.md`.

## Limits
- A file holds one concept and at most 800 code lines. Tests sit inline, or in a sibling file of at most 800 lines.
- A folder holds at most 3,000 non-test code lines and 12 source files.
- A function holds at most 100 lines.
- The checks: `scripts/tests/check-layout.sh`, run by rust.yml's "Check the layout" with each exception and its reason
  in `scripts/tests/check-layout.allow`; and rust.yml's "Function length" clippy step, whose count of allowances can
  only fall. This page is the map tier and has no `## Files` list (a reasoned exception in that allow file).

## Rules
- **Elegance first: simple and elegant leads to performance and security; as simple as possible, but no simpler.** Every
  design and review round asks both "what is missing?" and "what can be deleted?". A field, type, file or knob names the
  invariant it serves, or it is a deletion candidate. Stripping past an invariant (durability, identity, the honesty of
  the record) is false elegance.
- One owner per concept. Before adding code, find the concept's owner in the ownership table and change it there.
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
- Defer features until forced. A diagnosed defect is never deferred: it goes in the next candidate, and the only reasons
  to hold one back are two fixes contending for one file or a root cause not yet proven (say which, per item).
- Read `requirements.md` before adding a feature. Change a CLAUDE.md, ADR status or manual page in the commit that
  changes the code it describes. Plots in Julia use CairoMakie.
- The repo is public: no private host names, user names, LAN details or host-suffixed handles in commits, PRs, code
  comments or docs.

## Working in this repo
- Development runs in the `ship-of-tools` workspace row. The daemon's default row is a hidden anchor; nothing runs in it.
- `/worktree` (`agents/worktree/comm-worktree-new.sh <short>`) makes `<repo-parent>/worktrees/ship-of-tools-wt-<short>`
  on branch `wt/<short>` with its own session; `/worktree status|sync|clean` manage it.
- The repo is canonical, not any machine's memory: the user works across computers, and per-machine session memory is
  never a prerequisite. Session memory holds working practices only; a fact about the code belongs in the CLAUDE.md of
  the folder that owns it.
- A launcher the daemon spawns full-paths its binaries: a capsule inherits the daemon's environment, whose `PATH`
  lacks `~/.local/bin`. Spawn and daemon boot: ADR 0046.
- Window restart (ADR 0017; read it before any restart): never kill the window's process. `scripts/relaunch-sot.ps1`
  writes the relaunch sentinel and the Windows launcher respawns the window on exit 75 or 76. On Linux and macOS the
  installed all-in-one `sot-launch` respawns on 75 only, and a window started by `scripts/launch-sot.sh` is not
  respawned.
- Releases follow the `release` skill and `scripts/release.sh`.
- A session's handoff is its recovery file, `dev/output/handoff-<handle>.md` (gitignored), written at milestones.
  The private ops sidecar (`../ship-of-tools-ops`, or `$SOT_OPS_DIR`) holds the publish guard's denylist; on a machine
  where the sidecar exists but the denylist is unreadable, the guard blocks publishing.

## Messaging between sessions
- Design of record: `docs/adr/0049-messaging-on-one-page.md`; the contract is `comm/PROTOCOL.md`.
- A session's handle is its folder name plus its box name; `comm-context.sh` prints it.
- Send with `comm-send.sh @<handle> "text"`; its one result is `filed -> @<handle>` or `FAILED -> @<handle>: <why>`,
  except that a send relayed to a handle the hub's folder does not list can still end `NOT CONFIRMED: sent for @h; ...`
  or `filed -> @h (by <filer>, relay)` (`comm/mail/comm-relay.sh`).
- An idle row is woken by the line `[sot-comm] you have mail: run comm-poll.sh`; a turn cannot end with directed
  mail unread. Read with `comm-poll.sh`. To wait for a reply, end your turn.
- Run `/sot-session-start` once when a session starts.
