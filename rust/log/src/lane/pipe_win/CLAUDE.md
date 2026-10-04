# rust/log/src/lane/pipe_win: the Windows named-pipe transport (capsule)

Moves bytes and reports completions for `\\.\pipe\sot-voyage-<id>` and `\\.\pipe\sot-supervisor-<h>`; it knows no lane,
frame or opcode. Windows only (`mod.rs` is `#![cfg(windows)]`). Part of capsule; charter: rust/log/CLAUDE.md.

## Files
- `mod.rs`: the module doc, pipe-name helpers, shared types (`SendableHandle`, `WriteCmd`, `ConnHandle`, `AcceptState`, `ReaperMsg`, `ServerShared`), the re-exports and the `join_within` tests
- `slot.rs`: `IoSlot`, the overlapped I/O slot state machine, and `wait_overlapped`
- `registry.rs`: `create_pipe_instance`, `InstanceRegistry` and `LiveHandle`, the one closer of instance handles
- `server.rs`: `PipeServer` and its `LaneServer` impl
- `accept.rs`: the accept loop: `obtain_instance`, `accept_loop`, `handle_new_connection`, `recycle_instance`
- `conn.rs`: the reaper and each connection's reader and writer threads
- `client.rs`: `PipeClient`, the voyage and supervisor connects, `PipeEndpoint`

## Start here
`server.rs` `PipeServer::bind_named` for the server; `client.rs` `connect_voyage_pipe` for a client.

## Rules
- Every instance is created with `PIPE_REJECT_REMOTE_CLIENTS` and the owner-only descriptor (`create_pipe_instance`).
- A raw connect (`connect_voyage_pipe_unchallenged`, `connect_supervisor_pipe_unchallenged`) stays `pub(crate)`; `connect_voyage_pipe` authenticates the server before returning.
- An `OVERLAPPED`, its event and buffer stay valid until the kernel is done: if completion is not proven within `OVERLAPPED_COMPLETION_PROOF_TIMEOUT`, the server leaks the slot and buffer and the client aborts (`CompletionUnproven`).
- Same event vocabulary and thread roles as `socket_unix`, its Unix twin by property; a change to one server's accept or teardown is made to both.
