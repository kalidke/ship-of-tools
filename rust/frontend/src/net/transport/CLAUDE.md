# rust/frontend/src/net/transport: the per-host control connection (fe-net)

One task per dialled host: connect, hello, ping, run the request and event loop, reconnect. The UI side sees only
`OutgoingReq` and `IncomingEvt`. Part of fe-net; charter: rust/frontend/src/net/CLAUDE.md.

## Files
- `event.rs`: `IncomingEvt`, every event a connection hands the UI thread
- `request.rs`: `OutgoingReq`, every request the UI can send a host, and `send_request`, which writes one
- `mod.rs`: declares the parts and names the transport's interface to the window; holds the per-host connection task
  (`spawn`, `connect_and_run`, `run_protocol`, `run_session`, `steady_loop`, `spawn_stderr_drain`) and the rest no
  other file here holds
- `reply.rs`: reply matching: the pending entry per request id (`PendingKind`), `PendingGuard`, and
  `handle_response_frame`, which turns each reply into an `IncomingEvt`
- `tests.rs`: the connection task's tests: backoff, the link gate, a tree.root error reply, a closed local connection,
  the stderr drain
- `golden_tests.rs`: every request kind's wire line and the events its error reply yields, against the golden file
- `testdata/`: golden files for golden_tests.rs
- `ops/`: one file per op family: the request each op writes, and the event its reply becomes
- `hello.rs`: the hello: its 30 s reply bound (`HELLO_TIMEOUT`, `read_hello_reply`), the refusal type `HelloRefused`,
  the protocol-mismatch message, and the three steps of the hello (`send_hello`, `read_hello`, which puts any refused
  hello on the blocking screen as `IncomingEvt::HelloRefused`, and `accept_hello`)
- `preamble.rs`: the connect preamble after the hello (tree.root, then preview.get of its root)

## Start here
`spawn`, then `connect_and_run`, `run_protocol`, `run_session` (the hello and the preamble), `steady_loop` (everything
after). A new request or reply goes through `ops/`.

## Rules
- In `steady_loop`, read frames only through the one held read future (`read_owned`); `codec::read_frame` is not
  cancel-safe. The hello and the preamble read plainly, before any request can race them, and so does the drain after
  the outgoing channel closes.
- A request whose loss must be reported inserts its `PendingKind` before its write (`send_figure_get`).
- The link gate goes up at any hello reply (`read_hello`) and down when the session ends, except after a hello refusal
  (`run_protocol`).
- The local socket or pipe is dialled only through `connect_pipe`, which applies `sot_log::identity::connect_own`'s rule: `own_socket` before the connect on Unix; on Windows `connect_own` itself, which opens the pipe at identification level and checks the serving process before any byte is written, bounded by `CONNECT_BOUND`, run on a blocking thread, its handle adopted as the stream.
- Every event is tagged with the dial `HostKey`; the daemon's declared host is display only.
