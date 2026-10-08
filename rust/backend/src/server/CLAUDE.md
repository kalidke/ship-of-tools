# rust/backend/src/server: the daemon's one listener (charter)

## Idea
One listener per daemon. A connection is admitted twice, each in one place: by the OS peer read at accept
(`admit_peer`), and by its first frame, which must be a hello the daemon accepts (`admit_hello`: the protocol, the
declared host and OS account, the host's account record). Until then nothing is served. The hello's role says what the
connection becomes: `handoff` leaves for a byte pipe or a lease after its reply (`hand_off`: `proxy.connect`
(`handle_proxy_connect`), `lane.connect` (`handle_lane_connect`) or `fe.lease` (the lease code's `hold`)); every other
role is a control session at once, and its only writer is its own loop (`serve_control`). Part of the daemon's server
subsystem; the sotd entry point and the roster beside this folder serve the same idea.

## Owns
- `<state>/daemon.lock`: taken by `take_daemon_lock` (which calls `lock_daemon`) at the top of `run`, held for the process's life.
- The session socket or pipe: `run_local` secures its private directory, refuses a socket a live daemon answers on
  (`refuse_live_socket`), binds it (`bind_session`: on Windows the owner-only pipe descriptor and an inbound buffer that
  holds a client's hello and its request, each at most the envelope cap), accepts, and unlinks at shutdown.
- The eight buses `run` creates: repl frames, preview changes, workspace events, topology writes, agent messages, agent
  receipts, frontend commands and the monitor tick. Their payloads belong to their owners.
- Each control session's task and state, in `serve_control`: roster guard (control sessions only), declared host and
  name, active workspace, monitor flag, read deadline, and the off-loop `JoinSet` and semaphore.
- The op table: the `match` in `dispatch`, which hands each op to its owner's handler (four through the off-loop
  pool) and keeps inline only the arms that touch connection state or must keep request order (`preview.set_scale`),
  and the unknown-op reply.
- sotd's entry (argv, umask 077, boot refusals, the log tee to `<state>/sotd.log`, which follows `XDG_STATE_HOME` and
  has no size bound or rotation), the client roster (`clients.rs`) and the revision ring (`session.rs`), all in the
  crate root.

## Promises
- One daemon per state root: `lock_daemon` fails at once when a daemon answers on the socket, and waits up to
  `daemon_lock_wait` for a predecessor that is still shutting down.
- A socket a live daemon answers on is never unlinked: `refuse_live_socket`, called from `run_local`.
- At accept, a Unix connection is dropped before a byte is read if its recorded effective uid is foreign or the peer
  read fails (`admit_peer`, called from `run_local`; Linux and macOS compare with `same_account`). On macOS this
  authenticates cached connection provenance and returns a live token process observation; it establishes neither the
  holder's current euid nor a binding to that process. Credential transitions and descriptor transfers leave the cached
  account unchanged. Windows uses the pipe's owner-only descriptor.
- A connection whose first frame is not a hello that parses and passes `admit_hello` gets one reply (`unauthenticated`,
  `protocol_mismatch`, `identity_missing` or `os_user_conflict`) and is closed; nothing else is served or sent to it
  (`handle_connection`). A first frame that declares a blob is refused `unauthenticated` before a byte of its blob is
  read (`read_envelope`, `parse_first_frame`). A host that has said hello as two OS accounts is refused for either
  until the daemon restarts (`Clients::admit_account`); connections already open are left alone.
- Only `admit_hello` makes an `Admitted`, and `serve_control`, `hand_off` and `register_hello` take one, so no path from accept serves a connection whose hello was not admitted.
- A second hello on a control connection closes it unanswered; a `handoff` connection's next frame is
  `proxy.connect`, `lane.connect` or `fe.lease`, else `bad_request` and a close (`hand_off`).
- A connection whose first frame has not arrived within 10 s, or a `handoff` connection whose next frame has not, is closed
  with nothing sent (`ADMISSION_READ_BOUND`, `handle_connection`, `hand_off`).
- A connection has one writer. Off-loop jobs hand their reply back over `OutTx` and the loop writes it with
  `write_reply`.
- A handler `Err` is one `handler_error` frame and the connection stays (`finish_dispatch`); an over-cap envelope
  degrades to an error frame for that request (`write_reply`); only a write failure ends the connection, or a file
  download's read error once its chunks are on the wire (`stream_file_download`).
- A peer that cannot drain a frame within 10 s plus 1 s per MiB of blob is dropped (`write_frame_to`,
  `write_deadline`, ADR 0027). No end of a control session
  owes its peer bytes, nor does a lease whose reply or notice could not be written, nor a `handoff` connection whose
  next frame failed or did not come: each is closed without waiting for its peer to read (`Abandon`), so on Windows
  no such peer holds the closes behind it in interprocess's one linger thread. A refusal, a `bad_request`, a lease's
  last answer and a byte pipe's tail still wait until their peer has read them.
- On Windows a client may write its hello and its request, each one envelope at most the cap, before it reads: the
  session pipe's inbound buffer holds both (`bind_session`, `PIPE_INBOUND_BYTES`), so a refusal, which the pipe holds
  open until the client has read it, never waits on a client still blocked in its own write. The cost: a client of this
  account may leave up to 2 MiB of nonpaged pool waiting per connection until the daemon reads it or closes the pipe.
- A pinged `fe` or `bridge` connection silent for 90 s is reaped (the reaper arm in `select_once`,
  `ping_read_deadline`).
- At most 4 off-loop jobs run per connection, and one queued 10 s is answered with a timeout and never runs
  (`spawn_job`); `pty.*` and `monitor.*` run inline and never wait on that pool.
- Accepting stops at the deciding lease departure, before any row is touched (`run_local`'s accept loop, then
  `shutdown::run`).

## Connections
Each connection is one row of docs/integration.md, owned by its provider. Provides: handle_fe_command_send relay diagnostics, `proxy.connect`,
`handle_connection`, `handle_proxy_connect`, `pipe_bidirectional`, `reject`, `lane.connect`, `handle_lane_connect`,
`fe.lease`, `lease::hold`, `admit_peer`, `dispatch`, `hello`, `admit_hello`, `sotd stdio-bridge`, `write_frame_within`,
`write_frame_to`, `version.query`. Uses: `Frame`, `codec::read_frame`, `codec::read_envelope`, `codec::write_frame`, `hello`,
`PROTOCOL_VERSION`, `rust/protocol/src/ops/mod.rs`, `rust/protocol/src/ops/`, `version_line`, `--version`,
`TopologyStore`, `topology.set`, `topology.changed`, `startup::begin`, `lease::ticker`, `Leases::gone`,
`shutdown::run`, `Workspaces::resolve`, `row_or_reply`, `capsule_guard`, `seed_default_row`, `set_repl_frame_tx`,
`set_watch_bus`, `set_monitor_hub`, `sot_state_dir`, `sot_config_dir`, `host_name`, `state_dir_hash`,
`publish_noreplace`, `lock_writer`, `try_lock_daemon`, `preflight_volume`, `owner_protected_pipe_descriptor`,
`harden_own_stdio`, `boot_identity`, `process_created`, `IdentityExchange`, `start_page_servers`, `remove_root`,
`handle_agent_join`.

## Folders
The crate root holds the rest of this subsystem: `main.rs` (sotd's entry), `clients.rs` (the roster) and `session.rs`
(the revision ring).

## Files
- `mod.rs`: the entry: `run` boots the buses and the roster
- `hello.rs`: the hello: the admission (`parse_first_frame`, `admit_hello`, and `Admitted`, the only proof of it), the reply with its replay (`handle_hello`) and the roster entry (`register_hello`)
- `listen.rs`: the daemon lock (`take_daemon_lock`, `lock_daemon`), the live-socket refusal, the listener (`bind_session`, with the pipe descriptor and its inbound buffer), the accept loop (`run_local`) and the accept-time admission (`admit_peer`, `same_account`)
- `conn.rs`: one connection: the read-deadline reaper, its admission at the first frame (`handle_connection`), the handoff to a pipe or a lease (`hand_off`), the control loop (`serve_control`) and its select (`select_once`)
- `dispatch.rs`: the op table: `dispatch` routes one request to its owner and writes the reply
- `events.rs`: one `write_*` per bus turning a broadcast item into its evt frame, and `recv_or_pending` for the two buses a connection holds as `Option` (always `Some` in a served connection, `None` only in tests)
- `reply.rs`: the write deadline, the frame writers, the reply and error containment, and the off-loop job pool
- `pipe.rs`: the shared byte pipe after a connect frame (`pipe_bidirectional`) and the one error frame for a refused connect (`reject`)

## Start here
An op: the `match` in `dispatch`. Bind, accept and the lock: `run_local` and `lock_daemon`. Boot order and
buses: `run`. Write deadlines and the job pool: `reply.rs`.

Records: ADR 0001, 0027, 0035, 0045, 0050.
