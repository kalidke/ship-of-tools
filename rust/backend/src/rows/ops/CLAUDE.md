# rust/backend/src/rows/ops: the row ops clients call (rows)

The ops a client sends about a row: make one, end one, list them, switch the view to one, and type into or read the
screen of a capsule row. Each handler takes the parsed payload and the registry and answers one frame; the row state they
change lives in the parent folder. Part of the daemon's rows subsystem; charter: rust/backend/src/rows/CLAUDE.md.

## Files
- `mod.rs`: the module list
- `create.rs`: `workspace.create` and its two gates, one root per session and no refresh of a row in use
- `create_tests.rs`: tests of `create.rs`: the duplicate-root and same-slug gates, and the seven refusals of `workspace.create`
- `destroy.rs`: `workspace.destroy`, which ends a row's run and then removes the row; the default row only ends its run
- `destroy_tests.rs`: tests of `destroy.rs`: `workspace.destroy` on the default row, with the state-root fixtures they share
- `lane_bridge.rs`: `lane.connect`, the byte pipe onto a row's supervisor or voyage lane after one answered frame
- `list.rs`: `workspace.list` (the rows the window shows, with their comm state) and `workspace.activate`
- `pty.rs`: `pty.input` and `pty.screen` through a capsule row's supervisor lane

## Start here
`create.rs::handle_workspace_create` for what a new row is; `destroy.rs::handle_workspace_destroy` for how one ends.

## Rules
- A row and its tomls are removed only after `destroy_capsule_workspace` reports `Removable`; `Kept` and
  `AlreadyRemoved` answer a typed error (`capsule_end_not_reached_payload`) and remove nothing.
- The row guard that `destroy_capsule_workspace` returns is held through `remove_by_id`, or through the default row's
  reset (`end_default_row_run`).
- The default row is never removed by `handle_workspace_destroy`, only its run is ended.
- `handle_workspace_create` refuses a root another row holds (`find_other_workspace_with_root`, the inert anchor
  excepted), a same-slug row in use (`same_slug_row_in_use`) and an `agent_name` failing `valid_name`, each before
  the row is registered by `insert`.
- `handle_workspace_list` reads memory and one registry read (`read_comm_agents`), never a lane.
- A voyage id given to `lane.connect` must be the target row's own (`check_voyage_ownership`), checked before any dial.
- The daemon authenticates the lane's server before piping (`dial_and_authenticate`) and never decodes a lane frame after
  the pipe starts (`handle_lane_connect`).
