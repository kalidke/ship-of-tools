# rust/log/src/lane/socket_unix: the Unix domain-socket transport (capsule)

The twin of the Windows pipe transport by property, not mechanism: a server for `<runtime_dir>/voyage-<id>.sock` and
`supervisor-<h>.sock`, a client for them, the same event vocabulary and the same thread roles (`sot-sock-accept`,
`sot-sock-reaper`, `sot-sock-r-<id>`, `sot-sock-w-<id>`). Unix only. Part of capsule; charter: rust/log/CLAUDE.md.

## Files
- `mod.rs`: module doc, the socket paths, and the server's shared types (`ServerShared`, `ConnHandle`, `WriteCmd`, `Probes`)
- `server.rs`: `SocketServer`: bind, events, send, close, and the `LaneServer` impl
- `listener.rs`: the private runtime dir, the fd-anchored bind, and the socket flag helpers
- `accept.rs`: the accept loop thread and admission of one new connection
- `conn.rs`: per-connection workers and the polling reaper's charged pending teardown
- `client.rs`: `SocketClient`, its `Client` and `Endpoint` impls, and the unchallenged and challenged connects
- `connect.rs`: the bounded, non-blocking `connect(2)` attempt over a fresh socket

## Start here
`server.rs` `SocketServer::bind_named` for how a server starts; `client.rs` `connect_voyage_socket` for how a client dials.

## Rules
- A test-support checkpoint never waits behind a deliberate fixture hold of the recorder (`hold`) and is never lost to an ordinary holder: a busy ring is tried again until free, and only a fixture hold or a poisoned ring makes the checkpoint skip, counted; snapshots report skipped admissions and retain admitted records after connection removal until overwritten, without taking the connection-state lock.
- The runtime dir must be private (`ensure_private_runtime_dir`); every later file step is anchored to the verified directory fd (`open_verified_dir_fd`), and the socket's mode is set and verified before `listen` (`create_and_bind_listener`; on Linux `bind` itself goes through the fd, on macOS by path).
- A socket path is checked against `max_sun_path_bytes` before binding (`socket_path`).
- A raw connect (`connect_voyage_socket_unchallenged`, `connect_supervisor_socket_unchallenged`) stays `pub(crate)`; `connect_voyage_socket` authenticates the server through `challenge_os::authenticate_server` before returning.
- A connect uses a fresh socket per attempt and retries a busy listener only until `CONNECT_BOUND` has passed; the
  attempt or 20 ms sleep in progress finishes first (`connect_unix_socket_unchallenged`).
- The accept loop's error handling is `sot_log::lane::accept_policy::accept_policy` (retry, sleep on a resource error, stop on a dead listener), shared with `pipe_win` and the daemon's session listener.
- A change to one server's accept or teardown is made to the Windows twin, `pipe_win`, too.
- Live and pending records share the admission bound; `Closed` follows both joins. Shutdown errno alone preserves close reason and failure state.
- A socket read that `shutdown(2)` must end waits in `poll(2)` and reads with `MSG_DONTWAIT` (`conn::read_when_ready`), never a blocking `read`/`recv`: XNU's `soreceive` can lose a concurrent shutdown's wake, and its `sorflush` zeroes `SO_RCVTIMEO`.
- The listener and connector sockets and the wake pipe are born close-on-exec by `host::fd` (`socket_cloexec`, `pipe_cloexec`), never by a bare `socket` or `pipe`.
