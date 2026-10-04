# rust/log/src/capsule/producer/pty: the Unix producer (capsule)

`PtyProducer` runs the agent on a bare `openpty` pair in its own session and process group, and holds a spare slave
open so the master reports end of output only when the loop closes it. Unix only (`#![cfg(unix)]`). Part of the log
subsystem; charter: rust/log/CLAUDE.md.

## Files
- `mod.rs`: `PtyProducer`, its helpers and `/proc` scanners, `Drop`, `parent_lease_fd_broken` and the tests
- `verbs.rs`: `impl Producer for PtyProducer`, the nine verbs (`spawn` first)

## Start here
`PtyProducer::spawn` in `verbs.rs` for anything about how the child starts; the module doc for why each ordering is as it is.

## Rules
- `spawn` checks the program resolves before forking (`executable_is_resolvable`), because the pre-exec fd close
  would hide a real exec failure.
- `Drop` kills the group and reaps within `REAP_BOUND`; a task stuck in an uninterruptible wait is left, not waited on.
- A closed or unreadable lease fd counts as broken (`parent_lease_fd_broken`).
