# rust/backend/src/comm/registry: the address book in the daemon (messaging)

The address book says which handle names which session; this is the daemon's arm of it. Part of messaging; charter:
comm/CLAUDE.md.

## Files
- `ancestors.rs`: the process-ancestry walk printed by `sotd ancestors`
- `join.rs`: `agent.join`: a session declares its handle on its row
- `lock.rs`: the daemon's arm of the registry lock, `.registry.lock`
- `lock_tests.rs`: the lock's tests, including the shell-parity test (Linux)
- `mod.rs`: declares the files
- `poll.rs`: the registry poll task (`spawn_registry_poll`) and its change detection (`project_comm_registry`)
- `registry.rs`: the daemon's registry reads, prune, unread clear, row binding and UTC stamps
- `registry_tests.rs`: the tests of `registry.rs`
- `registry_write_tests.rs`: the tests of the write that ends the prune and the unread clear

## Start here
`registry.rs` `with_comm_registry_lock` for any registry write; `lock.rs` `acquire` for how the lock is taken.
`ancestors.rs` `run` (`sotd ancestors [--from <pid>]`, Windows only), read by comm-lib-agent-layers.sh's `_sot_ancestor_chain`. It
prints parent first, one `<pid>\t<exe>\t<command line>` line each; past `MAX_LINES` (64) it ends `!truncated`, exit 3.

## Rules
- Every daemon registry write (`remove_comm_agents_for_workspace`, `clear_comm_unread`) runs under
  `with_comm_registry_lock` and goes through `replace_registry`: `write_synced` flushes a temp file, then it is renamed
  into place.
  `remove_comm_agents_for_workspace` prunes a destroyed row's entry.
- `clear_comm_unread` removes `done` (and turns a `done` state to `idle`) on a person's view of a row. It is the
  daemon's only work-state write.
- `comm_handle_for_workspace` is the one row-binding rule: the declared handle, else the pinned self-file, else the
  stored agent name; a self-file or stored name that names a handle another row declares binds nothing. Its callers
  (`handle_workspace_list`, `clear_comm_unread`, `running_row_holds`) pass the daemon's rows.
- A registry entry's `workspace_id` is the row whose session last joined that handle (`comm-join.sh` writes it;
  `entry_row` reads it). The destroy prune (`remove_comm_agents_for_workspace`) removes an entry by the row's stored
  name only when the entry names no other row.
- `handle_agent_join` moves the declared handle (`set_agent_handle`) and answers `ok` only after the joining row is
  saved under its guard. Whatever that save did, a spawned task (`spawn_persist_moved`) then saves each row that lost
  the handle under that row's own guard, if it is still registered (`persist_moved`): the reply never waits on another
  row's guard, and no two guards are held at once. A failed save there is a warning; each move is one info line.
- `spawn_registry_poll`, started by the server's `run`, publishes `agent_state` on the workspace bus when
  `project_comm_registry` changes between polls; `last_seen` is not in the projection.
- `acquire` takes `<comm home>/.registry.lock`, the lock comm-lib-registry-lock.sh's `with_lock` takes. Both write the same one-line
  record, `name:machine:boot:pidns:pid:start`, by `link(2)` of a temp file that already holds it (`take`); a test
  holds them byte-equal (`the_shell_and_rust_records_are_byte_equal_and_judged_alike`, Linux).
- A holder is proved dead on Linux only, and only from its own machine (`judge`). A waiter that proves holder D dead
  takes `.registry.lock.reclaim.<D>`, settles, rereads, and removes the lock only if it still names D (`step`). Nothing
  else is forced: at its bound a waiter fails closed and names the holder (`Blocked::fail_text`).
- One daemon thread at a time is inside the protocol (`TURN`). `Held` removes the file when dropped, a panic included.
- An ID counts as this process only through `Me::is_me`; test-registry-lock.sh t15 fails on any other `.id` comparison.
