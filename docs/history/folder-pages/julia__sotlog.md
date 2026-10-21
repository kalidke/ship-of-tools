# julia/sotlog: SotLog, the Julia golden reader of the voyage segment format (capsule)

SotLog reads sealed `.sotseg` segment files written by the Rust `sot-log` crate. It is the Julia half of ADR 0039's
cross-language gate: Rust writes the golden fixtures, this package reads them. It has no writer, treats every input as a
sealed segment, and throws one exception type per failure category. Part of the capsule subsystem; charter:
rust/log/CLAUDE.md. CI runs its tests with `Pkg.test()` for this package.

## Files
- `Project.toml`: the package manifest (name, uuid, version, dependencies, `Test` as the test target).
- `src/SotLog.jl`: the module: record walk, segment parse, seal verification and the `SotLogError` exception types.
- `test/runtests.jl`: golden-fixture tests plus whitebox corruption tests; finds `golden-*.sotseg` fixtures under
  rust/log/tests/fixtures by walking up from the package.

## Start here
`read_segment` and `verify_seal` (the exports in `src/SotLog.jl`), for any change to the segment format.

## Rules
- A format change lands in the Rust writer (`sot-log`) and in this reader together, as ADR 0039 specifies; the golden
  fixtures in rust/log/tests/fixtures are never rewritten.
- The reader accepts only sealed segments: a byte after the seal record is `CorruptRecordError`, a provable tear is
  `TornTailError`; it does not implement the writer-side recovery states.
- SotLog's own dependencies are CRC32c, JSON3 and SHA (`Project.toml`).
