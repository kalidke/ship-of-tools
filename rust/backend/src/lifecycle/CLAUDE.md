# rust/backend/src/lifecycle: lifecycle (charter)

## Idea
Nothing outlives its owner unless designed to: every child process the daemon starts has one owner that selects on a
process-wide signal, every exit is bounded on the OS clock, and the last window on a computer decides, through its
lease, whether that computer's sessions end (ADR 0050).

## Owns
- The window leases and `<state>/held.json`: `Leases`, `read_record`, `write_or_delete` (`crate::lease`, still the
  file rust/backend/src/lease.rs; it joins this folder in a later unit).
- The lease ops `fe.lease`, `fe.leaving`, `fe.notice_seen` (`lease::hold`) and the 1 s `lease::ticker`.
- The start plan Resume, Pending or Cleanup: `startup::begin`, `lease::startup_plan`.
- The close and its backstop `exit(1)`: `shutdown::run`, `shutdown::end_rows`.
- The child signal: `Signal`, `ChildGuard`, `fire`, `fired`, `live_children`.
- The bounds and exit codes in `sot_protocol::ops::lease`.
- The window's half, rust/frontend/src/lease.rs.

## Promises
- A lease is granted only to a peer whose pid, creation time and boot equal what the OS reported at accept, and whose
  token matches when one is expected (`lease::claim`, called by `Leases::grant`).
- Deadlines are wall-clock unix milliseconds, so a persisted handover deadline survives a restart (`startup_plan`
  reads `handover_until_ms` as written).
- `held.json` is deleted when every field is empty or false and otherwise written through `crate::durable`
  (`write_or_delete`).
- The start plan is a function of the record and this boot alone, never of a process lookup (`startup_plan`); an
  unreadable record, a closing record, another boot or an expired handover plans Cleanup.
- Cleanup ends every row and resumes none (`startup::cleanup`); Resume and Pending resume rows at once
  (`capsule_workspace::resume_all`).
- The close stops accepting before it touches a row (the accept loop in `server::run` breaks on `Leases::gone`, drops
  the listener, then calls `shutdown::run`), ends rows without resuming any (`end_rows`), counts each row not confirmed
  ended, and a backstop thread exits 1 at `bounds::SHUTDOWN_BOUND` (`shutdown::run`, step 0).
- A close that finishes exits 0 (`bounds::EXIT_REQUESTED_SHUTDOWN`); the update restart exits 75 and only while no
  shutdown has begun (`Leases::while_open`, called by update.rs).
- `fire()` is permanent: the signal is never reset for the life of the process (`Signal::fire`).
- A window started with `--ephemeral`, `--capture` or `--no-lease` never leases (the frontend's `lease_exempt`).

## Connections
- In from the server: `server::run` calls `startup::begin` before the listener binds, spawns `lease::ticker`, hands a
  connection whose first frame is `fe.lease` to `lease::hold`, and on `Leases::gone` calls `shutdown::run`.
- Out to rows: `shutdown::end_row` and `end_drawer` call `handlers::destroy_capsule_workspace`,
  `handlers::end_default_row_run`, `handlers::remove_comm_agents_for_workspace`; `startup::forget_rows` calls
  `handlers::remove_row_files`; the plan calls `capsule_workspace::resume_all`; the drawer's end goes through
  `capsule_workspace::end_run`.
- Child owners hold a `ChildGuard` or await `fired()` (`shutdown::process()` is passed in): `kernel`, `repl`, `pluto`,
  `mathjax`, `monitor`, `hub_link`, the quarto run in `handlers`, and `topology_dial`.
- `update.rs` exits through `Leases::while_open`.
- The window: rust/frontend/src/lease.rs holds the lease connection (`notice`, `owed`, `leave_all`, `Leaving::poll`).
  The Windows launcher holds a lease with hand-written frames (scripts/launch-sot.ps1).

## Folders
- `rust/backend/src/lifecycle/`: this folder.
- `rust/frontend/src/lease.rs`: a file, the window's half.

## Files
- `mod.rs`: declares the two modules.
- `shutdown.rs`: the close, its backstop, the row ends and the child signal.
- `startup.rs`: the start's decision from `held.json` and acting on it.

## Start here
`startup::begin` for what a start does; `shutdown::run` for the close and its order; the child signal (`Signal`,
`ChildGuard`) in `shutdown.rs` before a change to how a child is owned.
