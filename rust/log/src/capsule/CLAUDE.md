# rust/log/src/capsule: the leg, one producer recorded into its voyage (capsule)

The capsule is one process babysitting one producer and writing its voyage: the writer loop drives any
`Producer` through the output budget, the input WAL, the run-end marker and the attach protocol. This folder is
the leg's runtime; its types, constants and `run` are re-exported as `sot_log::capsule::*`. Part of the log
subsystem; charter: rust/log/CLAUDE.md.

## Files
- `mod.rs`: the leg's public types, limits and self status, and the whole writer loop `run`

## Start here
`run` in `mod.rs` for any change to what the leg records or when it commits; `CapsuleConfig` for what a caller
sets.

## Rules
- Committed output reaches subscribers only after the fsync that made it durable (`flush_output!` in `run`).
- Input is recorded redacted and its `forward_intent` fact is fsynced before the bytes reach the producer
  (`run_input_wal`).
- The run-end marker is appended at most once and latches only after a successful append
  (`commit_run_end_marker`).
- Every exit from `run` cancels the output budget (`BudgetCancelGuard`).
- Geometry outside 2x2..512x256 is refused (the `MIN_COLS`..`MAX_ROWS` constants, checked in `run`).
