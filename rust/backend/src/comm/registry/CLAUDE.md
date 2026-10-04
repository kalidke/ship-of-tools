# rust/backend/src/comm/registry: the address book in the daemon (messaging)

The address book says which handle names which session; this folder is the daemon's arm of it. Part of messaging;
design of record: docs/adr/0049-messaging-on-one-page.md.

## Files
- `ancestors.rs`: the process-ancestry walk printed by `sotd ancestors`
- `mod.rs`: declares the files

## Start here
`ancestors.rs` `run` (`sotd ancestors [--from <pid>]`, Windows only). comm-lib.sh's `_sot_ancestor_chain` reads it to
count the agents between a comm script and its row's capsule.

## Rules
- Output is parent first, one `<pid>\t<exe>\t<command line>` line each. The walk stops where the chain stops being one (a
  missing pid, pid 0 or 4, an unreadable record, a process created after its child).
- Past `MAX_LINES` (64) the output ends `!truncated` with exit 3.
