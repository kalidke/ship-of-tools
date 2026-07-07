# ADR 0007: Tool registration timing

**Status:** Accepted
**Date:** 2026-05-07

## Context

The orchestrator's tool surface comes from core (Phase 1) and eventually plugins (Phase 2). When does the orchestrator learn about tools — once at session start, or dynamically as plugins load?

## Decision

Static at session start.

After the kernel finishes loading all extensions (per ADR 0006), it computes the full tool list and sends it to the backend. The list is frozen for the session. Reload requires kernel restart.

For Phase 1, all tools live Rust-side in `rust/backend/src/orchestrator/tools.rs`, wrapping existing kernel ops (`fs.read`, `fs.write`, `concept.read`, `concept.write`, `repl.eval`, `mode.tree.children` for `list_modules`). The Julia-side `Tool` abstract type is *defined* in `core/src/ToolSpec.jl` but unwired — that's the documented Phase 2 seam.

Phase 2 will add a `kernel.tools.list` op that aggregates `tool_spec(::Type{<:Tool})` dispatches from all loaded plugins and merges them with the core Rust-side tools.

## Consequences

- Simpler orchestrator state: tool list is immutable per session.
- Adding a plugin tool means restarting the kernel, which restarts the orchestrator session. Acceptable for Phase 1.
- Anthropic prompt cache (ADR 0004 block 1) treats the tool schema as near-permanent — static registration aligns naturally.
- If a Phase 1 tool needs to be added urgently, it goes in Rust. This biases toward fewer, well-chosen tools.
