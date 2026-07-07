# ADR 0005: AST hash algorithm

**Status:** Accepted
**Date:** 2026-05-07

## Context

`.concept/` annotations need to detect when the code they describe has meaningfully changed. Whitespace and comment edits should not mark annotations stale; structural edits should. We need a stable, fast, cheap hash keyed on the parsed AST of the targeted entity.

## Decision

Walk the `JuliaSyntax.GreenNode` tree of the entity depth-first. For each *non-trivia* node, emit `(kind, text)`. Trivia (whitespace, comments outside docstrings) is skipped. Feed the byte stream to SHA-256, take the first 16 bytes as hex (`32` chars).

Pin `JuliaSyntax = "~1.0"` in `core/Project.toml`. Frontmatter on every annotation carries `hash_v: 1`. Bumping `hash_v` triggers mass invalidation; format is part of the cache contract.

## Consequences

- Whitespace outside string literals does not change the hash → reformatting is invisible. Correct.
- Whitespace *inside* docstrings or string literals does change the hash → docstring edits mark annotations stale. Correct (a meaningful docstring edit is a meaningful change).
- Variable renames change the hash (text differs). Correct.
- A property test in `core/test/test_ast_hash.jl` enforces these invariants — it must pass before any AST hash code merges.
- Future `JuliaSyntax` bumps may change `GreenNode` shape. If so: bump `hash_v`, add a one-time migration sweep that recomputes all `synced_against` fields and marks every annotation stale. Document in a follow-up ADR.
