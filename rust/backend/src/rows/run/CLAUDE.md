# rust/backend/src/rows/run: a capsule row's run as the daemon sees it (rows)

The daemon learns what a capsule row's supervisor is doing from its lane and sometimes types into a row's agent. This
folder holds the phase vocabulary the daemon reports, the per-row observer that feeds a row's phase cell, and the
headless attach client. Part of the daemon's rows subsystem; charter: `rust/backend/src/rows/CLAUDE.md`.

## Files
- `mod.rs`: declares the three modules below
- `probe.rs`: the phase strings (`UNREACHABLE_PHASE`, `FOREIGN_PHASE`, `NEVER_STARTED_PHASE`), `phase_for_missing_pointer`, `phase_str`, `local_phase`
- `observer.rs`: the per-row lifecycle observer task and `observe`
- `headless.rs`: the daemon's own attach client: `type_into`, `write_and_enter`, `screen_of`, `HeadlessError`

## Start here
`observer.rs::ensure_running` for how a row's phase is polled; `headless.rs::type_into` and `write_and_enter` for the
ops that type into a sibling row (`pty.input` in `handlers.rs`, the comm wake in `comm_wake.rs`).

## Rules
- `observer::observe` is the one call that feeds a row's phase cell, through `Workspace::apply_phase_observation`.
- One observer task runs per row; `observer::ensure_running` starts it and `Workspaces::insert` never does.
- The headless client never resizes the pane and takes the pen only to deliver one input (`headless::type_into`).
- Once the text is recorded, an Enter failure is never a hard error (`headless::write_and_enter`, `enter_outcome`).
