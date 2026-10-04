# rust/backend/src/rows: rows (charter)

## Idea
A row is one project directory on this host running one agent: the daemon registers it, its id survives restarts, and
every change to its supervisor goes through one guard. This folder holds the in-memory registry of rows and the row toml store
(`store/`); the launch of a supervisor is in `spawn/`, and its start, end, watchdog and boot resume are in `run/`.
Part of the daemon's rows subsystem, under `rust/backend/src`.

## Owns
- The registry `Workspaces`: `by_id`, `by_slug` and the default id, with `insert`, `resolve`, `list`, `has_slug`,
  `workspace_for_tmux` and `remove_by_id` (`registry.rs`, the structs in `mod.rs`).
- The row `Workspace`: id, slug, label, root, session name, agent, agent name, account, declared comm handle, the
  phase cell, the watchdog token, the activation error and the lazy file, concept, kernel, repl and watcher handles
  (`workspace.rs`, the struct in `mod.rs`).
- One lifecycle guard per row: `Workspaces::capsule_guard`.
- The run gate: `Workspaces::begin_start` and `close_gate_and_settle`, `StartPermit` (`gate.rs`).
- The default row's inert-anchor rule, `is_inert_default_anchor`, `reset_agent_to_none`, `default_row_launch_seed` and
  the boot seed `seed_default_row` (`anchor.rs`).
- Three daemon-wide handles kept beside the rows: the `repl.frame` sender, the watch bus and the monitor hub
  (`set_repl_frame_tx`, `set_watch_bus`, `set_monitor_hub`), and each row's lifecycle observer task
  (`install_observer`, `has_observer`).
- The `WorkspaceChanged` event the `workspace.changed` bus carries (`mod.rs`).

## Promises
- Re-inserting a slug keeps its workspace id and takes every other field from the new row (`Workspaces::insert`).
- Handle, account and agent change in place on the shared `Arc`, never through a replacing `insert`, so a destroyed row
  is not brought back (`set_agent_handle`, `set_account`, `reset_agent_to_none`).
- `capsule_guard` returns a guard only for a registered row and creates it under the registry's write lock, so two
  first callers never mint two guards; `remove_by_id` drops it with the row, cancels and aborts the row's observer,
  and clears the default id if it was the removed row.
- No start begins once the gate is closed: `begin_start` refuses after `close_gate_and_settle`, which then waits for
  the permits already out.
- The phase cell is written only through `Workspace::apply_phase_observation`, which rejects an observation whose
  supervisor identity differs from the cell's epoch and latches `Terminal` and `EndedNoRespawn`
  (`run::observer::observe` is the caller).
- The default row is an inert anchor when its agent is `none`, on every runtime: `is_inert_default_anchor` is the one
  predicate for that.
- The row toml is a projection of this state that `store::save` rewrites; the state changes through the daemon's
  ops, not through the file.

## Connections
- In: every op finds its row through `Workspaces::resolve`, and the ops that answer a missing row with
  `unknown_workspace` do it through `row_or_reply` (`mod.rs`); `ops/` creates, lists and destroys rows
  (`create::handle_workspace_create`, `destroy::handle_workspace_destroy`); `server::run` calls `anchor::seed_default_row`,
  which inserts the default row.
- Out: `run/` takes the guard and a `StartPermit` (`run::start::start_supervisor`, `run::start::reset_run`),
  writes observations (`run::observer::observe`) and installs the observer (`install_observer`); the lifecycle close calls
  `close_gate_and_settle`.
- Persistence: `store/` (`scan_disk`, `save`, `toml_path_for`) reads and writes the row toml.

## Folders
- `ops/`: the row ops clients call
- `reauth/`: `workspace.reauth`, the accept half and the restart runner
- `run/`: a row's run as the daemon sees it: phase strings, observer, headless client, start, end, activation, watchdog, boot resume
- `spawn/`: launching a row's supervisor: state-root checks, the detached spawn per OS, the Linux row scope
- `store/`: the row toml store, its codec and the boot migrations

## Files
- `mod.rs`: the `Workspace`, `Workspaces` and `Inner` structs, `row_or_reply`, the `WorkspaceChanged` event and the session name rule
- `workspace.rs`: the row's methods, `Phase`, `Observation`, `SupervisorIdentity`, the phase cell, `now_unix`
- `registry.rs`: `Workspaces` insert, lookup, removal, observers, buses and the per-row guard
- `gate.rs`: `RunGate`, `StartPermit`, `begin_start`, `close_gate_and_settle`
- `anchor.rs`: the inert default anchor rule, `reset_agent_to_none`, `end_default_row_run`, `default_row_launch_seed`, `seed_default_row`
- `ops/`: the row ops clients call
- `reauth/`: `workspace.reauth`, the accept half and the restart runner
- `run/`: a row's run as the daemon sees it: phase strings, observer, headless client, start, end, activation, watchdog, boot resume
- `spawn/`: launching a row's `sot-capsule supervise`: state-root checks, the detached spawn per OS, the Linux row scope
- `store/`: the row toml store, its codec and the boot migrations

## Start here
`mod.rs` for the three structs, then `registry.rs::insert` for how a row enters. For a phase change read
`workspace.rs::apply_phase_observation`; for the default row read `anchor.rs`.
