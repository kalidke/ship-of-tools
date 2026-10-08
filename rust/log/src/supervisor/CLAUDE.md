# rust/log/src/supervisor: the authority over a state dir's runs (capsule)

`sot-capsule supervise` is the one process that starts, ends, adopts and resets a run in a state dir. This folder holds
it: the entry points and exit codes, the leg it spawns, its lifecycle state machine, the lane it serves and the main loop.
Part of the capsule; charter: rust/log/CLAUDE.md.

## Files
- `mod.rs`: entry points (`supervise`, `endrun`, `reset`), exit codes and bounds, config, voyage paths, `note`
- `birth_claim.rs`: `BirthClaim`, the row's own supervisor fence taken before a capsule birth and carried to the new supervisor (Unix)
- `leg.rs`: the leg: voyage pointer discovery or mint, the spawn decision, `SpawnLease`, `LegLease`, `build_run_command`
- `lifecycle.rs`: the `Lifecycle` state machine, its recovery, end-run and reset worker threads, leg retirement, `force_terminal`
- `main_loop.rs`: `supervise_inner`, the authority's main loop
- `transitions.rs`: one tick of each `Lifecycle` state, called by the main loop
- `oneshot.rs`: `endrun_inner` and `reset_inner`, the fence-acquiring in-process callers
- `probe/`: is a leg there, and is it this user's: the `ProbeOps` seam, the classifier and the three OS implementations
- `lease_win.rs`: the Windows parent-death lease: a named mutex the supervisor owns for its whole life and the leg opens; abandoned means broken
- `journal/`: the durable records: operation journal, voyage pointer, authority fence
- `authority/`: the SOSV lane's server side: the authority's state and command handling, and the lane's connections

## Start here
`supervise` in `mod.rs` for the entry point, `supervise_inner` in `main_loop.rs` for the loop, `Lifecycle` in `lifecycle.rs` for a state change.

## Rules
- One authority per state dir: `supervise_inner` takes `fence::lock_supervisor` before binding the lane, else exit 70.
- Exit codes 0, 69 and 70 are an interface (`EXIT_CLEAN`, `EXIT_TERMINAL`, `EXIT_CONTENDED`).
- Journal recovery runs before the pointer is read (`spawn_recovery`).
- The main loop never joins a stuck worker (`watchdog_expired`, `abandon_worker`).
- `Terminal` is sticky (`force_terminal`).
- A leg dies with its supervisor (`LegLease`).
- On Unix SIGCHLD is reset first in `supervise_inner` and each leg is reaped once (`retire_leg`, `reap_retired_legs`).
- One write per diagnostic line (`note`).
