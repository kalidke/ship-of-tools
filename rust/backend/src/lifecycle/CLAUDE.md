# rust/backend/src/lifecycle: lifecycle (charter)

## Idea
Nothing outlives its owner unless designed to: every child process the daemon starts has one owner that selects on a
process-wide signal, every exit is bounded on the OS clock, and the last window on a computer decides, through its
lease, whether that computer's sessions end (ADR 0050).

## Owns
- The window leases and `<state>/held.json`: `Leases`, `read_record`, `write_or_delete` (`crate::lifecycle::lease`, the file
  `lease.rs` here).
- The lease ops `fe.lease`, `fe.leaving`, `fe.notice_seen` (`lease::hold`) and the 1 s `lease::ticker`.
- The start plan Resume, Pending or Cleanup: `startup::begin`, `lease::startup_plan`.
- The close and its backstop `exit(1)`: `shutdown::run`, `shutdown::end_rows`.
- The child signal: `Signal`, `ChildGuard`, `fire`, `fired`, `live_children`.
- The bounds and exit codes in `sot_protocol::ops::lease`.
- The window's half, rust/frontend/src/lease.rs.

## Promises
- A lease is granted only to a peer whose pid, creation time and boot equal what the OS reported at accept
  (`lease::claim`, called by `Leases::grant`).
- Deadlines are wall-clock unix milliseconds, so a persisted handover deadline survives a restart (`startup_plan`
  reads `handover_until_ms` as written).
- `held.json` is deleted when every field is empty or false and otherwise written through `crate::durable`
  (`write_or_delete`).
- The start plan is a function of the record and this boot alone, never of a process lookup (`startup_plan`); an
  unreadable record, a closing record, another boot or an expired handover plans Cleanup.
- Cleanup ends every row and resumes none (`startup::cleanup`); Resume and Pending resume rows at once
  (`rows::run::resume::resume_all`).
- The close stops accepting before it touches a row (the accept loop in `server::run` breaks on `Leases::gone`, drops
  the listener, then calls `shutdown::run`), ends rows without resuming any (`end_rows`), counts each row not confirmed
  ended, and a backstop thread exits 1 at `bounds::SHUTDOWN_BOUND` (`shutdown::run`, step 0).
- A close that finishes exits 0 (`bounds::EXIT_REQUESTED_SHUTDOWN`); the update restart exits 75 and only while no
  shutdown has begun (`Leases::while_open`, called by update.rs).
- `fire()` is permanent: the signal is never reset for the life of the process (`Signal::fire`).
- A window started with `--ephemeral`, `--capture` or `--no-lease` never leases (the frontend's `lease_exempt`).

## Connections
Each connection is one row of docs/integration.md, owned by its provider. Provides: `startup::begin`, `lease::ticker`,
`Leases::gone`, `shutdown::run`, `fe.lease`, `fe.leaving`, `fe.notice_seen`, `rust/frontend/src/lease.rs`,
`Leases::before_data_connection`, `scripts/sot-lease.ps1`, `Leases::while_open`, `ChildGuard`, `Signal`,
`child_signal::fired`, `child_signal::process`. Uses: `fe.lease`, `handle_connection`, `lease::hold`, `admit_peer`,
`reject`, `write_frame_within`, `write_frame_to`, `destroy_capsule_workspace`, `end_default_row_run`, `resume_all`,
`close_gate_and_settle`, `remove_row_files`, `sot_state_dir`, `sot_config_dir`, `host_name`, `state_dir_hash`,
`durable::write`, `durable::remove`, `rust/backend/src/durable.rs`, `deploy/sotd.service`, `sot-apply.sh`.

## Folders
- `rust/backend/src/lifecycle/`: this folder.
- `rust/frontend/src/lease.rs`: a file, the window's half.

## Files
- `child_signal.rs`: the process-wide `fired` flag and the live-child count.
- `lease.rs`: the window lease: `Leases`, the grant rule, the lease connection (`hold`), `held.json` and the start plan.
- `lease_tests.rs`: tests of the grant rule, departures and ticks, held.json, the start plan and the lease connection.
- `mod.rs`: declares the four modules.
- `shutdown.rs`: the close, its backstop and the row ends.
- `startup.rs`: the start's decision from `held.json` and acting on it.

## Start here
`startup::begin` for what a start does; `shutdown::run` for the close and its order; `child_signal.rs` before a change
to how a child is owned.
