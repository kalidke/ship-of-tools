# rust/frontend/src/net/transport: the per-host control connection (fe-net)

One task per dialled host: connect, hello, ping, run the request and event loop, reconnect. The UI side sees only
`OutgoingReq` and `IncomingEvt`. Part of fe-net; charter: rust/frontend/src/net/CLAUDE.md.

## Files
- `event.rs`: `IncomingEvt` and ResultTreeReply; result-tree successes and failures retain the issuing attempt under the connection's dial HostKey.
- `request.rs`: OutgoingReq, ResultAttemptId and ResultTreeRequest; send_request encodes one request for the steady writer using the existing op-family serializers, ordinary or locally tagged result-tree.
- `mod.rs`: transport declarations and per-host connect, hello, reconnect and stderr drain (`spawn`, `connect_and_run`, `run_protocol`, `run_session`, `spawn_stderr_drain`). It re-exports the local result-tree vocabulary and provides a cfg(test) ResultTreeTestDriver that delegates injected writes and replies to the real sender, PendingGuard and response dispatcher.
  The test-window-progress target also enters the existing steady_loop through run_native_progress_transport, a cfg(test), crate-visible wrapper with injected I/O, event/request channels and the native window; its bookkeeping remains private to transport.
- `reply.rs`: PendingKind, PendingGuard and handle_response_frame; the opt-in native progress fixture observes the actual successful fan-in send through a test-only scoped observer, installed through the gated observe_native_fan_in registration re-export in mod.rs. Result-tree pending entries retain the issuing attempt and request step; matching replies and connection-loss failures emit that tag once. Inline tests execute the sender, pending dispatcher and decoder with reordered responses.
- `steady.rs`: steady_loop and its held read/write futures, encoded-request correlation and orderly outgoing-close drain.
- `steady_tests.rs`: the steady loop over small in-memory streams: upload and download progress, blocked pings, fairness, partial frames, early replies, failures, close and cancellation.
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
`spawn`, then `connect_and_run`, `run_protocol`, `run_session` (the hello and the preamble), `steady.rs` `steady_loop` (everything
after). A new request or reply goes through `ops/`.

## Rules
- Steady reads and writes each use one held future; a partial frame is never cancelled and restarted. Outgoing close finishes the current write and retains the current read before draining.
- A request is encoded in memory and its pending entries enter PendingGuard before its first network byte; at most one encoded write is active and reads remain polled during it.
- The hello and the preamble read plainly, before any request can race them.
- send_figure_get and send_result_tree register their pending entry before writing; PendingGuard reports their outstanding failures when the connection ends.
- The link gate goes up at any hello reply (`read_hello`) and down when the session ends, except after a hello refusal
  (`run_protocol`).
- The local socket or pipe is dialled only through `connect_pipe`, which applies `sot_log::identity::connect_own`'s rule: `own_socket` before the connect on Unix; on Windows `connect_own` itself, which opens the pipe at identification level and checks the serving process before any byte is written, bounded by `CONNECT_BOUND`, run on a blocking thread, its handle adopted as the stream.
- Every event is tagged with the dial `HostKey`; the daemon's declared host is display only.
- ResultTree carries frontend-only canonical workspace, result and attempt identities through the existing request-id pending map; its wire payload remains tree.root or tree.children. Every matched success, backend error, malformed reply or pending connection loss returns the saved tag, never the current view's attempt.
- A Relay dial uses connect_pipe's account rule and budget but never Leases::before_data_connection; ResolvedDial::Relay retains its exact path.
- The native progress fixture counts accepted events at the actual successful emit send; its observer is compiled out of ordinary builds and adds no queue, forwarding task or event handling.
- run_native_progress_transport and the native fan-in observer registration exist only under cfg(all(test, feature = "test-window-progress")); the wrapper calls the same steady_loop as run_session with injected I/O and private bookkeeping, without connecting, loading reconnect memory or changing scheduling.
