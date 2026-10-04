# rust/backend/src/rows/spawn: launching a row's supervisor (rows)

The daemon starts a row's `sot-capsule supervise` here: it qualifies the state root, checks the state dir against the
project, builds the supervise command and detaches it so the supervisor outlives the daemon. On Linux the supervisor
runs in a systemd user scope of its own, and the scope is the kill domain for everything the row started. Part of the
daemon's rows subsystem; charter: `rust/backend/src/rows/CLAUDE.md`.

## Files
- `mod.rs`: declares the folder's modules and carries the row-scope doc
- `state_root.rs`: `STATE_ROOT_HINT`, `state_dir_for`, `qualified_state_root`, `state_root_inside_project` and the per-OS volume probes
- `detach.rs`: `StartMode`, the supervise flags, the `sot-capsule` sibling check and `spawn_detached_supervisor` with its three `spawn_detached` arms
- `row_scope.rs` (Linux): the row's scope record, `capture`, `listed` and the aimed `end`
- `row_scope_aim.rs` (Linux): `aim`, the pure rule that decides which scope may be killed; no dependencies

## Start here
`detach.rs::spawn_detached_supervisor` for how a launch is built and refused; `row_scope.rs::end` for how a row's
scope is closed.

## Rules
- The daemon never creates a row's state dir: it passes the path to `sot-capsule supervise`, which creates it
  (`spawn_detached_supervisor`).
- The state root is qualified before every launch (`qualified_state_root`), and a state dir never lies inside its
  project (`state_root_inside_project`).
- Every spawn holds a run-gate permit: `StartPermit` is `spawn_detached_supervisor`'s first parameter.
- Without an escape from the daemon's kill domain the supervisor launches degraded and the daemon logs it
  (`spawn_detached`).
- A supervisor survives a daemon restart only inside its own user scope (`spawn_detached`, Linux arm).
- A scope is killed only after `row_scope_aim::aim` accepts it (`row_scope::end`).
