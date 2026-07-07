# ADR 0004: LLM provider and prompt-cache layout

**Status:** Accepted
**Date:** 2026-05-07

## Context

The orchestrator drives long, tool-heavy sessions. Cost and latency are dominated by repeated context. Need streaming, tool use, and high cache hit rate.

## Decision

Anthropic API directly via `reqwest` against `/v1/messages`. No official Rust SDK exists; the surface we need is small (messages create with streaming + tool use). Model: `claude-opus-4-7`.

Four prompt-cache breakpoints (each with `cache_control: ephemeral` on the last block of its segment):

1. **System prompt + tool schemas.** Rarely changes. Near-permanent cache.
2. **Project structural snapshot.** Module tree + file tree at depth 2. Invalidates on index version bump.
3. **`.concept/` index.** Annotation paths + AST hashes only — *not* contents. Invalidates on annotation create/delete or hash change.
4. **Conversation tail.** Uncached.

Tool result content (returned by `read_file`, `concept.read`, etc.) flows through the conversation tail, not back into the cached blocks. Avoids cache thrash on every read.

## Consequences

- Block 1 nearly permanent → high cache hit rate across all turns.
- Block 2 invalidates on every code change that bumps the index version → some thrash during active editing, acceptable.
- Block 3 invalidates rarely → high hit rate for annotation-heavy workflows.
- This layout is Anthropic-specific. Adding a second provider later means a per-provider adapter, not a refactor of this scheme.
- Migration to a future model: change the model id; the cache layout is model-agnostic. Re-test cache TTL behavior — Anthropic has historically held this stable but check release notes.
