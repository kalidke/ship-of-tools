# rust/backend/src/sidecars/repl: the Julia REPL child of each row (sidecars)

Each row can hold one persistent Julia process that evaluates the user's code in its own `Main`, apart from the kernel
so a runaway eval cannot take introspection down. One daemon task owns the child from spawn to reap; callers submit and
get frames back over the broadcast bus, or a collected reply. Part of the sidecars; charter:
rust/backend/src/sidecars/CLAUDE.md.

## Files
- `mod.rs`: the handle (`Repl`): submit, execute, interrupt and restart, the frame bus message and `ExecAccum`.
- `lifecycle.rs`: the child's state (`ReplLifecycle`), spawn generations and the `lifecycle` frames.
- `supervisor.rs`: spawning the child and `supervisor_task`, its life: wire, routing and close-out on death.

## Start here
mod.rs `Repl::ensure_supervisor` for when a child starts; `supervisor_task` for its life.

## Rules
- Liveness is the child's state, not the channel's: `Repl::ensure_supervisor` respawns unless the sender is open and
  the state is not `Dead`.
- `Repl::request_if_running` never spawns (`repl.interrupt` must not pay a spawn to answer "not running").
- A lifecycle transition applies only for the current spawn generation (`lifecycle_transition`), and a generation
  opens only after `.spawn()` succeeds (`lifecycle_begin_starting`).
- A child's browser ports are recorded as its `browser` frames pass (`route_line`) and revoked when a new generation
  starts or the current one dies.
- The child's exit ends the supervisor, not its pipes: `supervisor_task`'s `child.wait()` branch is polled last.
- On death each streamed eval in flight gets a synthetic `error` and `done` frame.
- The REPL project is resolved at every spawn (`Repl::repl_project`), never cached.
- With a workspace project, user code runs in it with the shim on `JULIA_LOAD_PATH` and the project as cwd
  (`spawn_supervisor_with_project`).
- `repl.execute` output is collected loss-free per eval, text capped at `EXEC_TEXT_CAP` (`ExecAccum`).
