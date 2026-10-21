# rust/backend/src/sidecars/repl: the Julia REPL child of each row (sidecars)

Each row can hold one persistent Julia process that evaluates the user's code in its own `Main`, apart from the kernel
so a runaway eval cannot take introspection down. One daemon task owns the child from spawn to reap; callers submit and
get frames back over the broadcast bus, or a collected reply. Part of the sidecars; charter:
rust/backend/src/sidecars/CLAUDE.md.

## Files
- `escalation.rs`: `Escalation`, the interrupt signal for an eval that never yields: which eval, how long the signals go on, and the answer when they did not stop it.
- `execute.rs`: repl.execute, the whole-report run
- `execute_tests.rs`: native `repl.execute` submit-failure and res-only drawer completion, a run that times out behind a REPL still starting (Unix), plus the existing Unix shell reply/frame table.
- `interrupt_tests.rs`: real Julia: one interrupt stops a loop with a safepoint, a forced one a loop with none, one naming another eval sends nothing, and one during BLAS, a compile or a collection leaves the REPL alive.
- `ops.rs`: repl.eval, repl.run_file, repl.interrupt, the run announcement (`announce_run`), and their stub-child tests (interrupt reach by the daemon's numbers, a full queue, a fresh run's announcement, a waiting run's answer on a restart and on a death, and a window's run named running from its write until its end)
- `mod.rs`: the handle (`Repl`): submit, execute, interrupt and restart, the frame bus message and `ExecAccum`.
- `numbers.rs`: a run's number: the daemon's one counter (`next`) and a window's own numbers for its own runs and how far each has gone (`CallerNumbers`, kept per window by the server's roster), with their table tests.
- `lifecycle.rs`: the child's state (`ReplLifecycle`), spawn generations, the `lifecycle` frames, the notice the next child gives and the current child's start (`Start`), which `Repl::still_starting` reads.
- `project_tests.rs`: real Julia bare/project workspace, package-write destination and all-entry spawn controls in isolated resource/depot fixtures (a copy of the whole shim folder, `ext/` included), whose first start compiles only the shim. Real owned-child parsed-argument/environment controls, the shim's WGLMakie extension loading on every spawn route with `display(fig)` announcing its page through it, and WGL page-secret exclusion controls that read every process of the REPL's tree (on Windows the Julia root's own command line cannot be read, so there the descendants alone), with a deliberate-leak sensitivity probe that finds a needle in the root and in a descendant, a killed child's temporary files removed with it, and (Linux) the child's own display, which only its cookie opens, which ends with the child on a restart, a death, a row's end and the daemon's stop, a process the child started detached ending with it at a restart, and whose end under a live child ends that child and what it started, in its group or detached, the next eval saying why first; no source-text assertions.
- `restart_tests.rs`: noncooperative restart, retained senders, channel close, retired-generation grants and selected-project respawn, with owned child observations.
- `tree_argv_tests.rs`: the command-line observer the page-secret and spawn controls share: every process of a child's tree (Linux and macOS its process group; Windows a walk of one CIM process listing that follows a parent edge only to a process created after that parent, since Windows reuses process ids) and each one's command line, with the walk's table test.
- `supervisor.rs`: spawning the child and `supervisor_task`, its life: wire, routing, the steps of an `Escalation` and close-out on death.

## Start here
mod.rs `Repl::ensure_supervisor` for when a child starts; `supervisor_task` for its life.

## Rules
- Liveness is the child's state, not the channel's: `Repl::ensure_supervisor` respawns unless the sender is open and
  the state is not `Dead`.
- `Repl` retains the current selected project and one supervisor owner: submission, explicit stop and checked retirement join. No replacement starts before successful retirement; a cleanup error or `REPL_RESTART_WAIT` join timeout is visible and retains the retiring owner for reconciliation.
- Stop, channel close, child exit, pipe failure, the end of the child's display and shutdown share pending/streamed closeout and browser-grant revocation. The closeout names why the supervisor ended (`ReplEnded`): a restart, a stop, the child's death or its display's end, and only a death says the REPL died; `repl.execute` answers a restart `restarted` and every other end `repl_died`. A display's end (Linux) ends the child at once (`ChildTmp::display_ended`), and the next child, however it starts, says why at its first eval (`LifecycleCell::notice`, passed as `SOT_PREVIOUS_END`). Contained-tree termination requests are checked and the direct child is confirmed reaped before successful retirement; escaped descendants remain ADR 0050 residual 7, except that on Linux one that holds the child's display ends with it (display/).
- `REPL_RESTART_WAIT` (5 s) covers the old child's kill and reap, its display's end and the stderr tail's wait, and holds with
  margin for a large heap: measured on a 16 GB REPL (17.2 GB resident, three runs, Linux), the child and its Xvfb were gone
  1.15 to 1.18 s after the retire began, `restart_with_project` returned at 1.21 to 1.24 s, and the new child answered an
  eval 3.4 to 3.5 s after the retire began.
- A supervisor is joined before a replacement generation opens, so its retired output cannot restore a revoked browser grant. The existing lifecycle generation guard still bounds state transitions. The child's exit ends supervision independently of inherited pipes.
- The child runs with `--startup-file=no`; the shim's entry (`run_shim`) runs the user's startup file with stdout on stderr, so its prints never touch the wire (julia/repl/CLAUDE.md).
- `Repl::request_if_running` never spawns (`repl.interrupt` must not pay a spawn to answer "not running").
- No submission waits on the child: one to a full queue is refused at once (`enqueue`, `ReplBusy`), and
  `repl.interrupt` waits at most `REPL_INTERRUPT_WAIT` (10 s) for the child's answer.
- An eval that never yields holds the shim's one thread, so the shim reads no interrupt while it runs. A child that
  says nothing for 1 s with an interrupt unanswered is sent ONE interrupt signal (`Contained::interrupt`), which Julia
  raises at the eval's next safepoint and holds through C code, compiling and garbage collection (the stock REPL's
  Ctrl-C). Only a request with `force: true` (additive; absent means false: a window's second Ctrl+C, `sot-fe repl
  interrupt --force`) sends them 50 ms apart for 0.4 s on macOS and Windows and 0.8 s on Linux (`FORCE_SPAN`), which
  Julia forces into code with no safepoint and into C code, the compiler and the collector, where it can crash or hang
  the REPL. After a force Julia ignores signals for half a second and takes one after that as a new interrupt, raised in
  the shim's answer: on macOS and Windows it counts every signal on a thread of its own, so the burst ends inside that
  half second; on Linux it counts a signal only when the eval's thread runs between two, so under heavy CPU load
  forcing may not stop the eval, the span forces at most once, and a child that forced early and then wrote nothing for
  half a second takes the last signals as new interrupts, which the shim holds outside a user's code and drops, so its
  answers stay whole (julia/repl). The signals go only to the eval the child runs (the first written and still in flight,
  `in_order`) and only if the request names no evals or names it, only while it is in flight: never to an idle REPL,
  never to an eval the request did not name, never after the eval's `done`. Any line from the child restarts the
  quiet, one after a signal ends the signals, and the interrupt's answer ends the steps (`Escalation`). When the
  signals are over and a second has passed, or none could be sent, the daemon answers the request itself with an error
  that says what is left (`--force`, `--fresh`, the send error). The shim answers a request read after the signal
  stopped the eval `interrupted:true` for it (julia/repl `handle_interrupt`).
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
- The daemon numbers every run from one counter (`numbers::next`) and announces it with `Started` after any restart and before its submit (`announce_run`, `handle_repl_execute`), so a number names one run in every window; a submit failure gets a synthetic `Done`. A caller's own number (a window's eval or `r`/`R`) is an alias of its window (`CallerNumbers`, shared by each connection of the window, a redial's included): each of them is written the run under it and never its `Started`, nor any frame of a run its submit refused (the reply says why), and an interrupt's `eval_ids` from any of them are mapped to the daemon's numbers. A window's number names one run (a later run numbered the same takes it), and a run stays past its `done`, so each connection of the window finds it there; at most 256 are kept, finished ones going first. A window's run is marked running when the REPL's queue accepts it (`Repl::submit`, `CallerNumbers::accepted`: every refusal, a full queue or a REPL that cannot start, comes before) and ended by the supervisor before it publishes the run's `done`, the close-out's included, a run still queued or unwritten when its child ends too (`end_then`, carried by the run's `Submission`; the mark and the publish are one call, and `running` takes the frame-bus subscription it reads after), so each hello reply names the window's runs still running (`running`, `HelloRes::repl_running`): a run still waiting for the queue is named too (`submit` waits in `ensure_supervisor` for a settle of at most `REPL_RESTART_WAIT` and a display start of at most 10 s, about 15 s in all, and a window that redials in that time must still be written the run), and a run named that the queue then refuses is written its reason and `done` (its reply went to the dropped connection); never one refused before a hello named it, nor one whose `done` is out. A run_file with no path is refused (`bad_request`): the shim would answer it with a res alone, which this op drops.
- `repl.execute` output is collected loss-free per eval, text capped at `EXEC_TEXT_CAP` (`ExecAccum`).
- A `repl.execute` that times out while the child has not said it is ready (`Starting`) answers `timeout` with an
  `error` saying how long the REPL has been starting and its last stderr lines (`Repl::still_starting`), and logs that
  at warn; its run stays queued. A timeout behind a ready REPL carries no error: its run is going.
- A test's Julia child never inherits the test binary's stdout or stderr (the leak probe in `project_tests.rs` nulls
  its stderr): Julia's runtime sets `O_NONBLOCK` on an inherited stdio pipe and never restores it, and the flag lives on
  the pipe's open file description, so every later writer to the job's shared stderr (the test binaries after it and
  their children) gets EAGAIN once the pipe is full.
