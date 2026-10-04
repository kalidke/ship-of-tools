# rust/frontend/src/ui/session: which session row the window is on (fe-ui)

Each daemon's `workspace.list` is the only source of rows. The window keeps a projection of those lists keyed on
(host, slug), a parked UI and REPL view per row, and the one switch that moves between rows. Part of fe-ui; charter:
rust/frontend/src/ui/CLAUDE.md. The record is ADR 0042, 0044 and 0025.

## Files
- `mod.rs`: declares the files below and re-exports their names to `ui`.
- `workspace_key.rs`: the (host, slug) key: `ws_key_of`, `lifecycle_key_of`, and `State`'s current, active, caption, reply and lifecycle keys.
- `workspace_list.rs`: `workspace.list` into the strip: `fresh_workspace_caches`, `declared_sessions_from`, `activity_order`, and `State`'s `rebuild_workspace_caches`, `resort_strip`.
- `snapshot.rs`: `WorkspaceUiSnapshot` and `WorkspaceReplSnapshot`, saved and restored by `State`'s snapshot and restore methods.
- `switch.rs`: `State::switch_to_workspace` and `cycle_workspace`.
- `picker.rs`: `WorkspacePicker` and its start directory, and `State`'s `begin_create_session` through `commit_workspace_create`.
- `presence.rs`: `ReadMark`, `read_mark_decision`, and `State`'s `report_presence` and `fire_due_read_mark`.
- `badge.rs`: the badge floor: `pending_nav_status`, `State::mark_pending_nav` and `badged_keys`.

## Start here
`State::switch_to_workspace` in switch.rs, for any change to what happens when the window moves to another row.

## Rules
- Workspace maps key on (host, slug), never a bare slug, and both spellings of the default workspace become
  `"<default>"` (`ws_key_of`).
- Every switch sends `workspace.activate`; `read: true` goes only from `fire_due_read_mark` after `READ_DWELL` on a view a
  person chose, and `read_mark_decision` cancels the mark on any other view.
- Strip order within a host block depends only on the rows, their state and stamp, the previous order and the pinned
  row, so an unchanged list reproduces it (`activity_order`).
- `fe.presence` reaches every connected daemon at most once per `PRESENCE_THROTTLE` and never from a harness instance
  (`report_presence`).
- A badged result never switches the view (`mark_pending_nav`).
