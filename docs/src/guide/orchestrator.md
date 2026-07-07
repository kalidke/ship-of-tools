# The Orchestrator

*This is the deep dive: the tool surface, the tool-use loop, and the
prompt-cache layout. For the pane's layout and how it fits alongside the REPL
day-to-day, see [Orchestrator Pane](panes/orchestrator.md).*

The orchestrator is the agent that operates Ship of Tools on your behalf. Beyond core
editor and REPL interactions, it is your primary means of driving the
environment: you describe what you want, and it reads code, writes code,
maintains [concept annotations](concept-layer.md), and runs code in the
[REPL](repl.md) to get there.

Today that agent is **Claude Code** — Ship of Tools is Claude Code–centric. Each agent
session is associated with a workspace on the **backend daemon**, so it sits
alongside project state and survives a frontend restart.

Multi-agent operation is **real today**: several Claude Code agents run
concurrently — one per workspace/session, across machines — coordinated over the
[inter-agent communication system](../features.md). What is still deferred is the
dedicated in-UI **Agents mode** (a tasks → timeline → step-detail view for
starting, pausing, resuming, and comparing agents); today you spawn and drive
them hands-on through the [Sessions view](modes.md) and the comm bus.

!!! note "Original design vs. today"
    The tool surface and prompt-cache layout below describe the original Ship of Tools
    phase-1 orchestrator design: a bespoke client talking to the Anthropic API
    directly. That client was never built — in practice the agent role is filled
    by **Claude Code**, which brings its own tools and context management.

## The tool surface

The orchestrator acts on the project through a fixed set of tools:

| Tool | What it does |
|------|--------------|
| `read_file` | read a file in the project |
| `write_file` | write a file in the project |
| `read_annotation` | read a `.concept/` annotation |
| `write_annotation` | write a `.concept/` annotation (stamps provenance) |
| `repl_eval` | evaluate code in the persistent [REPL](repl.md) |
| `list_modules` | list the project's modules from the index |

The tool set is static for the session — registered at session start. A
dynamically growing tool surface (plugin-defined tools appearing as packages
load) is a phase-2 seam, not a phase-1 capability.

## Streaming and the tool-use loop

The orchestrator streams its response token by token, and runs a **tool-use
loop**: it emits a tool call, the backend executes it, the result is fed back,
and the model continues — repeating until the model has no further tool calls
and finishes its turn. Tool calls surface in the frontend as collapsed
breadcrumbs, so you can see what it did (read this file, evaluated that
expression, wrote that annotation) without the transcript drowning in tool
plumbing.

Tool *results* — the contents returned by `read_file`, `read_annotation`, and so
on — flow through the conversation tail, not back into the cached context blocks
(see below). That keeps a large read from invalidating the cache.

## Prompt-cache layout

Orchestrator sessions are long and tool-heavy, and cost and latency are dominated
by repeated context. Ship of Tools talks to the Anthropic API directly and structures
the request around **four prompt-cache breakpoints**, each cached so that the
stable parts of the context are not re-sent every turn:

| Block | Contents | Invalidates when |
|-------|----------|------------------|
| 1 | System prompt + tool schemas | rarely — near-permanent cache |
| 2 | Project structural snapshot (module tree + file tree) | the index version bumps (i.e. on code change) |
| 3 | `.concept/` index — annotation paths + AST hashes only, *not* contents | an annotation is created/deleted or a hash changes |
| 4 | Conversation tail | uncached |

Block 1 is nearly permanent, giving a high cache hit rate across all turns.
Block 3 carries only paths and hashes, not annotation bodies, so it invalidates
rarely even in annotation-heavy work. This layout is Anthropic-specific; adding
another provider later means a per-provider adapter, not a redesign.

## Confirmation policy

**The LLM decides which actions need your confirmation.** Ship of Tools does not impose
its own permission tiers, sandboxes, or rule-based action gating — that policy
lives with the orchestrator, not in the environment around it. This is a
deliberate requirement: the environment trusts the LLM to mediate, and does not
wrap it in a separate gate.

This is also why the concept layer matters. The orchestrator is the primary
author of code *and* the maintainer of the artifacts that describe it, so the
two stay coupled: when it changes code, it is responsible for keeping the
[concept annotations](concept-layer.md) current, and you see drift it misses as a
stale badge.

## See also

- [Orchestrator Pane](panes/orchestrator.md) — the drawer's layout and how it fits in the UI.
- [The REPL](repl.md) — the interactive session the orchestrator drives via `repl_eval`.
- [The Concept Layer](concept-layer.md) — the annotations it authors and maintains.
