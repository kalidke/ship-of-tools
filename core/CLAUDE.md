# core: ConceptExplorerCore, the plugin ABI (sidecars)

Abstract types plus generic functions. A package extends the system by adding methods to them; core owns no state,
process, file or wire op. Part of sidecars; charter: rust/backend/src/sidecars/CLAUDE.md.

## Files
- `Project.toml`: the package manifest (module ConceptExplorerCore).
- `src/ConceptExplorerCore.jl`: the whole ABI: `FileType` (the one wired type, with `matches`, `preview` in a 2-arg and a 3-arg form that drops params, `file_types`, `file_type_for`); `Mode`, `ConceptEntity`, `AnnotationKind`, `Tool` and `Capture`, declared with no subtype anywhere; `TreeNode`, built only in core's tests because Rust builds every tree node; `PreviewPayload`, what `preview` returns, whose `data` the kernel re-encodes as base64. Its `using JSON3`, `JuliaSyntax` and `SHA` lines load packages core never uses.
- `test/runtests.jl`: the suite for the above.

## Start here
`file_type_for`, then `preview`.

## Rules
- `file_type_for` takes the first claimant in `subtypes(FileType)` order and skips types without `matches`, so no two
  plugins may claim one path.
- Keep the 3-arg `preview` fallback: the kernel always calls the 3-arg form.
- No I/O and no wire here.

Records: ADR 0006 (its not-implemented note), ADR 0021, docs/src/extend/abi.md and filetype.md.
