# rust/backend/src/rows/spawn: launching a row's supervisor (rows)

The daemon starts a row's `sot-capsule supervise` here: it qualifies the state root, checks the state dir against the
project, builds the supervise command and has it forked by the durable parent (`durable/`) after the row's authority
fence is claimed, so the supervisor outlives the daemon and a birth in flight cannot lose the fence to a second one. On
Linux the supervisor runs in a systemd user scope of its own, and the scope is the kill domain for everything the row
started. On Windows the daemon creates the supervisor itself, which takes the fence at its first act. Part of the
daemon's rows subsystem; charter: `rust/backend/src/rows/CLAUDE.md`.

## Files
- `mod.rs`: declares the folder's modules and carries the row-scope doc
- `state_root.rs`: `STATE_ROOT_HINT`, `state_dir_for`, `qualified_state_root`, `state_root_inside_project` and the per-OS volume probes
- `detach.rs`: the supervise flags over `sot_log::supervisor::StartMode`, the `sot-capsule` sibling check and `spawn_detached_supervisor` with its three `spawn_detached` arms (`Spawn`: started, or contended)
- `durable/` (Unix): the capsule-only birth parent: the daemon's end, the parent's loop, one launch's acceptance and the private channel (own page)
- `row_scope.rs` (Linux): the row's scope record, `capture`, `listed` and the aimed `end`
- `row_scope_aim.rs` (Linux): `aim`, the pure rule that decides which scope may be killed, and `v2_root`, where the host mounts its cgroup v2 hierarchy; no dependencies

## Start here
`detach.rs::spawn_detached_supervisor` for how a launch is built and refused; `row_scope.rs::end` for how a row's
scope is closed.

## Rules
- The daemon never creates a row's state dir: on Unix the durable parent creates it, and takes the row's fence in it,
  before it forks anything (`durable::accept::accept`); on Windows `sot-capsule supervise` creates it. A refused launch
  leaves a state dir without a pointer, which reads as never started.
- The state root is qualified before every launch (`qualified_state_root`), and a state dir never lies inside its
  project (`state_root_inside_project`). A full volume qualifies only a row that has run (its `supervisor.lock` exists), whose supervisor holds its fence and waits for storage; `workspace.create` refuses a full volume (`preflight_verdict`, `row_has_run`).
- The account half of the spawn env comes from `agents::env::account_spawn_env`, which refuses a missing account
  folder; the spawner passes its error on at once (`spawn_detached_supervisor`).
- Every spawn holds a run-gate permit: `StartPermit` is `spawn_detached_supervisor`'s first parameter.
- Without an escape from the daemon's kill domain the supervisor launches degraded and the daemon logs it
  (`spawn_detached`).
- A supervisor survives a daemon restart only inside its own user scope (`spawn_detached`, Linux arm).
- A launch whose row's fence is already claimed forks nothing and answers `Spawn::Contended` (`spawn_detached_supervisor`);
  the caller reports the authority pending (`rows/run/admission.rs`), never success and never a replacement.
- A scope is killed only after `row_scope_aim::aim` accepts it (`row_scope::end`).
- A scope is read and killed under the host's cgroup v2 root, `/sys/fs/cgroup` or, on a hybrid host,
  `/sys/fs/cgroup/unified` (`row_scope_aim::v2_root`); the test helpers read it through the same file.
- The user-scope probe runs through `Signal::spawn_std`; on its timeout `ContainedStd::wait_within` kills its tree
  before it reaps it (`user_scope_available`).
