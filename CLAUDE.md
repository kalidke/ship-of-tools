# Ship of Tools

An agentic Julia development environment: AI agents drive the TUI, REPL, and navigation to read, run, and surface code — the developer steers, watches, and reviews. It preserves conventional editor and REPL mechanics and layers a **concept explorer** on top (moving fluidly between project, module, type, function, output, and math). The LLM is the primary author of code and maintainer of the concept-explorer artifacts.

`requirements.md` is the source of truth for **what** this system does. This document captures the design decisions for **how** it does it.

**Operational handoff** — the working-session handoff docs and durable Claude context live in the **PRIVATE ops sidecar** (repo `ship-of-tools-ops`, sibling checkout `../ship-of-tools-ops`, override `$SOT_OPS_DIR`), relocated out of this repo pre-public-flip (ADR 0030 §7):

- `<ops>/STATUS.md` — what's *done* (current at the last working session); `<ops>/TODO.md` — what's *next*. **Read TODO on session start if the user asks "what should we do" or pulls into a fresh machine** — find the first unchecked item and either do it or confirm with the user before proceeding.
- `<ops>/claude-memory/` — durable cross-OS Claude context (project memories). See "Cross-OS Claude memory" below.
- `<ops>/claude-bus/` — ephemeral cross-machine Claude-to-Claude messages; `/bus-note` + `/bus-sync` operate on it (they resolve the ops checkout themselves). See its README.

## Architecture at a glance

Three processes, even when running locally, communicating over a socket:

1. **Frontend (Rust + ratatui)** — yazi-inspired TUI. Stateless about the project. Renders what the backend sends; forwards keystrokes and commands.
2. **Backend daemon (Rust)** — owns project state. Watches files, supervises Julia processes, holds the orchestrator LLM session, exposes a JSON line protocol to the frontend.
3. **Julia kernel** — plugin host and project introspector. Owns dispatch tables, mode tree computation, file-type-aware indexing, AST hashing, and Julia-aware previews. Loads the project's `Project.toml` environment.

Plus a separate **REPL process** (Julia) for the user's interactive session, supervised by the backend, with a display shim that emits structured frames (stdout, stderr, value, image, error) over stdio.

Client/server even on local because it makes remote operation almost free later (same protocol, different transport). Pay the plumbing cost once.

## Why this language split

- **Rust** for the frontend, backend, file watching, IPC, terminal-protocol image rendering, LLM provider client. Single-binary cross-platform distribution. `tokio` + `notify` + `ratatui` + `ratatui-image` cover the stack.
- **Julia** for everything plugin-extensible and Julia-aware. `JuliaSyntax.jl` for parsing — reimplementing in Rust is a non-starter. Dispatch-as-plugin-mechanism is the unique value proposition here.

## The plugin model: multiple dispatch as the extension substrate

A small set of abstract types defines what's pluggable. Methods on these are the ABI; users and packages extend the system by writing methods. No registration, no manifest — `using MyExtension` and the dispatch tables grow.

```julia
abstract type FileType end          # PNG, JuliaSource, MarkdownDoc, ...
abstract type Mode end              # Files, Modules, Types, Math, ...
abstract type ConceptEntity end     # function, type, module, math derivation
abstract type AnnotationKind end    # type-meaning, math-derivation, ...
abstract type Tool end              # things the orchestrator can call
abstract type Capture end           # REPL outputs: Figure, DataFrame, ...
```

Dispatched methods (the contract):

```julia
preview(::Type{<:FileType}, path)        :: PreviewPayload
parse_entities(::Type{<:FileType}, path) :: Vector{ConceptEntity}

tree_root(::Type{<:Mode}, project)       :: TreeNode
tree_children(::Type{<:Mode}, node)      :: Vector{TreeNode}
preview_for(::Type{<:Mode}, node)        :: PreviewPayload

ast_hash(e::ConceptEntity)               :: String
applicable_annotations(::ConceptEntity)  :: Vector{Type{<:AnnotationKind}}

capture_payload(x::Capture)              :: Frame

tool_spec(::Type{<:Tool})                :: ToolSpec
tool_call(::Type{<:Tool}, args)          :: Result
```

