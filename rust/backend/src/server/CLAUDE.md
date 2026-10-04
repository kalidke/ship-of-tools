# rust/backend/src/server: the daemon's one listener (charter)

## Idea
One listener per daemon. A connection is admitted once, by the OS peer read at accept, and then by its first frame:
`proxy.connect` (`handle_proxy_connect`), `lane.connect` (`handle_lane_connect`) and `fe.lease` (the lease code's
`hold`) leave the control loop for a byte pipe or a lease; anything else is a control session, and its only writer is
its own loop (`handle_connection`). Part of the daemon's server subsystem; the sotd entry point and the roster beside
this folder serve the same idea.

## Owns
- `<state>/daemon.lock`: taken by `lock_daemon` at the top of `run`, held for the process's life.
- The session socket or pipe: `run_local` secures its private directory, refuses a socket a live daemon answers on
  (`refuse_live_socket`), builds the owner-only pipe descriptor on Windows, accepts, and unlinks at shutdown.
- The eight buses `run` creates: repl frames, preview changes, workspace events, topology writes, agent messages, agent
  receipts, frontend commands and the monitor tick. Their payloads belong to their owners.
- Each connection's task and state, in `handle_connection`: roster guard, declared host and name, active workspace,
  monitor flag, read deadline, and the off-loop `JoinSet` and semaphore.
- The op table: the `match` in `handle_connection`, which runs `pty.open` and `monitor.*` inline and hands the rest to
  each owner's handler.
- sotd's entry (argv, umask 077, boot refusals, the log tee to `<state>/sotd.log`, which follows `XDG_STATE_HOME` and
  has no size bound or rotation), the client roster (`clients.rs`) and the revision ring (`session.rs`), all in the
  crate root.

## Promises
- One daemon per state root: `lock_daemon` fails at once when a daemon answers on the socket, and waits up to
  `daemon_lock_wait` for a predecessor that is still shutting down.
- A socket a live daemon answers on is never unlinked: `refuse_live_socket`, called from `run_local`.
- A connection has one writer. Off-loop jobs hand their reply back over `OutTx` and the loop writes it with
  `write_reply`.
- A handler `Err` is one `handler_error` frame and the connection stays (`finish_dispatch`); an over-cap envelope
  degrades to an error frame for that request (`write_reply`); only a write failure ends the connection.
- A peer that cannot drain a frame within 10 s plus 1 s per MiB of blob is dropped (`write_frame_to`,
  `write_deadline`, ADR 0027).
- A pinged `fe` or `bridge` connection silent for 90 s is reaped (the reaper arm in `handle_connection`,
  `ping_read_deadline`).
- At most 4 off-loop jobs run per connection, and one queued 10 s is answered with a timeout and never runs
  (`spawn_job`); `pty.*` and `monitor.*` run inline and never wait on that pool.
- Accepting stops at the deciding lease departure, before any row is touched (`run_local`'s accept loop, then
  `shutdown::run`).

## Connections
- In: `main` calls `run` and `refuse_live_socket`; the lease and reauth code write through `write_frame_within` and
  `write_frame_to`; `capsule_workspace` calls `record_test_activation_marker`.
- Out: `startup::begin` before bind, `shutdown::run` when accepting ends, each op's handler, `proxy`'s
  `handle_proxy_connect`, `lane_bridge`'s `handle_lane_connect`, `lease::hold`, and `lease::accepted_peer` at accept.
  Frames are read by `codec::read_frame`, which allocates a blob of its declared length with no cap (a known defect).

## Folders
The crate root holds the rest of this subsystem: `main.rs` (sotd's entry), `clients.rs` (the roster) and `session.rs`
(the revision ring).

## Files
- `mod.rs`: the whole listener for now: `run`, `run_local`, `lock_daemon`, `handle_connection`, the per-bus
  `recv_*`/`write_*` pairs, `write_frame_to` and the job pool.

## Start here
An op: the `match` in `handle_connection`. Bind, accept and the lock: `run_local` and `lock_daemon`. Boot order and
buses: `run`. Write deadlines: `write_deadline`.

Records: ADR 0001, 0027, 0035, 0045, 0050.
