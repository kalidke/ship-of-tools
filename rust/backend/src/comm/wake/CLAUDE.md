# rust/backend/src/comm/wake: the wake of idle sessions (backend)

The daemon's tick that types a one-line wake into a session's free prompt when its handle has unread mail. Part of the
backend; charter: comm/CLAUDE.md.

## Files
- `mod.rs`: the whole wake, one file for now

## Start here
`mod.rs` `check_row` for when a row is woken.

## Rules
- The wake only reads `inbox/<h>.jsonl` and `read/<h>.cursor`.
