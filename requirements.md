# Ship of Tools — an Agentic Julia Development Environment — Requirements

*Draft v0.1 — problem and requirements only. Implementation choices are deliberately deferred.*

## Problem

Working with a Julia codebase — whether you wrote it yourself, an LLM wrote it, or you have just landed in someone else's repo — the developer's bottleneck is **comprehension**, not text manipulation. Conventional IDEs surface files and lines well but expose structure, types, runtime outputs, and the underlying math poorly. This hurts onboarding, code review, and debugging in any context, and is especially acute in LLM-assisted development, where the human never builds understanding line by line.

## Goal

A keyboard-driven Julia development environment built to be driven by AI agents: the agents write, run, and surface code while the developer steers, watches, and reviews. It preserves the conventional mechanics of finding, viewing, editing, and running any `.jl` file with a persistent REPL, and layers a **concept explorer** on top — letting a developer move fluidly between levels of abstraction (project, module, type, function, outputs, math). The LLM has a dual role: it writes and edits code, and it maintains the concept-explorer artifacts so they stay in sync with the codebase as it evolves.

## Users and scope

- Single user.
- One project at a time. A project is flexible in definition but is typically a git repository.
- Cross-platform: must run on Windows and Linux.
- Local and remote operation are both supported, and must offer the same user experience.

## Functional requirements

### Code authoring and execution

- The LLM is the primary author of code. The user may also edit code directly.
- The user must be able to find, view, edit, and run any `.jl` file in the project using only the keyboard.
- The user must be able to dispatch code to the REPL at multiple granularities.
- A persistent Julia REPL must be available throughout a session.

### Document authoring

- The user must be able to author markdown documents (such as PRDs) directly in the environment.

### File navigation and inspection

- The user must be able to navigate the entire project filesystem by keyboard alone.
- The environment must render previews of project files at appropriate fidelity for their type, beginning with PNG images, and including markdown, MP4 video, and JSON. Other figure and structured-data formats are within scope and may be added incrementally.
- The user must be able to pin a file to a persistent view that updates as the file changes on disk.

### Concept explorer

- The system must support inspection of, at a minimum: source code, type structure, program outputs, project and module structure, and mathematical content.
- The LLM must maintain — and keep current with the codebase — artifacts capturing project intent, module purpose, type meaning, function contracts, mathematical derivations, and data shapes.
- The user must be able to move between levels of abstraction, from project overview down to specifics, without leaving the environment.
- When a project has no existing concept-explorer artifacts (for example, when first opening an unfamiliar repo), generating them is the LLM's responsibility.

### Multi-agent operation

- Multiple LLM agents must be able to work on the project simultaneously without interfering with each other's in-progress changes.
- The user must be able to start, pause, resume, and stop agents.
- The user must always be able to see what each active agent is doing and what state it is in.
- The user must be able to inspect, after the fact, any agent's actions, reasoning, and outputs in enough detail to verify the work.
- The user must be able to compare, accept, or discard each agent's work product.

### LLM mediation and control

- The LLM is the user's primary means of operating the environment beyond core editor and REPL interactions.
- Decisions about which actions require user confirmation are the LLM's responsibility. The environment does not impose its own permission tiers, sandboxes, or rule-based action gating.

### Window and layout management

- The layout system must accommodate both ultrawide displays and small laptop displays (16:10) without per-device reconfiguration by the user.
- All layout operations must be possible from the keyboard.

## Quality requirements

### Reliability

- File navigation, content inspection, the editor, the REPL, and watch views must continue to function when the LLM service is unavailable.
- All static content — anything written to disk in the project — must survive a restart of the environment. In-memory state (REPL bindings, agent conversations, live processes) is not required to persist.

### Responsiveness

- Interactive operations — navigation, opening files, dispatching code to the REPL, switching views — must feel immediate.
- Long-running operations such as agent tasks may run in the background for arbitrary durations, including hours.

### Remote and local parity

- The user experience on a remote project must be indistinguishable from a local project: same surfaces, same capabilities, same interaction feel.

### Future-proofing

The system must avoid the following assumptions, all of which are expected to become limiting within the next year:

- A single LLM model or provider.
- Short-running operations only.
- Text-only inputs and outputs; visual artifacts must flow in both directions.
- A fixed tool surface available to the LLM.
- A single task or session active at a time.

## Out of scope

- Manual coding as the primary workflow. The LLM writes code; the user may intervene.
- IDE features whose purpose is to support manual authoring of code: language servers, completion, linting, formatting, refactoring tooling, and graphical debuggers.
- Mouse-driven interaction.
- Multi-user or collaborative use of the environment.
- First-class environment surfaces for runtime value inspection, dataflow visualization, and execution tracing. The user accesses runtime values through the REPL itself.
- A configuration surface specified at the requirements level. Configuration will be opinionated by design and is treated as an implementation concern.

## Deferred to implementation

These were raised during requirements discussion and are intentionally not specified here:

- The format, location, and update discipline of concept-explorer artifacts.
- The coordination model among parallel agents and the mechanism by which they are isolated.
- The specific bridge between the LLM and the REPL — what it can read, inject, and inspect.
- The window-management model that satisfies the layout requirements.
- The transport and architecture of the local-and-remote-parity experience.
- The configuration mechanism by which user preferences are expressed to the LLM.

## Amendment 2026-07-05 — distribution is in scope (ADR 0030)

This document originally predated any distribution story; going public made
one load-bearing. The following are now **in scope**, governed by ADR 0030
(and its clone-install amendment):

- **Public releases**: versioned, tag-driven, prebuilt binaries for Linux and
  Windows (macOS experimental), CI-gated by artifact smoke tests, a Julia
  env resolve+load check, and a full-history secret scan.
- **Installation**: a scripted installer (interactive or flag-driven) laying
  down binaries plus a repo checkout pinned at the release tag — the checkout
  doubles as the product's resource tree and its in-app manual.
- **Self-update**: installed instances detect, download, verify, and stage
  new releases; apply is staged-next-launch with rollback.

Unchanged: multi-user/collaborative use of one environment instance stays
out of scope; "distribution" means many single-user installs, not a shared
deployment.
