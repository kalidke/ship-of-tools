# rust/backend/src/sidecars/repl: the Julia REPL child of each row (sidecars)

Each row can hold one persistent Julia process that evaluates the user's code in its own `Main`, apart from the kernel
so a runaway eval cannot take introspection down. One daemon task owns the child from spawn to reap; callers submit and
get frames back over the broadcast bus, or a collected reply. Part of the sidecars; charter:
rust/backend/src/sidecars/CLAUDE.md.

## Files
- `execute.rs`: repl.execute, the whole-report run
- `execute_tests.rs`: native `repl.execute` submit-failure and res-only drawer completion, plus the existing Unix shell reply/frame table.
- `ops.rs`: repl.eval, repl.run_file, repl.interrupt, and their stub-child tests (interrupt reach, a full queue, a fresh run's announcement and a waiting run's answer on a restart)
- `mod.rs`: the handle (`Repl`): submit, execute, interrupt and restart, the frame bus message and `ExecAccum`.
- `lifecycle.rs`: the child's state (`ReplLifecycle`), spawn generations and the `lifecycle` frames.
- `project_tests.rs`: real Julia bare/project workspace, package-write destination and all-entry spawn controls in isolated resource/depot fixtures, whose first start compiles only the shim. Real owned-child parsed-argument/environment controls, and WGL page-secret exclusion controls that read every process of the REPL's tree (on Windows the Julia root's own command line cannot be read, so there the descendants alone), with a deliberate-leak sensitivity probe that finds a needle in the root and in a descendant, a killed child's temporary files removed with it, and (Linux) the child's own display, which only its cookie opens and which ends with the child on a restart, a death, a row's end and the daemon's stop; no source-text assertions.
- `restart_tests.rs`: noncooperative restart, retained senders, channel close, retired-generation grants and selected-project respawn, with owned child observations.
- `tree_argv_tests.rs`: the command-line observer the page-secret and spawn controls share: every process of a child's tree (Linux and macOS its process group; Windows a walk of one CIM process listing that follows a parent edge only to a process created after that parent, since Windows reuses process ids) and each one's command line, with the walk's table test.
- `supervisor.rs`: spawning the child and `supervisor_task`, its life: wire, routing and close-out on death.

## Start here
mod.rs `Repl::ensure_supervisor` for when a child starts; `supervisor_task` for its life.

## Rules
- Liveness is the child's state, not the channel's: `Repl::ensure_supervisor` respawns unless the sender is open and
  the state is not `Dead`.
- `Repl` retains the current selected project and one supervisor owner: submission, explicit stop and checked retirement join. No replacement starts before successful retirement; a cleanup error or `REPL_RESTART_WAIT` join timeout is visible and retains the retiring owner for reconciliation.
- Stop, channel close, child exit, pipe failure and shutdown share pending/streamed closeout and browser-grant revocation. The closeout names why the supervisor ended (`ReplEnded`): a restart, a stop or the child's death, and only a death says the REPL died; `repl.execute` answers a restart `restarted`. Contained-tree termination requests are checked and the direct child is confirmed reaped before successful retirement; escaped descendants remain ADR 0050 residual 7.
- A supervisor is joined before a replacement generation opens, so its retired output cannot restore a revoked browser grant. The existing lifecycle generation guard still bounds state transitions. The child's exit ends supervision independently of inherited pipes.
- `Repl::request_if_running` never spawns (`repl.interrupt` must not pay a spawn to answer "not running").
- No submission waits on the child: one to a full queue is refused at once (`enqueue`, `ReplBusy`), and
  `repl.interrupt` waits at most `REPL_INTERRUPT_WAIT` (10 s) for the child's answer.
- The REPL ops answer an error with one of these codes: `repl_busy` (the queue was full), `repl_eval_failed`,
  `repl_run_file_failed`, `repl_interrupt_failed` and `repl_restart_failed`; `repl.execute` reports a full queue as its
  outcome `busy` (ADR 0033).
- A lifecycle transition applies only for the current spawn generation (`lifecycle_transition`), and a generation
  opens only after `.spawn()` succeeds (`lifecycle_begin_starting`).
- A child's browser ports are recorded as its `browser` frames pass (`route_line`) and revoked when a new generation
  starts or the current one dies.
- `Repl` keeps its constructor's `Signal` and passes it to each supervisor's spawn and shutdown wait.
- The shim project is resolved at every spawn (`Repl::repl_project`), never cached. `spawn_supervisor` is the only REPL spawn recipe: user code's active project and cwd are the selected user directory, even before it has a `Project.toml`; the shim is a fallback on `JULIA_LOAD_PATH`, never the bare-workspace active project.
- Once `repl.execute` announces `Started`, every returned report closes that run: a submit failure or collected result without `Done` gets a synthetic `Done`, after an `error` frame with the report's error unless the run timed out (`close_exec_run`); an already completed result gets no duplicate from the handler. Validation before `Started` creates no drawer run.
- A `repl.run_file` with no `eval_id` (`sot-fe repl run --fresh`) is numbered from `repl.execute`'s space and announced with `Started` after any restart and before its submit (`handle_repl_run_file`), since a window makes a drawer entry only for its own evals and a `Started` frame; a submit failure gets a synthetic `Done`. A run_file with no path is refused (`bad_request`): the shim would answer it with a res alone, which this op drops.
- `repl.execute` output is collected loss-free per eval, text capped at `EXEC_TEXT_CAP` (`ExecAccum`).
- A test's Julia child never inherits the test binary's stdout or stderr (the leak probe in `project_tests.rs` nulls
  its stderr): Julia's runtime sets `O_NONBLOCK` on an inherited stdio pipe and never restores it, and the flag lives on
  the pipe's open file description, so every later writer to the job's shared stderr (the test binaries after it and
  their children) gets EAGAIN once the pipe is full.
