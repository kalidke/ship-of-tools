# rust/backend/src/rows/run: a capsule row's run as the daemon sees it (rows)

The daemon learns what a capsule row's supervisor is doing from its lane, starts and resumes it, watches the one it
spawned, ends it with a proof, and sometimes types into a row's agent. This folder holds the phase vocabulary and the
status probe, the per-row observer that feeds a row's phase cell, the start, activation, watchdog, resume and end
paths, and the headless attach client. Part of the daemon's rows subsystem; charter: `rust/backend/src/rows/CLAUDE.md`.

## Files
- `mod.rs`: declares the modules below and the names their bodies reach as `super::`
- `end.rs`: ending a row's run (destroy_capsule_workspace and its outcome) and removing its tomls
- `probe.rs`: the phase strings (`UNREACHABLE_PHASE`, `FOREIGN_PHASE`, `NEVER_STARTED_PHASE`), `phase_for_missing_pointer`, `phase_str`, `local_phase`, and `probe` / `phase_of`, the one status round trip
- `observer.rs`: the per-row lifecycle observer task, `observe` and `observe_with_adoption`
- `headless.rs`: the daemon's own attach client: `type_into`, `write_and_enter`, `screen_of`, `checkpointed` (the checkpoint wait every attach goes through; it shuts the client down on failure), `HeadlessError`
- `start.rs`: `spawn_and_watch`, `start_supervisor`, `reset_run` (the run gate) and `settle_after_spawn`
- `activation.rs`: `ensure_started` (start on attach), `resume_if_absent`, `resume_locked`, `ActivationIntent`
- `watchdog.rs`: `install_watchdog`, the exit classes (`LegOutcome`) and the restart budget (`RESTART_BACKOFFS`)
- `resume.rs`: `resume_all`, the boot resume of every registered row whose pointer exists
- `end_run.rs`: `end_run`, `EndRunOutcome` and the destroy proof (`absence_proof`, `leg_absent`)

## Start here
`activation.rs::ensure_started` for how a row starts on attach, and `end_run.rs::end_run` for how a row ends with the
destroy proof. `observer.rs::ensure_running` for how a row's phase is polled; `headless.rs::type_into` and
`write_and_enter` for the ops that type into a sibling row (`pty.input` in `rows/ops/pty.rs`).

## Rules
- A phase's wire string is written once, in `Phase::as_wire_str`; `probe.rs`'s constants and `phase_str` read it. The one exception is `topology/cli.rs`, which compares a reply's `phase` against "starting" and "ready" itself.
- `observer::observe` is the one call that feeds a row's phase cell, through `Workspace::apply_phase_observation`.
- One observer task runs per row; `observer::ensure_running` starts it and `Workspaces::insert` never does.
- The headless client never resizes the pane and takes the pen only to deliver one input (`headless::type_into`).
- Once the text is recorded, an Enter failure is never a hard error (`headless::write_and_enter`, `enter_outcome`).
- A watchdog exists only for a supervisor this daemon spawned (`start::spawn_and_watch` calls `watchdog::install_watchdog`).
- Only a crash restarts, after 1, 3, 7, 15 and 30 s and at most 5 times in 60 s (`watchdog::classify_exit_code`,
  `RESTART_BACKOFFS`); exit 69 is terminal and never restarted, exit 70 is never terminal (`watchdog::LegOutcome`).
- A run is reported ended only when the supervisor fence and the voyage's writer.lock are both proven free
  (`end_run::absence_proof`, `end_run::leg_absent`).
- Every run start passes the gate (`start::start_supervisor`, `start::reset_run`).
- Every change to a row's supervisor holds the row's guard (`activation::ensure_started`, `activation::resume_if_absent`,
  `watchdog::install_watchdog`).
- `end_run` waits out an authority reporting `starting` for activation's bound (50 re-probes 200 ms apart); one still starting is stopped and judged by `absence_proof`, as a terminal one is (`settle_starting`, `end_still_starting`). A destroy or a window's close therefore ends a row held for storage.
- Boot resume spawns nothing for a live authority (`resume::resume_all` through `activation::resume_locked`).
