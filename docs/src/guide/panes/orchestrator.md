# The Orchestrator Pane

*Bottom-left.* The orchestrator pane holds the **agent session** that operates Ship
of Tools on your behalf. You describe what you want; the agent reads code, writes
code, maintains [concept annotations](../concept-layer.md), and runs code in the
[REPL](repl.md) to get there. It is your primary means of driving the
environment beyond direct editor and REPL interactions.

This page covers the pane — what fills it, how it acts, its confirmation
policy. For the tool surface, the tool-use loop, and the prompt-cache layout in
depth, see [The Orchestrator](../orchestrator.md).

## What fills it

Today that agent is **Claude Code** — Ship of Tools is Claude Code–centric. Each
agent session is associated with a workspace on the **backend daemon**, so it sits
alongside project state and **survives a frontend restart**. (When Ship of Tools
is developed on itself, the dev agent runs in the [Terminal drawer](terminal.md)
instead.)

Multi-agent operation is **real today**: several Claude Code agents run
concurrently — one per workspace/session, across machines — coordinated over the
[inter-agent communication system](../../features.md). You start and switch them
from [Sessions mode](navigation.md) (`s`); cycle the active workspace with
`Shift+Arrow`. What is still deferred is a dedicated in-UI **Agents mode** (a
tasks → timeline → step-detail view).

## How it acts

- **Its own tool surface.** Today the agent is Claude Code, which brings its own
  tools and context management. The original design's fixed, session-start tool
  set (read/write files, read/write `.concept/` annotations, evaluate code in the
  REPL, list modules) was never built as a bespoke client — see
  [The Orchestrator](../orchestrator.md).
- **Confirmation is the LLM's call.** Ship of Tools imposes no permission tiers,
  sandboxes, or rule-based gating of its own — that policy lives with the agent,
  not the environment around it.
- **Coupled to the concept layer.** Because the agent is the primary author of
  code *and* the maintainer of the artifacts describing it, the two stay coupled;
  drift it misses surfaces as a stale badge.

## See also

- [The Orchestrator](../orchestrator.md) — the tool surface, the tool-use loop, and the prompt-cache layout in depth.
- [The REPL](repl.md) — the interactive session the agent drives.
- [The Concept Layer](../concept-layer.md) — the annotations it authors and keeps current.