**Core ships as plugins to itself.** The standard file types are implemented as methods on these types — no privileged access. This forces the ABI to stay honest and exercises the same path third-party plugins use.

**Implementation status (v0.3.x):** `FileType` is the seam that is wired end-to-end today (seven standard plugins + the HDF5 external example, all through the public ABI). The other five abstract types — `Mode`, `ConceptEntity`, `AnnotationKind`, `Tool`, `Capture` — are declared design targets with **no concrete subtypes yet**: the shipped navigation modes are implemented natively in the Rust frontend/backend (`files_mode.rs`, the kernel's `project.scan`), not dispatched through `Mode`. Plugin discovery is likewise not the declarative ADR 0006 mechanism yet (see that ADR's status note). When writing docs or answering questions about extensibility, scope claims to `FileType`.

The Rust↔Julia boundary is a serialization seam. The IR is generic:

```julia
struct TreeNode
    id::String                # opaque to Rust
    label::String
    kind::Symbol              # :module, :function, :pngfile, ...
    has_children::Bool
    badges::Vector{Symbol}    # :stale, :user_edited, :immutable, ...
    payload::Dict{String,Any} # opaque, kernel-defined per kind
end

struct PreviewPayload
    mime::String
    data::Vector{UInt8}
    extras::Dict{String,Any}
end
```

Adding a new `FileType` requires zero Rust changes.

## Modes (the switchable nav-tree roots)

Same three-level tree shape across all of them. A hotkey switches the root tree. Cursor position is preserved per-mode across switches.

| Mode      | Level 1 → Level 2 → Level 3                           | Preview                              |
|-----------|-------------------------------------------------------|--------------------------------------|
| Project   | Sections → contents → subitems                        | Rendered markdown / task detail      |
| Files     | Parent dir → current dir → contents                   | File at appropriate fidelity         |
| Modules   | Modules → functions → methods                         | Method source + concept artifact     |
| Types     | Types → facets (fields/methods/sub) → members         | Type def + meaning + data shape      |
| Math      | Concept areas → concepts → derivations/impls          | LaTeX + implementing functions       |
| Outputs   | Recent runs → contents → artifacts                    | PNG/plot/JSON/MP4                    |
| Agents    | Tasks → timeline → step detail                        | Diff / live tail / message           |

This table is the **design target**. Built today: **Files**, **Modules**, plus two modes the table predates — **Sessions** and **Hosts** (the frontend `Mode` enum is `{Files, Modules, Sessions, Hosts}`). Project, Types, Math, Outputs, and Agents modes are unbuilt; Agents mode is **pinned for later** — single orchestrator only in phase 1.

## Concept layer

LLM-maintained annotations live in a sidecar `.concept/` directory:

```
.concept/
  project/intent.md
  modules/MyModule.md
  types/MyModule/MyType.md
  functions/MyModule/myfunction.md
  math/geometry/rotation.md
```

Each annotation file has YAML frontmatter:

```yaml
target: MyModule.MyType
target_kind: type
synced_against: <ast_hash>
synced_at: 2026-01-15T14:30Z
authored_by: orchestrator | user
references:
  - MyModule.method1
  - math/geometry/rotation
```

**Two layers** with different update mechanics:

1. **Structural layer** — derived mechanically from code via `JuliaSyntax.jl` parse + (where loaded) live introspection. Always current. No LLM.
2. **Annotation layer** — LLM/user-authored prose attached to nodes in the structural layer. Can drift; staleness is detected by AST hash mismatch.

**Update lifecycle:**
- File save → re-parse affected files → annotations whose target's AST hash changed are marked stale.
- Stale annotations render with a yellowed/wilting badge in every mode (color is cross-cutting over entity provenance, not mode-specific).
- Refresh is **reactive**: user navigates to a stale annotation and triggers refresh with one keypress. Background sweep is opt-in for later.

**Reference verification:** annotations link to entities (`MyModule.method1`, `math/geometry/rotation`). Background pass verifies refs after every re-index; broken refs mark the annotation stale.

## Color coding (cross-cutting layer)

Independent of mode. Rendered uniformly across all trees because the same entities carry the same provenance.

