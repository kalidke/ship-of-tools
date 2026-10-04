# rust/log/src/capsule/producer/conpty: the Windows producer (capsule)

The owned ConPTY layer: pseudoconsole, pipes, the anonymous containment job and the spawned process, plus
`ConptyProducer`, the thin `Producer` implementation over them. Windows only. Part of the log subsystem; charter:
rust/log/CLAUDE.md.

## Files
- `mod.rs`: the ConPTY, job and process primitives (`ConptySpawn`, `AnonymousJob`, `PrimaryProcess`, `Pseudoconsole`)
- `producer.rs`: `ConptyProducer`, `impl Producer` by delegation to those primitives

## Start here
`ConptySpawn` in `mod.rs` for how a child is created inside the job; `producer.rs` for what the writer loop calls.

## Rules
- Windows only: both files are `#![cfg(windows)]`.
- Exit codes stay raw unsigned u32 end to end (`PrimaryProcess::exit_code_after_confirmed_exit`).
