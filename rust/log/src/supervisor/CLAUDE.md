# rust/log/src/supervisor: the authority over a state dir's runs (capsule)

`sot-capsule supervise` is the one process that starts, ends, adopts and resets a run in a state dir. This folder holds
it: the entry points and exit codes, the leg it spawns, its lifecycle state machine, the lane it serves and the main loop.
Part of the capsule; charter: rust/log/CLAUDE.md.

## Files
- `mod.rs`: the whole authority today: entry points, numbers, leg, lifecycle, lane, main loop, one-shot end and reset

## Start here
`supervise` in `mod.rs` for the entry point, `supervise_inner` for the main loop.

## Rules
- One authority per state dir: `supervise_inner` takes `fence::lock_supervisor` before binding the lane, else exit 70.
- Exit codes 0, 69 and 70 are an interface (`EXIT_CLEAN`, `EXIT_TERMINAL`, `EXIT_CONTENDED`).
- Journal recovery runs before the pointer is read (`spawn_recovery`).
- The main loop never joins a stuck worker (`watchdog_expired`, `abandon_worker`).
- `Terminal` is sticky (`force_terminal`).
- A leg dies with its supervisor (`LegLease`).
- One write per diagnostic line (`note`).
