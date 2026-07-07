# ADR 0008: Project root detection

**Status:** Accepted
**Date:** 2026-05-07

## Context

The user runs `sot` from anywhere inside (or outside) a project. The system needs an unambiguous answer to "what is the project I'm working on?"

## Decision

Walk upward from `cwd` until a `Project.toml` is found. That directory is the project root.

In monorepos with nested `Project.toml` files (umbrella + sub-packages), the *outermost* one (highest in the tree) is chosen. Override via `--project <path>` CLI flag on the backend binary.

If no `Project.toml` is found by the time we hit the filesystem root, the backend exits with an error message instructing the user to run inside a Julia project or pass `--project`.

## Consequences

- Predictable in flat repos (the common case): exactly one `Project.toml`, exactly one answer.
- Monorepos: outermost wins by default, override is one flag away. Documented behavior beats clever heuristics.
- No multi-project sessions in Phase 1 — one backend, one project, one kernel. Phase 2 may revisit if remote-multi-project becomes a real ask.
- The kernel loads the chosen `Project.toml` environment (`Pkg.activate(<root>)`). Plugin discovery (ADR 0006) reads `[sot].extensions` from this same file.
