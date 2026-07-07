# ADR 0006: Plugin discovery

**Status:** Accepted
**Date:** 2026-05-07

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
