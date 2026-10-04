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

## Start here
`registry.rs` `with_comm_registry_lock` for any registry write; `lock.rs` `acquire` for how the lock is taken.
`ancestors.rs` `run` (`sotd ancestors [--from <pid>]`, Windows only), read by comm-lib.sh's `_sot_ancestor_chain`. It
prints parent first, one `<pid>\t<exe>\t<command line>` line each; past `MAX_LINES` (64) it ends `!truncated`, exit 3.

## Rules
- Every daemon registry write (`remove_comm_agents_for_workspace`, `clear_comm_unread`) runs under
  `with_comm_registry_lock` and is one document: `write_synced` flushes a temp file, then it is renamed into place.
  `remove_comm_agents_for_workspace` prunes a destroyed row's entry.
- `clear_comm_unread` removes `done` (and turns a `done` state to `idle`) on a person's view of a row. It is the
  daemon's only work-state write.
- `comm_handle_for_workspace` is the one row-binding rule: the declared handle, else the pinned self-file, else the
  stored agent name.
- `handle_agent_join` stores the declared handle under the row's guard, overwriting a previous one without a check
  (`set_agent_handle`); `ok` only after the row is persisted.
- `spawn_registry_poll`, started by the server's `run`, publishes `agent_state` on the workspace bus when
  `project_comm_registry` changes between polls; `last_seen` is not in the projection.
- `acquire` takes `<comm home>/.registry.lock`, the lock comm-lib.sh's `with_lock` takes. Both write the same one-line
  record, `name:machine:boot:pidns:pid:start`, by `link(2)` of a temp file that already holds it (`take`); a test
  holds them byte-equal (`the_shell_and_rust_records_are_byte_equal_and_judged_alike`, Linux).
- A holder is proved dead on Linux only, and only from its own machine (`judge`). A waiter that proves holder D dead
  takes `.registry.lock.reclaim.<D>`, settles, rereads, and removes the lock only if it still names D (`step`). Nothing
  else is forced: at its bound a waiter fails closed and names the holder (`Blocked::fail_text`).
- One daemon thread at a time is inside the protocol (`TURN`). `Held` removes the file when dropped, a panic included.
- An ID counts as this process only through `Me::is_me`; test-registry-lock.sh t15 fails on any other `.id` comparison.
