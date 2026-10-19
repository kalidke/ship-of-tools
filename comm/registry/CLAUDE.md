# comm/registry: the address book scripts (messaging)

A handle (`<folder>-<box>`) names one session, its registry row says where it lives, and only that session's own agent
may act as it. These scripts resolve the handle, claim it, list the rows and recover the registry's lock; each sources
`comm-lib.sh` from its own folder and calls its siblings through `$SCRIPT_DIR`. They install flat into
`~/.sot-comm/bin` (the folder is listed in `bin-folders.txt`). Part of messaging; charter: comm/CLAUDE.md.

## Files
- `comm-context.sh`: resolve identity from pin, declared-host self slot or derivation, migrating only a validated raw-host legacy slot
- `comm-join.sh`: claim and declare a handle, and send `join`, which starts the floor of the turn it runs in
- `comm-leave.sh`: leave the registry
- `comm-list.sh`: the registry plus the frontend boxes' declared sessions
- `comm-self-audit.sh`: find slots naming another project
- `comm-registry-lock-clear.sh`: clear, at once, a lock whose holder this box proves dead
- `comm-session-start.sh`: the session bootstrap, which joins and prints the work-state rule

## Start here
`comm-join.sh`'s precedence (`--name` > `$SOT_COMM_NAME` > self-file > derive), then `claim_derived_handle` in
`../lib/comm-lib.sh`.

## Rules
- A derived handle is decided and written in one critical section (`claim_derived_handle`); a pin is kept verbatim.
- `sot_require_agent` runs before any read, send, join or stamp.
- `comm-join.sh` runs `ensure_home` before its writes, so a new comm folder is private and an older one is tightened.
- Every registry write is `registry_replace` under `with_lock`, and an unreadable registry is never "absent"
  (`sot_registry_read` returns 2).
- `comm-join.sh` refuses to write another repo's identity into a slot keyed by `$SOT_WORKSPACE_ID`.
- An unpinned self slot and registry host use the declared host; derived handle text keeps its raw host component. Every current self publication and legacy migration uses the registry lock; the migration calls the already-locked writer. It never replaces another project or a concurrently populated canonical slot, and legacy recovery requires the matching registry handle/workspace/root.
- `comm-context.sh` discards a handle whose registry `workspace_id` is another row's (`sot_row_binding`) and emits
  `NAME_HELD_BY`; the row it compares against is the id the walk proves (`ROW_ID`), never the ambient
  `$SOT_WORKSPACE_ID`. It keeps the handle when the entry has none or the registry cannot be read. Only `comm-join.sh`
  passes `--rebind`, honored with a row id alone; `comm-session-start.sh` reads `NAME_HELD_BY` and, for a process with a
  row id, says `@<h> moved to row <id>` and joins again (M5) instead of refusing.
- `comm-join.sh` from a process with no row id asks the daemon about every handle it would take, bound or not
  (`sot_outside_join_clearance`): an explicit name is refused, a derived one is cleared per tier name before the lock and
  steps past a held tier with one plain line. The writes under the lock are held to what was cleared
  (`_sot_outside_cleared`, `_sot_put_outside`).
