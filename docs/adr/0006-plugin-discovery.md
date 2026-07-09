# ADR 0006: Plugin discovery

**Status:** Accepted — **not yet implemented** (as of v0.3.2, 2026-07-09)
**Date:** 2026-05-07

> **Implementation status.** The `[sot].extensions` / `Base.require` mechanism
> below is design, not shipped code. What the kernel actually does today:
> (a) the seven standard plugins are eagerly `using`-ed at kernel startup,
> (b) `LAZY_PLUGIN_FOR_EXT` (a hardcoded extension→package table in
> `ShipToolsKernel.jl`) lazy-loads registered heavy plugins (HDF5Preview) on
> first preview, and (c) the `plugins.load` kernel op loads any package already
> on the kernel's load path by name. Implementing this ADR replaces (b)'s
> hardcoded table and gives third-party plugins a declarative path in.

## Context

Plugins extend the dispatch tables on `FileType`, `Mode`, `ConceptEntity`, `AnnotationKind`, `Tool`, `Capture`. Two reasonable approaches: (a) auto-scan loaded packages for those that depend on `ConceptExplorerCore`; (b) explicit list in project config.

CLAUDE.md leans explicit; this ADR commits to it.

## Decision

The project's `Project.toml` carries a `[sot]` table:

```toml
[sot]
extensions = ["HDF5Preview", "MyOrgPlugin"]
```

The kernel calls `Base.require` on each entry at startup, in declared order, before serving any requests. Anything not listed is not loaded, even if installed in the active environment.

## Consequences

- Predictable. No magic load order. No "why is this plugin active?" debugging.
- Adding a plugin is an explicit user action: install, add to `extensions`, restart kernel. Three steps, all visible.
- No auto-discovery means new plugins are discoverable only via documentation. Plugin READMEs must lead with the install + enable snippet.
- Compatible with ADR 0007 (static tool registration): the full plugin set is known at kernel startup.
- Phase 2 may add a `--list-installed-extensions` diagnostic to surface candidates the user hasn't enabled yet.
