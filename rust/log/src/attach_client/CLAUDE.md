# rust/log/src/attach_client: one client for every viewer of a capsule (capsule)

The frontend's pane and drawer, the daemon's headless typing and the tests all attach through one client; the daemon's
own supervisor-lane calls live here too. Part of capsule; charter: rust/log/CLAUDE.md.

## Files
- `client.rs`: `FeAttachClient`, the vt100 parser and pane bookkeeping over an `AttachWorker`
- `client_tests.rs`: its unit tests (pump and checkpoint bookkeeping)
- `mod.rs`: declares the two modules; `supervisor_client` is gated to Windows, Linux and macOS
- `rules/`: the six attach-client rulings as pure state machines
- `supervisor_client.rs`: the daemon's SOSV client: `query_status`, `stop`, `end_run`, `reset`, `Persistent`, and `connect_and_challenge`, which the supervisor shares
- `worker/`: the lane half: one thread per client that dials, attaches and runs the steady state

## Start here
`client.rs`, `FeAttachClient::pump`, for how worker events reach the parser; `supervisor_client.rs`, `connect_and_challenge`, for how any supervisor-lane call starts.

## Rules
- `FeAttachClient` owns only the parser and UI state and drains its worker's events in `pump`.
- Every supervisor-lane call connects through `connect_and_challenge` (hello with the build identity, then the challenge) before any request, each bounded: `CONNECT_AND_HELLO_BUDGET` 2 s, `STATUS_BUDGET` 5 s, `RESET_BUDGET` 30 s.
