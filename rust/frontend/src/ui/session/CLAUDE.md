# rust/frontend/src/ui/session: which session row the window is on (fe-ui)

Each daemon's `workspace.list` is the only source of rows. The window keeps a projection of those lists keyed on
(host, slug), a parked UI and REPL view per row, and the one switch that moves between rows. Part of fe-ui; charter:
rust/frontend/src/ui/CLAUDE.md. The record is ADR 0042, 0044 and 0025.

## Files
- `mod.rs`: declares the files below and re-exports their names to `ui`.
- `workspace_key.rs`: row/view key helpers, private ResultRowIdentity/ResolvedWorkspace, and resolve_listed_workspace over the producing host's listed canonical row facts.
- `workspace_list.rs`: workspace-list projections, canonical pending-result reconciliation before cache rebuild, and activity ordering of live badges.
- `snapshot.rs`: `WorkspaceUiSnapshot` and `WorkspaceReplSnapshot`, saved and restored by `State`'s snapshot and restore methods.
- `switch.rs`: switch_to_workspace and cycle_workspace; attachment uses the row's stored session_name.
- `keys.rs`: Session keys from the tree: the workspace picker, Enter on a Sessions row, and the two-press destroy.
- `picker.rs`: `WorkspacePicker` and its start directory, and `State`'s `begin_create_session` through `commit_workspace_create`.
- `presence.rs`: `ReadMark`, `read_mark_decision`, and `State`'s `report_presence` and `fire_due_read_mark`.
- `badge.rs`: canonical-row PendingNav and its replace/start/invalidate/cursor/preview/presentation transitions, pending_nav_status, mark_pending_nav and live badged_keys.
- `replies.rs`: host and workspace replies; authoritative lists and successful non-kept destruction invalidate removed canonical result identities before view effects.

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
- Strip keys retain the listed (host, slug); view keys normalize that host's default. A command never inserts its unresolved spelling as a row key.
- A listed row is attached by its session_name from workspace.list; neither a slug nor a session name is derived from the other.
- A result belongs to the producing host's listed canonical row. Removal or identity replacement invalidates its entry and attempts; while the row remains listed, only its matching cursor, installed preview and successful presentation acknowledge it. Late completion cannot affect a successor.
- Reconcile canonical pending-result identities before rebuilding workspace caches from an authoritative host list. A disconnect is not removal, and a kept default row retains its identity.
- A replacement result or restarted attempt gets fresh local serials. Result-owned root/children requests retain them through pending entries and tagged events; stale successes and failures are rejected before tree or reveal mutation. Ordinary tree replies cannot complete or abort a result-owned reveal. Preview generations and presentation certificates are bound to the same issuing attempt.
- The picker sends its selected account by name; only absent, out-of-range or default selections omit the account (selected_account).
