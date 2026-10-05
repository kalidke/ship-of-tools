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
- The default row's declared handle is carried through the boot re-seed (`seed_default_row` copies it, `insert`
  keeps it); the one-row-per-handle promise below still applies to it.
- Handle, account and agent change in place on the shared `Arc`, never through a replacing `insert`, so a destroyed row
  is not brought back (`set_agent_handle`, `set_account`, `reset_agent_to_none`).
- No two rows hold one declared handle. At run time `set_agent_handle` clears it from every other row under the
  registry's write lock and returns their ids for the caller to save; at boot `store::scan_disk` keeps a handle that
  several tomls declare only on the row the comm registry names as its last joiner, if it is one of them
  (`clear_shared_handles`).
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
Each connection is one row of docs/integration.md, owned by its provider. Provides: `destroy_capsule_workspace`,
`end_default_row_run`, `resume_all`, `close_gate_and_settle`, `remove_row_files`, `workspace.create`,
`workspace.destroy`, `workspace.list`, `workspace.reauth`, `pty.input`, `pty.screen`, `workspace.changed`,
`lane.connect`, `Workspace::agent_handle`, `set_agent_handle`, `attach`, `send_text`, `send_enter`,
`rust/backend/src/rows/run/headless.rs`, `Workspaces::resolve`, `row_or_reply`, `capsule_guard`, `seed_default_row`,
`set_repl_frame_tx`, `set_watch_bus`, `set_monitor_hub`. Uses: `lane.connect`, `handle_connection`,
`handle_lane_connect`, `pipe_bidirectional`, `reject`, `dispatch`, `write_frame_within`, `write_frame_to`,
`agent_argv`, `agent_exec_argv`, `claude_recipe`, `account_env`, `account_spawn_env`, `ensure_folder_trusted`,
`comm_handle_for_workspace`, `clear_comm_unread`, `read_comm_agents`, `host_matches`, `last_joiner`,
`capsule_supervisor_env`, `sot-capsule supervise`, `supervisor_client`, `FeAttachClient`,
`rust/backend/src/rows/run/headless.rs`, `rust/frontend/src/ui/agent_pane/attach.rs`, `drawer.voyage`, `writer.lock`,
`sot_state_dir`, `sot_config_dir`, `host_name`, `state_dir_hash`, `durable::write`, `durable::remove`,
`rust/backend/src/durable.rs`, `remove_comm_agents_for_workspace`, `handle_agent_join`, `FilesMode`, `ConceptStore`,
`rust/backend/src/rows/workspace.rs`, `Watcher`, `rust/backend/src/rows/registry.rs`, `Kernel`, `Repl`, `Signal::spawn_std`, `Held`.

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
