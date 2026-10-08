# rust/log/src/lane/pipe_win: the Windows named-pipe transport (capsule)

Moves bytes and reports completions for `\\.\pipe\sot-voyage-<id>` and `\\.\pipe\sot-supervisor-<h>`; it knows no lane,
frame or opcode. Windows only (`mod.rs` is `#![cfg(windows)]`). Part of capsule; charter: rust/log/CLAUDE.md.

## Files
- `mod.rs`: the module doc, pipe-name helpers, shared types (`SendableHandle`, `WriteCmd`, `ConnHandle`, `AcceptState`, `ServerShared`), the re-exports and the `join_within` tests
- `slot.rs`: `IoSlot`, the overlapped I/O slot state machine, and `wait_overlapped`
- `registry.rs`: `create_pipe_instance`, `InstanceRegistry` and `LiveHandle`, the one closer of instance handles
- `server.rs`: `PipeServer` and its `LaneServer` impl
- `accept.rs`: the accept loop: `obtain_instance`, `accept_loop`, `handle_new_connection`, `recycle_instance`
- `conn.rs`: per-connection workers and the polling reaper's charged pending teardown
- `client.rs`: `PipeClient`, the voyage and supervisor connects, `PipeEndpoint`

## Start here
`server.rs` `PipeServer::bind_named` for the server; `client.rs` `connect_voyage_pipe` for a client.

## Rules
- Test-support progress snapshots take no connection, slot or registry lock (`progress_for_test`).
- Native admission tests exercise the live owner-only pipe access decision for voyage and supervisor endpoints and subsequent instances, with denied-token and owner-token controls.
- Every instance is created with `PIPE_REJECT_REMOTE_CLIENTS` and the owner-only descriptor (`create_pipe_instance`).
- A raw connect (`connect_voyage_pipe_unchallenged`, `connect_supervisor_pipe_unchallenged`) stays `pub(crate)`; `connect_voyage_pipe` authenticates the server before returning.
- An `OVERLAPPED`, its event and buffer stay valid until the kernel is done: if completion is not proven within `OVERLAPPED_COMPLETION_PROOF_TIMEOUT`, the server leaks the slot and buffer and the client aborts (`CompletionUnproven`).
- Same event vocabulary and thread roles as `socket_unix`, its Unix twin by property; a change to one server's accept or teardown is made to both.
- Registration and shutdown share one cutoff lock; instances remain charged through joins and close-event retirement. Cancellation precedes `close_all`; slot completion proof is preserved.