- *User-edited recently* — warm color, fades over time
- *Agent-edited, unaccepted* — distinct color (unresolved diff)
- *Agent-edited, accepted* — neutral, small sigil
- *Immutable / external* (Base, deps, vendored) — dimmed
- *Stale annotation* — yellowed
- *Pinned / favorited* — accented border

## Phase 1 milestone

The smallest useful working slice:

- **Files mode** — filesystem nav with previews (markdown, PNG, syntax-highlighted `.jl`)
- **Modules mode (read-only)** — structural view from `JuliaSyntax.jl`
- **Persistent Julia REPL** with code dispatch (line, block, file)
- **Orchestrator LLM chat** that can read/write files and dispatch code to the REPL
- **`.concept/` annotation read and write** with AST-hash provenance
- **One demo external plugin** (e.g., HDF5 file preview) shipped as a separate package, validating the extension surface from outside core

**Explicitly out of scope for phase 1:**

- Multi-agent (orchestrator only)
- Types mode, Math mode, Outputs mode (come after)
- Remote operation (architecture supports it; transport not built)
- ~~MP4 playback (thumbnail only via shelled-out ffmpeg)~~ — **done post-phase-1:** the preview pane shows an ffmpeg poster frame; playback opens in the OS browser (HTML5 `<video>`, native decode) via `o` → `video.open`. An earlier in-pane `VideoPlayer` (frame streaming + transport controls) was built and then **removed** — see the ADR 0018 revision.
- Embedded editor — shell out to `$EDITOR`
- Background staleness sweep — reactive only
- Automatic plot capture from REPL — phase 1, user saves to `.concept/outputs/` or calls a small helper
- Windows polish — get Linux working first; Rust + a modern terminal mostly handles it but expect edge cases

## Conventions for Claude

When working in this repo:

- **Julia is the canonical language** for plugin code, ABI definitions, and Julia-aware logic. Use it expressively — leverage multiple dispatch, the type system, and idiomatic patterns.
- **Rust is for plumbing** — TUI, IPC, file watching, process supervision. Keep it boring and predictable.
- **Plotting is CairoMakie** when generating plots in Julia.
- **Eat dogfood**: core handlers ship as plugins to themselves. If core wants privileged access, fix the ABI instead.
- **Boundaries are serialization seams.** `TreeNode` and `PreviewPayload` carry opaque payloads. Rust never learns about new entity kinds.
- **Reactive over eager** for staleness, refresh, indexing. Visible drift is a feature, not a bug.
- **Defer until forced.** If a feature can wait until phase 2, it should.
- **Read `requirements.md` before adding features.** That document defines scope. This document defines structure.
- **Use Agent subagents actively when working in this repo** — both worktree-isolated (forked) and inline (non-forked). Pick per task: forked for speculative or risky multi-file work, inline for focused research and subtasks. Don't default to one mode.
- **Worktrees of this repo show as `.SoT-wt-<thing>`** in the sessions list — matching the `.SoT` home row, so the family groups left. Make them with the **`/worktree`** skill (`comm-worktree-new.sh <short>`), never by hand: it places the worktree at `<repo-parent>/worktrees/ship-of-tools-wt-<short>`, branches `wt/<short>`, replicates the external storage data symlinks, and spawns a session whose **display label** is `.SoT-wt-<short>` while the comm handle + on-disk dir stay `ship-of-tools-wt-<short>` (so status/clean/sync still group by the real repo). The `.SoT` display-prefix is pinned in the committed `.sot/worktree.toml` (`display_prefix`). `/worktree status|sync|clean` manage them.

## Cross-OS Claude context — the repo is canonical, not per-machine memory

The user may work across local and remote machines (see `.sot/hosts.toml.example`).
Some deployments use a shared `$HOME`; others are per-machine.

**Do not rely on per-machine Claude auto-memory.** It is *not* seeded on every box, and a session that depends on it having been seeded will run on stale or absent context — this has caused real failures (a Windows session followed a deleted memory rule and broke a working flow). The fix is not to seed harder; it is to treat the **repo itself as the single source of truth** and read it on any machine:

