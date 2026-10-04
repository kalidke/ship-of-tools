# rust/log/src/lane/socket_unix: the Unix domain-socket transport (capsule)

The twin of the Windows pipe transport by property, not mechanism: a server for `<runtime_dir>/voyage-<id>.sock` and
`supervisor-<h>.sock`, a client for them, the same event vocabulary and the same thread roles (`sot-sock-accept`,
`sot-sock-reaper`, `sot-sock-r-<id>`, `sot-sock-w-<id>`). Unix only. Part of capsule; charter: rust/log/CLAUDE.md.

## Files
- `mod.rs`: module doc, the socket paths, and the server's shared types (`ServerShared`, `ConnHandle`, `WriteCmd`, `ReaperMsg`, `Probes`)
- `server.rs`: `SocketServer`: bind, events, send, close, and the `LaneServer` impl
- `listener.rs`: the private runtime dir, the fd-anchored bind, and the socket flag helpers
- `accept.rs`: the accept loop thread and admission of one new connection
- `conn.rs`: the reaper, reader and writer threads, lifecycle events and teardown requests
- `client.rs`: `SocketClient`, its `Client` and `Endpoint` impls, and the unchallenged and challenged connects
- `connect.rs`: the bounded, non-blocking `connect(2)` attempt over a fresh socket

## Start here
`server.rs` `SocketServer::bind_named` for how a server starts; `client.rs` `connect_voyage_socket` for how a client dials.

## Rules
- The runtime dir must be private (`ensure_private_runtime_dir`); every later file step is anchored to the verified directory fd (`open_verified_dir_fd`), and the socket's mode is set and verified before `listen` (`create_and_bind_listener`; on Linux `bind` itself goes through the fd, on macOS by path).
- A socket path is checked against `max_sun_path_bytes` before binding (`socket_path`).
- A raw connect (`connect_voyage_socket_unchallenged`, `connect_supervisor_socket_unchallenged`) stays `pub(crate)`; `connect_voyage_socket` authenticates the server through `challenge_os::authenticate_server` before returning.
- A connect uses a fresh socket per attempt and gives up at `CONNECT_BOUND` (`connect_unix_socket_unchallenged`).
- A change to one server's accept or teardown is made to the Windows twin, `pipe_win`, too.
