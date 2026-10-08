# rust/backend/tests/daemon_lifetime/native: the fixture's process authority per OS (tests)

An `Identity` is a process the fixture opened while it was alive and proved to be the one it meant: its death is read
from the identity. Only an identity for a pid the case's own code got back from its own spawn (`acquire_own`) can be
signalled, and then through the identity, so a number the OS has given to a stranger never is; any other identity is
watched (`acquire`) and `signal` and `kill` refuse it.
Part of the daemon-lifetime harness; page: rust/backend/tests/daemon_lifetime/CLAUDE.md.

## Files
- `mod.rs`: picks the file for the OS and re-exports `Identity` and `start_ticks`
- `linux.rs`: a pidfd opened while the process was alive and checked against its start time
- `macos.rs`: the task-control authority, not yet established; every request fails with that cause

## Rules
- An identity for a process the fixture did not start is kept only if the process's own report of its start time matches, and is never signalled.
- `exited` reads the identity; the pidfd stays readable after the reap, so an exit is seen however it was reaped.