- **Operational procedures live in repo docs and are authoritative there** — read them in-repo, no copy step:
  - `requirements.md` (scope), this `CLAUDE.md` (design + conventions), `<ops>/STATUS.md` / `<ops>/TODO.md` (handoff, in the private ops sidecar).
  - `docs/adr/` for design decisions. **Frontend rebuild/restart is `docs/adr/0017-frontend-self-relaunch.md`** plus the header comments in `scripts/launch-sot.ps1` and `scripts/relaunch-sot.ps1`. **Read ADR 0017 before attempting any frontend restart** — the dev `claude` runs *inside* the frontend's Terminal drawer, so killing the frontend kills your own session; use `scripts/relaunch-sot.ps1` (sentinel → exit-75 respawn), never a process kill.
  - **Session spawn / daemon boot is `docs/adr/0023-daemon-fe-commands-and-spawn.md`** — read its top Update (the current design). A `workspace.create` with `autostart_claude` gets a wait-for-attach wrapper that `exec`s `ccb`, and the daemon boot-pty gives claude a stable init client (comm-spawn *and* nav-pane). **Gotcha: launchers the daemon spawns into a tmux pane must full-path their binaries** — the pane inherits the tmux *server* env, whose `PATH` lacks `~/.local/bin` (a bare `exec claude` → not found → silent <1s boot death). The daemon→FE command channel is **ADR 0025**.
- **`claude-memory/` (in the PRIVATE ops sidecar `../ship-of-tools-ops`) is a committed mirror of auto-memory, safe to *read* — but never a prerequisite to copy anywhere.** If a fact is load-bearing for operating the project, it belongs in (or is pointed to from) the durable repo docs above, not only in auto-memory.
- When a Claude session does update an auto-memory file, mirror it into the ops sidecar's `claude-memory/` and commit THERE (the product repo no longer carries it — public-flip hygiene).

## Decisions explicitly deferred

- Multi-agent coordination model and isolation (worktrees vs containers vs ?)
- Remote transport (architecture supports it; specific transport TBD)
- MCP as internal protocol (not used in phase 1; may revisit)
- Embedded editor (shell out for now)
- Configuration mechanism for user preferences
- Automatic plot capture from REPL (manual save in phase 1)

## Open questions

- **AST hash exact algorithm**: walk `JuliaSyntax.GreenNode`, skip trivia, hash kind+text. Pin a `JuliaSyntax` version; treat hash format as part of cache invalidation.
- **Project root detection**: `Project.toml` is the anchor. Multi-`Project.toml` repos: pick one, document the rule, allow override via project config.
- **Plugin discovery**: explicit `concept_extensions = ["MyExt"]` key in project config (predictable) vs auto-scan loaded packages for `ConceptExplorerCore` dependents (magical). Leaning explicit.
- **Tool registration timing**: orchestrator learns tools at session start (simple) vs dynamically as plugins load (nicer). Static for phase 1.
- **REPL streaming protocol**: length-prefixed JSON frames over stdio with structured event types (stdout, stderr, value, image, error). Borrow IJulia's protocol shape, simplified.

## Repository layout (target)

```
Ship of Tools/
  requirements.md         # source of truth for scope
  CLAUDE.md               # this file — design and conventions
  Project.toml            # the umbrella Julia package
  
  core/                   # Julia: ConceptExplorerCore.jl
    src/                  # abstract types, IR, dispatch contracts
    
  julia/                  # Julia kernel + standard plugins
    kernel/               # plugin host process entry point
    plugins/
      julia-source/       # JuliaSource FileType plugin
      markdown/           # MarkdownDoc plugin
      modules-mode/       # Modules Mode plugin
      files-mode/         # Files Mode plugin
      
  rust/                   # Rust workspace
    frontend/             # ratatui TUI binary
    backend/              # daemon binary
    protocol/             # shared types for the JSON line protocol
    
  repl/                   # Julia REPL shim (display protocol, framing)
  
  examples/
    plugins/              # demo external plugins (HDF5 preview, etc.)
```

This layout is a target, not gospel. The first commits will likely be smaller — start with `core/`, the Rust workspace skeleton, and a single end-to-end skeleton that can render an empty Files mode.
