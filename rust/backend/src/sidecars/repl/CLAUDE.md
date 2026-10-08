# rust/backend/src/sidecars/repl: the Julia REPL child of each row (sidecars)

Each row can hold one persistent Julia process that evaluates the user's code in its own `Main`, apart from the kernel
so a runaway eval cannot take introspection down. One daemon task owns the child from spawn to reap; callers submit and
get frames back over the broadcast bus, or a collected reply. Part of the sidecars; charter:
rust/backend/src/sidecars/CLAUDE.md.

## Files
- `execute.rs`: repl.execute, the whole-report run
- `execute_tests.rs`: repl.execute against a stub child: every reply and drawer frame pinned
- `ops.rs`: repl.eval, repl.run_file, repl.interrupt
- `mod.rs`: the handle (`Repl`): submit, execute, interrupt and restart, the frame bus message and `ExecAccum`.
- `lifecycle.rs`: the child's state (`ReplLifecycle`), spawn generations and the `lifecycle` frames.
- `project_tests.rs`: real Julia bare/project workspace, package-write destination and all-entry spawn controls in isolated resource/depot fixtures. Real owned-child argv/environment and WGL page-secret exclusion controls, with a deliberate-leak sensitivity probe; no source-text assertions.
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
- `Repl` keeps its constructor's `Signal` and passes it to each supervisor's spawn and shutdown wait.
- The child's exit ends the supervisor, not its pipes: `supervisor_task`'s `child.wait()` branch is polled last.
- On death each streamed eval in flight gets a synthetic `error` and `done` frame.
- The shim project is resolved at every spawn (`Repl::repl_project`), never cached. `spawn_supervisor` is the only REPL spawn recipe: user code's active project and cwd are the selected user directory, even before it has a `Project.toml`; the shim is a fallback on `JULIA_LOAD_PATH`, never the bare-workspace active project.
- `repl.execute` output is collected loss-free per eval, text capped at `EXEC_TEXT_CAP` (`ExecAccum`).
