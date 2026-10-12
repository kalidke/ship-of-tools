# core: ConceptExplorerCore, the plugin ABI (sidecars)

Abstract types plus generic functions. A package extends the system by adding methods to them; core owns no state,
file or wire op, and runs a process only in `read_within`. Part of sidecars; charter: rust/backend/src/sidecars/CLAUDE.md.

## Files
- `Project.toml`: the package manifest (module ConceptExplorerCore).
- `src/ConceptExplorerCore.jl`: the whole ABI: `FileType` (the one wired type, with `matches`, `preview` in a 2-arg and a 3-arg form that drops params, `file_types`, `file_type_for`); `Mode`, `ConceptEntity`, `AnnotationKind`, `Tool` and `Capture`, declared with no subtype anywhere; `TreeNode`, built only in core's tests because Rust builds every tree node; `PreviewPayload`, what `preview` returns, whose `data` the kernel re-encodes as base64. `read_within`, the one function that runs a process. Its `using JSON3`, `JuliaSyntax` and `SHA` lines load packages core never uses.
- `test/runtests.jl`: the suite for the above.

## Start here
`file_type_for`, then `preview`.

## Rules
- `file_type_for` takes the first claimant in `subtypes(FileType)` order and skips types without `matches`, so no two
  plugins may claim one path.
- Keep the 3-arg `preview` fallback: the kernel always calls the 3-arg form.
- No I/O and no wire here, except `read_within`.
- `read_within(cmd, seconds)` is `read(cmd)` with a bound: at the bound the process is killed (KILL, so a tool that
  ignores TERM ends too) and its output pipe closed, so a descendant holding it cannot hold the call; the error names
  the program and the bound; a plugin runs every external tool through it.

Records: ADR 0006 (its not-implemented note), ADR 0021, docs/src/extend/abi.md and filetype.md.
