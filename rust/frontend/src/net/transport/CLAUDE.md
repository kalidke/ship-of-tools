# rust/frontend/src/net/transport: the per-host control connection (fe-net)

One task per dialled host: connect, hello, ping, run the request and event loop, reconnect. The UI side sees only
`OutgoingReq` and `IncomingEvt`. Part of fe-net; charter: rust/frontend/src/net/CLAUDE.md.

## Files
- `mod.rs`: the per-host connection task (`spawn`, `connect_and_run`, `run_protocol`, `run_session`) and every part of the transport no other file here holds

## Start here
`spawn`, then `connect_and_run`, `run_protocol`, `run_session`.

## Rules
- Read frames only through the one held read future (`read_owned`); `codec::read_frame` is not cancel-safe.
- A request whose loss must be reported inserts its `PendingKind` before its write (`send_figure_get`).
- The link gate goes up at any hello reply and down when the session ends, except after a hello refusal
  (`run_protocol`).
- Every event is tagged with the dial `HostKey`; the daemon's declared host is display only.
