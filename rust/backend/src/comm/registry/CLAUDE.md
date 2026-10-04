# rust/backend/src/comm/registry: the address book in the daemon (messaging)

The address book says which handle names which session; this folder is the daemon's arm of it. Part of messaging;
design of record: docs/adr/0049-messaging-on-one-page.md.

## Files
- `ancestors.rs`: the process-ancestry walk printed by `sotd ancestors`
- `lock.rs`: the daemon's arm of the registry lock, `.registry.lock`
- `mod.rs`: declares the files
- `poll.rs`: the registry poll's change detection

## Start here
`lock.rs` `acquire`, for any change to how the daemon takes the registry lock.
`ancestors.rs` `run` (`sotd ancestors [--from <pid>]`, Windows only). comm-lib.sh's `_sot_ancestor_chain` reads it to
count the agents between a comm script and its row's capsule.

## Rules
- Output is parent first, one `<pid>\t<exe>\t<command line>` line each. The walk stops where the chain stops being one (a
  missing pid, pid 0 or 4, an unreadable record, a process created after its child).
- Past `MAX_LINES` (64) the output ends `!truncated` with exit 3.
- `acquire` takes `<comm home>/.registry.lock`, the lock comm-lib.sh's `with_lock` takes. Both write the same one-line
  record, `name:machine:boot:pidns:pid:start`, by `link(2)` of a temp file that already holds it (`take`).
- A holder is proved dead on Linux only, and only from its own machine (`judge`).
- A waiter that proves the holder D dead takes `.registry.lock.reclaim.<D>`, settles, rereads, and removes the lock only
  if it still names D (`step`). Nothing else is forced.
- At its bound a waiter fails closed and names the holder (`Blocked::fail_text`).
- One daemon thread at a time is inside the protocol (`TURN`). `Held` removes the file when dropped, a panic included.
- An ID counts as this process only through `Me::is_me`. comm/core/tests/test-registry-lock.sh t15 fails on any other
  comparison to `.id` in lock.rs.
- File names map `:` to `.`.
